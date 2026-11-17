// A small C surface over whisper.cpp for Rocinante's `/talk`: load a model,
// transcribe one utterance, and run the Silero voice-activity model on a
// stream of audio. whisper.cpp is compiled against the same ggml that
// llama.cpp uses (see build.rs), so this links into the binary next to
// rcn_llama.cpp without a second ggml.
//
// Every entry point is called from one Rust thread at a time per context;
// none of them throw across the C boundary.

#include "whisper.h"

#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <string>

extern "C" {

typedef void (*rcn_log_fn)(int32_t level, const char * text);

static rcn_log_fn g_whisper_log = nullptr;

static void whisper_log_trampoline(enum ggml_log_level level, const char * text, void *) {
    if (g_whisper_log != nullptr && text != nullptr) {
        g_whisper_log((int32_t) level, text);
    }
}

// Route whisper.cpp's (and ggml's) logging to Rust instead of stderr, which
// would scribble over the TUI.
void rcn_whisper_init(rcn_log_fn log) {
    g_whisper_log = log;
    whisper_log_set(whisper_log_trampoline, nullptr);
}

struct whisper_context * rcn_whisper_load(const char * path, bool use_gpu) {
    struct whisper_context_params cparams = whisper_context_default_params();
    cparams.use_gpu = use_gpu;
    cparams.flash_attn = use_gpu;
    try {
        return whisper_init_from_file_with_params(path, cparams);
    } catch (...) {
        return nullptr;
    }
}

void rcn_whisper_free(struct whisper_context * ctx) {
    if (ctx != nullptr) {
        whisper_free(ctx);
    }
}

// Transcribe 16 kHz mono samples (English). Returns the text as a malloc'd
// UTF-8 string the caller frees with rcn_string_free, or NULL on failure.
// `prompt` (may be NULL) biases the vocabulary (names, project words).
// `no_speech` receives the highest no-speech probability across segments:
// near 1.0 means whisper thinks the audio held no words.
char * rcn_whisper_transcribe(
        struct whisper_context * ctx,
        const float * samples,
        int32_t n_samples,
        int32_t n_threads,
        const char * prompt,
        float * no_speech) {
    if (ctx == nullptr || samples == nullptr || n_samples <= 0) {
        return nullptr;
    }
    struct whisper_full_params params = whisper_full_default_params(WHISPER_SAMPLING_GREEDY);
    params.n_threads        = n_threads > 0 ? n_threads : params.n_threads;
    params.language         = "en";
    params.detect_language  = false;
    params.translate        = false;
    params.no_context       = true;
    params.no_timestamps    = true;
    params.single_segment   = false;
    params.print_special    = false;
    params.print_progress   = false;
    params.print_realtime   = false;
    params.print_timestamps = false;
    params.suppress_blank   = true;
    params.suppress_nst     = true;
    params.initial_prompt   = (prompt != nullptr && prompt[0] != '\0') ? prompt : nullptr;

    int rc;
    try {
        rc = whisper_full(ctx, params, samples, n_samples);
    } catch (...) {
        rc = -1;
    }
    if (rc != 0) {
        return nullptr;
    }

    std::string text;
    float worst = 0.0f;
    const int n = whisper_full_n_segments(ctx);
    for (int i = 0; i < n; i++) {
        const char * seg = whisper_full_get_segment_text(ctx, i);
        if (seg != nullptr) {
            text += seg;
        }
        const float p = whisper_full_get_segment_no_speech_prob(ctx, i);
        if (p > worst) {
            worst = p;
        }
    }
    if (no_speech != nullptr) {
        *no_speech = worst;
    }
    char * out = (char *) std::malloc(text.size() + 1);
    if (out == nullptr) {
        return nullptr;
    }
    std::memcpy(out, text.c_str(), text.size() + 1);
    return out;
}

struct whisper_vad_context * rcn_vad_load(const char * path) {
    struct whisper_vad_context_params vparams = whisper_vad_default_context_params();
    vparams.n_threads = 1;
    // The VAD model is tiny; the CPU is faster than a GPU round trip.
    vparams.use_gpu = false;
    try {
        return whisper_vad_init_from_file_with_params(path, vparams);
    } catch (...) {
        return nullptr;
    }
}

void rcn_vad_free(struct whisper_vad_context * vctx) {
    if (vctx != nullptr) {
        whisper_vad_free(vctx);
    }
}

void rcn_vad_reset(struct whisper_vad_context * vctx) {
    if (vctx != nullptr) {
        whisper_vad_reset_state(vctx);
    }
}

// Speech probability for each 512-sample window of `samples` (16 kHz mono),
// keeping the model's state between calls so a live stream can be fed in
// pieces. Writes at most `max_out` probabilities and returns how many were
// written, or -1 on failure.
int32_t rcn_vad_probs(
        struct whisper_vad_context * vctx,
        const float * samples,
        int32_t n_samples,
        float * out,
        int32_t max_out) {
    if (vctx == nullptr || samples == nullptr || n_samples <= 0 || out == nullptr) {
        return -1;
    }
    bool ok;
    try {
        ok = whisper_vad_detect_speech_no_reset(vctx, samples, n_samples);
    } catch (...) {
        ok = false;
    }
    if (!ok) {
        return -1;
    }
    const int n = whisper_vad_n_probs(vctx);
    const float * probs = whisper_vad_probs(vctx);
    const int count = n < max_out ? n : max_out;
    for (int i = 0; i < count; i++) {
        out[i] = probs[i];
    }
    return count;
}

}  // extern "C"
