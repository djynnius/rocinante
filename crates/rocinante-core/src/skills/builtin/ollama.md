---
name: ollama
description: "Run and manage local LLMs with Ollama: pull/list/remove models, run prompts, call the REST API, write Modelfiles, tune keep_alive, fix common errors. Use when asked to set up or debug Ollama, run a local model, call the Ollama API, or create a custom model variant."
---

# Ollama

Local LLM runtime with a REST API on port 11434. Run everything with the `bash` tool.

1. **Is the server up?**
```bash
curl -s localhost:11434/api/version || echo DOWN
```
   DOWN → start it: `ollama serve` (foreground; on desktops it usually runs as a service already). "address already in use" when starting means it is ALREADY running — that is success, not an error.

2. **Model management:**
```bash
ollama list                        # installed models + sizes
ollama ps                          # loaded right now + VRAM use
ollama pull llama3.2:3b            # download (tag = size/quant variant)
ollama show llama3.2:3b            # context length, parameters, template
ollama rm MODEL                    # delete
```
   Before pulling something big: models need roughly their file size in free RAM/VRAM — check `ollama list` sizes and prefer a smaller quant (`:q4_K_M`) when tight.

3. **Run a prompt:**
```bash
ollama run llama3.2:3b "Summarize: ..."          # one-shot, prints and exits
echo "long prompt from a file" | ollama run llama3.2:3b   # via stdin
```
   Never start `ollama run MODEL` with no prompt from an agent — it opens an interactive REPL that hangs the shell.

4. **REST API** (what apps should use):
```bash
curl -s localhost:11434/api/chat -d '{
  "model": "llama3.2:3b",
  "messages": [{"role": "user", "content": "Hello"}],
  "stream": false
}'
# completion endpoint: /api/generate with {"model","prompt","stream":false}
```
   Always `"stream": false` when parsing with scripts/jq. `"keep_alive": "10m"` keeps the model loaded between calls (`0` unloads immediately, `-1` forever).

5. **Custom variant via Modelfile:**
```bash
cat > Modelfile <<'EOF'
FROM llama3.2:3b
PARAMETER temperature 0.2
PARAMETER num_ctx 8192
SYSTEM You are a terse assistant. Answer in one sentence.
EOF
ollama create terse-llama -f Modelfile
ollama run terse-llama "test"
```

6. **Troubleshooting table:**
   | Symptom | Fix |
   |---|---|
   | `connection refused` on 11434 | server not running → `ollama serve` |
   | `model not found` | exact name+tag from `ollama list`; else `ollama pull` it |
   | Very slow / swapping | model too big → smaller quant or smaller model |
   | Out of memory / killed | `ollama ps` to see what is loaded; `keep_alive: 0` older models; smaller model |
   | Garbage/looping output | check `num_ctx` not exceeded; lower temperature |

## Rules

- Check the server (step 1) before every other command; most "ollama is broken" reports are just the server not running.
- Non-interactive only: prompts as arguments or stdin, API with `"stream": false` — never open the REPL.
- Do not `ollama rm` models you did not pull this session without confirming with the user — downloads are multi-GB.
- Rocinante itself talks to Ollama as a provider — model switching inside Rocinante is `/model`, not this skill; this skill is for managing the Ollama installation and using it from scripts.
