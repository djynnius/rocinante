// Rocinante's bridge to llama.cpp.
//
// One call runs one chat turn on a persistent context, using llama.cpp's own
// machinery — the code llama-server runs — rather than a re-implementation:
//   * jinja chat templating from the GGUF (`common_chat_templates_apply`),
//     tools included;
//   * the per-model tool-call / reasoning parser (`common_chat_parse`), run
//     partially while streaming and once more at the end;
//   * common sampling, including the lazy tool-call grammar and its triggers.
// Between turns the KV cache keeps the longest prompt prefix it can reuse, so
// an agent loop that resends the conversation only decodes what is new.
//
// Everything crossing the boundary is plain C: JSON in, JSON out, and a
// callback for streamed text. Every entry point catches C++ exceptions; none
// may unwind into Rust.

#include "chat.h"
#include "common.h"
#include "llama.h"
#include "log.h"
#include "sampling.h"

#include <algorithm>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <memory>
#include <set>
#include <string>
#include <vector>

namespace {

using rcn_log_fn = void (*)(int32_t level, const char * text);

rcn_log_fn g_log = nullptr;

void log_trampoline(ggml_log_level level, const char * text, void * /*user*/) {
    if (g_log != nullptr && text != nullptr) {
        g_log(static_cast<int32_t>(level), text);
    }
}

char * dup_string(const std::string & s) {
    char * out = static_cast<char *>(std::malloc(s.size() + 1));
    if (out == nullptr) {
        return nullptr;
    }
    std::memcpy(out, s.data(), s.size());
    out[s.size()] = '\0';
    return out;
}

std::string error_json(const std::string & message) {
    common_json j = common_json::object();
    j["error"]    = message;
    return j.dump_safe();
}

void set_err(char ** err, const std::string & message) {
    if (err != nullptr) {
        *err = dup_string(message);
    }
}

// Fill `batch` with tokens[from, to) at positions from..to, sequence 0; only
// the very last prompt token asks for logits.
void fill_batch(llama_batch & batch, const std::vector<llama_token> & tokens, size_t from, size_t to,
                bool logits_on_last) {
    batch.n_tokens = 0;
    for (size_t i = from; i < to; ++i) {
        const int32_t k    = batch.n_tokens;
        batch.token[k]     = tokens[i];
        batch.pos[k]       = static_cast<llama_pos>(i);
        batch.n_seq_id[k]  = 1;
        batch.seq_id[k][0] = 0;
        batch.logits[k]    = logits_on_last && i + 1 == to;
        batch.n_tokens++;
    }
}

struct batch_guard {
    llama_batch batch;

    explicit batch_guard(int32_t n) : batch(llama_batch_init(n, 0, 1)) {}

    ~batch_guard() { llama_batch_free(batch); }

    batch_guard(const batch_guard &)             = delete;
    batch_guard & operator=(const batch_guard &) = delete;
};

}  // namespace

extern "C" {

struct rcn_model {
    llama_model *             model = nullptr;
    common_chat_templates_ptr tmpls;
};

struct rcn_session {
    rcn_model *              m   = nullptr;
    llama_context *          ctx = nullptr;
    // Tokens whose K/V currently sit in sequence 0, in position order.
    std::vector<llama_token> cached;
    // Recurrent, hybrid and sliding-window models cannot drop the tail of
    // their memory, so the usual "keep the common prefix" fails as soon as
    // a new prompt diverges from what was generated. For them we keep a few
    // snapshots of the partial state (recurrent cells / SWA window) at
    // message boundaries the next request's prompt is likely to share, and
    // restore the latest one inside the shared prefix instead of starting
    // over (llama-server's context checkpoints, simplified).
    bool                     needs_snapshot = false;
    // Snapshots by ascending token count: after the system prompt (the big
    // stable part), at the last user message, and at the end of the
    // conversation.
    std::vector<std::pair<size_t, std::vector<uint8_t>>> snaps;
};

struct rcn_turn_params {
    const char * messages_json;  // OpenAI-style messages array
    const char * tools_json;     // OpenAI-style tools array ("[]" for none)
    bool         enable_thinking;
    float        temperature;
    float        top_p;
    int32_t      top_k;
    float        min_p;
    int32_t      max_tokens;  // <= 0: until the context is full
    uint32_t     seed;
};

// kind: 0 = answer text, 1 = reasoning text, 2 = prompt progress (text is
// "done/total"). Return false to cancel the turn.
typedef bool (*rcn_emit_fn)(void * user, int32_t kind, const char * text, size_t len);

void rcn_init(rcn_log_fn log) {
    g_log = log;
    llama_log_set(log_trampoline, nullptr);
    // llama.cpp's `common` logger prints straight to the terminal, which
    // would scribble over the TUI: silence it.
    common_log_set_verbosity_thold(-1);
    common_log_pause(common_log_main());
    llama_backend_init();
}

int32_t rcn_default_threads(void) {
    return common_cpu_get_num_math();
}

bool rcn_gpu_offload_supported(void) {
    return llama_supports_gpu_offload();
}

void rcn_string_free(char * s) {
    std::free(s);
}

rcn_model * rcn_model_load(const char * path, int32_t n_gpu_layers, char ** err) {
    try {
        llama_model_params mp = llama_model_default_params();
        mp.n_gpu_layers       = n_gpu_layers;
        llama_model * model   = llama_model_load_from_file(path, mp);
        if (model == nullptr) {
            set_err(err, std::string("llama.cpp could not load ") + path +
                             " (not a GGUF file, an unsupported architecture, or not enough memory)");
            return nullptr;
        }
        auto * m  = new rcn_model;
        m->model  = model;
        try {
            m->tmpls = common_chat_templates_init(model, "");
        } catch (const std::exception & e) {
            llama_model_free(model);
            delete m;
            set_err(err, std::string("the model's chat template could not be parsed: ") + e.what());
            return nullptr;
        }
        return m;
    } catch (const std::exception & e) {
        set_err(err, std::string("loading the model failed: ") + e.what());
        return nullptr;
    } catch (...) {
        set_err(err, "loading the model failed");
        return nullptr;
    }
}

void rcn_model_free(rcn_model * m) {
    if (m == nullptr) {
        return;
    }
    m->tmpls.reset();
    llama_model_free(m->model);
    delete m;
}

int32_t rcn_model_n_ctx_train(const rcn_model * m) {
    return llama_model_n_ctx_train(m->model);
}

char * rcn_model_desc(const rcn_model * m) {
    char buf[256] = { 0 };
    llama_model_desc(m->model, buf, sizeof(buf));
    return dup_string(buf);
}

rcn_session * rcn_session_new(rcn_model * m, uint32_t n_ctx, int32_t n_threads, char ** err) {
    try {
        llama_context_params cp = llama_context_default_params();
        cp.n_ctx                = n_ctx;
        cp.n_batch              = 512;  // also the progress / cancel granularity
        cp.n_ubatch             = 512;
        cp.n_threads            = n_threads;
        cp.n_threads_batch      = n_threads;
        cp.no_perf              = true;
        llama_context * ctx     = llama_init_from_model(m->model, cp);
        if (ctx == nullptr) {
            set_err(err, "llama.cpp could not create a context of " + std::to_string(n_ctx) +
                             " tokens (not enough memory? lower num_ctx)");
            return nullptr;
        }
        auto * s           = new rcn_session;
        s->m               = m;
        s->ctx             = ctx;
        s->needs_snapshot  = llama_model_is_recurrent(m->model) || llama_model_is_hybrid(m->model) ||
                            llama_model_n_swa(m->model) > 0;
        return s;
    } catch (const std::exception & e) {
        set_err(err, std::string("creating the context failed: ") + e.what());
        return nullptr;
    } catch (...) {
        set_err(err, "creating the context failed");
        return nullptr;
    }
}

void rcn_session_free(rcn_session * s) {
    if (s == nullptr) {
        return;
    }
    llama_free(s->ctx);
    delete s;
}

uint32_t rcn_session_n_ctx(const rcn_session * s) {
    return llama_n_ctx(s->ctx);
}

// Run one chat turn. Returns a JSON object (free with rcn_string_free):
//   {"content", "reasoning", "tool_calls": [{"name", "arguments", "id"}],
//    "stop": "eos" | "stop" | "length" | "cancelled",
//    "prompt_tokens", "cached_tokens", "completion_tokens"}
// or {"error": "..."}.
char * rcn_session_chat(rcn_session * s, const rcn_turn_params * p, rcn_emit_fn emit, void * user) {
    try {
        llama_model *       model = s->m->model;
        const llama_vocab * vocab = llama_model_get_vocab(model);
        llama_memory_t      mem   = llama_get_memory(s->ctx);

        // ---- template the conversation (the llama-server path) ----
        common_chat_templates_inputs inputs;
        inputs.messages = common_chat_msgs_parse_oaicompat(common_json::parse(p->messages_json));
        inputs.tools    = common_chat_tools_parse_oaicompat(
            common_json::parse(p->tools_json != nullptr ? p->tools_json : "[]"));
        inputs.tool_choice           = COMMON_CHAT_TOOL_CHOICE_AUTO;
        auto caps                    = common_chat_templates_get_caps(s->m->tmpls.get());
        inputs.parallel_tool_calls   = caps["supports_parallel_tool_calls"];
        inputs.use_jinja             = true;
        inputs.add_generation_prompt = true;
        inputs.reasoning_format      = COMMON_REASONING_FORMAT_DEEPSEEK;
        inputs.enable_thinking       = p->enable_thinking;
        const common_chat_params chat = common_chat_templates_apply(s->m->tmpls.get(), inputs);

        common_chat_parser_params parse_params(chat);
        parse_params.reasoning_format     = COMMON_REASONING_FORMAT_DEEPSEEK;
        parse_params.reasoning_in_content = false;
        parse_params.parse_tool_calls     = !inputs.tools.empty();
        if (!chat.parser.empty()) {
            parse_params.parser.load(chat.parser);
        }

        // ---- sampling: user knobs plus the template's tool-call grammar ----
        common_params_sampling sp;
        sp.seed              = p->seed;
        sp.temp              = p->temperature;
        sp.top_p             = p->top_p;
        sp.top_k             = p->top_k;
        sp.min_p             = p->min_p;
        sp.generation_prompt = chat.generation_prompt;
        for (const auto & t : chat.preserved_tokens) {
            auto ids = common_tokenize(vocab, t, false, true);
            if (ids.size() == 1) {
                sp.preserved_tokens.insert(ids[0]);
            }
        }
        if (!chat.grammar.empty()) {
            sp.grammar      = common_grammar(COMMON_GRAMMAR_TYPE_TOOL_CALLS, chat.grammar);
            sp.grammar_lazy = chat.grammar_lazy;
            for (const auto & trigger : chat.grammar_triggers) {
                if (trigger.type == COMMON_GRAMMAR_TRIGGER_TYPE_WORD) {
                    auto ids = common_tokenize(vocab, trigger.value, false, true);
                    if (ids.size() == 1 && sp.preserved_tokens.count(ids[0]) > 0) {
                        common_grammar_trigger t;
                        t.type  = COMMON_GRAMMAR_TRIGGER_TYPE_TOKEN;
                        t.value = trigger.value;
                        t.token = ids[0];
                        sp.grammar_triggers.push_back(t);
                        continue;
                    }
                }
                sp.grammar_triggers.push_back(trigger);
            }
            if (sp.grammar_lazy && sp.grammar_triggers.empty()) {
                // A lazy grammar nothing can trigger: generate unconstrained
                // and let the parser (and Rocinante's repair) cope.
                sp.grammar      = common_grammar();
                sp.grammar_lazy = false;
            }
        }
        common_sampler_ptr smpl(common_sampler_init(model, sp));
        if (!smpl) {
            return dup_string(error_json("llama.cpp rejected the sampling setup (tool-call grammar)"));
        }

        // ---- prompt: reuse the cached prefix, decode the rest ----
        const std::vector<llama_token> prompt = common_tokenize(vocab, chat.prompt, true, true);
        const int32_t                  n_ctx  = static_cast<int32_t>(llama_n_ctx(s->ctx));
        if (prompt.empty()) {
            return dup_string(error_json("the templated prompt is empty"));
        }
        if (static_cast<int32_t>(prompt.size()) >= n_ctx - 8) {
            return dup_string(error_json("the prompt is " + std::to_string(prompt.size()) +
                                         " tokens but the context holds " + std::to_string(n_ctx) +
                                         " — raise num_ctx or compact the conversation"));
        }

        size_t n_keep = 0;
        while (n_keep < s->cached.size() && n_keep < prompt.size() && s->cached[n_keep] == prompt[n_keep]) {
            n_keep++;
        }
        if (n_keep == prompt.size()) {
            n_keep--;  // the last prompt token is re-decoded for fresh logits
        }
        if (!llama_memory_seq_rm(mem, 0, static_cast<llama_pos>(n_keep), -1)) {
            // The memory cannot drop that tail (recurrent / SWA): restore the
            // latest snapshot inside the shared prefix, else start over.
            bool restored = false;
            for (auto it = s->snaps.rbegin(); it != s->snaps.rend() && !restored; ++it) {
                if (it->first == 0 || it->first > n_keep) {
                    continue;
                }
                if (llama_state_seq_set_data_ext(s->ctx, it->second.data(), it->second.size(), 0,
                                                 LLAMA_STATE_SEQ_FLAGS_PARTIAL_ONLY) > 0 &&
                    llama_memory_seq_rm(mem, 0, static_cast<llama_pos>(it->first), -1)) {
                    n_keep   = it->first;
                    restored = true;
                }
            }
            if (!restored) {
                llama_memory_clear(mem, true);
                n_keep = 0;
            }
        }
        s->cached.resize(n_keep);
        // Snapshots past the kept prefix describe tokens that are gone.
        while (!s->snaps.empty() && s->snaps.back().first > n_keep) {
            s->snaps.pop_back();
        }
        const size_t cached_tokens = n_keep;

        // Where to snapshot (recurrent / SWA models only): the first user
        // message (everything before it — system prompt, tools — is stable),
        // the last user message, and the conversation without the reply
        // prompt, i.e. what the next request most likely starts with.
        std::vector<size_t> snap_points;
        if (s->needs_snapshot) {
            try {
                common_chat_msg_delimiters delims = chat.message_delimiters;
                delims.tokenize(vocab);
                for (const auto & span : delims.split(prompt).spans) {
                    if (span.role == COMMON_CHAT_ROLE_USER) {
                        snap_points.push_back(span.pos);
                    }
                }
                if (snap_points.size() > 2) {
                    snap_points.erase(snap_points.begin() + 1, snap_points.end() - 1);
                }
            } catch (...) {
                snap_points.clear();
            }
            const std::string & gen = chat.generation_prompt;
            if (!gen.empty() && chat.prompt.size() > gen.size() &&
                chat.prompt.compare(chat.prompt.size() - gen.size(), std::string::npos, gen) == 0) {
                const auto head =
                    common_tokenize(vocab, chat.prompt.substr(0, chat.prompt.size() - gen.size()), true, true);
                if (head.size() < prompt.size() && std::equal(head.begin(), head.end(), prompt.begin())) {
                    snap_points.push_back(head.size());
                }
            }
            std::sort(snap_points.begin(), snap_points.end());
            snap_points.erase(std::unique(snap_points.begin(), snap_points.end()), snap_points.end());
            snap_points.erase(std::remove_if(snap_points.begin(), snap_points.end(),
                                             [&](size_t at) { return at <= n_keep || at >= prompt.size(); }),
                              snap_points.end());
        }

        const int32_t n_batch = static_cast<int32_t>(llama_n_batch(s->ctx));
        batch_guard   guard(std::max(n_batch, 1));
        llama_batch & batch = guard.batch;

        auto cancelled_json = [&]() {
            common_json out          = common_json::object();
            out["stop"]              = "cancelled";
            out["content"]           = "";
            out["reasoning"]         = "";
            out["tool_calls"]        = common_json::array();
            out["prompt_tokens"]     = static_cast<int64_t>(prompt.size());
            out["cached_tokens"]     = static_cast<int64_t>(cached_tokens);
            out["completion_tokens"] = static_cast<int64_t>(0);
            return dup_string(out.dump_safe());
        };
        // Decode prompt[from, to); logits only for the prompt's last token.
        // 0 = ok, 1 = decode failed, 2 = cancelled.
        auto decode_range = [&](size_t from, size_t to) -> int {
            for (size_t i = from; i < to; i += static_cast<size_t>(n_batch)) {
                const size_t end = std::min(to, i + static_cast<size_t>(n_batch));
                fill_batch(batch, prompt, i, end, end == prompt.size());
                if (llama_decode(s->ctx, batch) != 0) {
                    return 1;
                }
                s->cached.insert(s->cached.end(), prompt.begin() + static_cast<std::ptrdiff_t>(i),
                                 prompt.begin() + static_cast<std::ptrdiff_t>(end));
                const std::string progress =
                    std::to_string(end - cached_tokens) + "/" + std::to_string(prompt.size() - cached_tokens);
                if (!emit(user, 2, progress.data(), progress.size())) {
                    return 2;
                }
            }
            return 0;
        };
        auto failed = [&]() {
            llama_memory_clear(mem, true);
            s->cached.clear();
            s->snaps.clear();
            return dup_string(error_json("llama.cpp failed to process the prompt"));
        };

        {
            const std::string progress = "0/" + std::to_string(prompt.size() - cached_tokens);
            if (!emit(user, 2, progress.data(), progress.size())) {
                return cancelled_json();
            }
        }
        size_t from = n_keep;
        for (const size_t at : snap_points) {
            const int rc = decode_range(from, at);
            if (rc == 1) {
                return failed();
            }
            if (rc == 2) {
                return cancelled_json();
            }
            from               = at;
            const size_t size  = llama_state_seq_get_size_ext(s->ctx, 0, LLAMA_STATE_SEQ_FLAGS_PARTIAL_ONLY);
            std::vector<uint8_t> data(size);
            if (size > 0 && llama_state_seq_get_data_ext(s->ctx, data.data(), size, 0,
                                                         LLAMA_STATE_SEQ_FLAGS_PARTIAL_ONLY) == size) {
                s->snaps.emplace_back(at, std::move(data));
            }
        }
        // Keep the earliest snapshot (system prompt) and the latest three.
        while (s->snaps.size() > 4) {
            s->snaps.erase(s->snaps.begin() + 1);
        }
        {
            const int rc = decode_range(from, prompt.size());
            if (rc == 1) {
                return failed();
            }
            if (rc == 2) {
                return cancelled_json();
            }
        }

        // ---- generate ----
        std::string     generated;
        common_chat_msg streamed;  // what has been emitted so far
        streamed.role         = "assistant";
        std::string stop      = "eos";
        int32_t     i_logits  = batch.n_tokens - 1;
        int64_t     n_gen     = 0;
        const int64_t max_gen = p->max_tokens > 0 ? p->max_tokens : n_ctx;
        llama_pos   pos       = static_cast<llama_pos>(prompt.size());
        bool        cancelled = false;

        auto emit_diffs = [&](const common_chat_msg & next) {
            for (const auto & d : common_chat_msg_diff::compute_diffs(streamed, next)) {
                if (!d.reasoning_content_delta.empty() &&
                    !emit(user, 1, d.reasoning_content_delta.data(), d.reasoning_content_delta.size())) {
                    cancelled = true;
                }
                if (!d.content_delta.empty() &&
                    !emit(user, 0, d.content_delta.data(), d.content_delta.size())) {
                    cancelled = true;
                }
            }
            streamed = next;
        };

        while (true) {
            if (pos >= n_ctx || n_gen >= max_gen) {
                stop = "length";
                break;
            }
            const llama_token tok = common_sampler_sample(smpl.get(), s->ctx, i_logits);
            common_sampler_accept(smpl.get(), tok, true);
            if (llama_vocab_is_eog(vocab, tok)) {
                stop = "eos";
                break;
            }
            const bool special = sp.preserved_tokens.count(tok) > 0;
            generated += common_token_to_piece(s->ctx, tok, special);
            n_gen++;

            bool hit_stop = false;
            for (const auto & word : chat.additional_stops) {
                if (word.empty()) {
                    continue;
                }
                const size_t from = generated.size() > word.size() + 64 ? generated.size() - word.size() - 64 : 0;
                const size_t at   = generated.find(word, from);
                if (at != std::string::npos) {
                    generated.resize(at);
                    hit_stop = true;
                    break;
                }
            }

            // Stream what the partial parse can already tell: answer text
            // and reasoning, never raw tool-call markup.
            try {
                emit_diffs(common_chat_parse(generated, true, parse_params));
            } catch (...) {
                // A partial parse can trip over half a construct; the next
                // token (or the final parse) catches up.
            }
            if (cancelled) {
                stop = "cancelled";
                break;
            }
            if (hit_stop) {
                stop = "stop";
                break;
            }

            fill_batch(batch, std::vector<llama_token>{ tok }, 0, 1, true);
            batch.pos[0] = pos;
            if (llama_decode(s->ctx, batch) != 0) {
                llama_memory_clear(mem, true);
                s->cached.clear();
                return dup_string(error_json("llama.cpp failed while generating"));
            }
            s->cached.push_back(tok);
            pos++;
            i_logits = 0;
        }

        // ---- final parse: tool calls, plus any text the partial parse held back ----
        common_chat_msg final_msg;
        bool            parsed = false;
        try {
            final_msg = common_chat_parse(generated, stop == "length" || stop == "cancelled", parse_params);
            parsed    = true;
        } catch (...) {
            parsed = false;
        }
        if (!parsed) {
            // Malformed tool-call markup: hand back the raw text so the
            // harness's repair pass can still find the call.
            final_msg            = common_chat_msg();
            final_msg.role       = "assistant";
            final_msg.content    = generated;
            final_msg.reasoning_content = streamed.reasoning_content;
        }
        if (!cancelled) {
            try {
                emit_diffs(final_msg);
            } catch (...) {
                // The final text diverged from what was streamed (a partial
                // stop word); the JSON below carries the authoritative text.
            }
        }

        common_json calls = common_json::array();
        for (const auto & tc : final_msg.tool_calls) {
            common_json c  = common_json::object();
            c["name"]      = tc.name;
            c["arguments"] = tc.arguments;
            c["id"]        = tc.id;
            calls.push_back(c);
        }
        common_json out          = common_json::object();
        out["content"]           = final_msg.content;
        out["reasoning"]         = final_msg.reasoning_content;
        out["tool_calls"]        = calls;
        out["stop"]              = stop;
        out["prompt_tokens"]     = static_cast<int64_t>(prompt.size());
        out["cached_tokens"]     = static_cast<int64_t>(cached_tokens);
        out["completion_tokens"] = n_gen;
        return dup_string(out.dump_safe());
    } catch (const std::exception & e) {
        return dup_string(error_json(std::string("llama.cpp: ") + e.what()));
    } catch (...) {
        return dup_string(error_json("llama.cpp: unknown error"));
    }
}

}  // extern "C"
