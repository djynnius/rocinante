---
name: rocinante-help
description: "How to use Rocinante itself, answered from its bundled user guide: slash commands, keys, config, models and /localmodel, the crew and @name, /collaborate, /thinking and scrutiny, permissions, skills, MCP and LSP, the toolbox, sessions. Use when the user asks how to do something in Rocinante, what a Rocinante command or setting does, or why Rocinante behaves a certain way."
---

# Answering questions about Rocinante

Rocinante's full user guide ships with this skill as `GUIDE.md`. The line above these instructions ends with `Files it mentions live in <DIR>` — the guide is `<DIR>/GUIDE.md`.

1. **Decide what the question is about.** "How do I switch models?" asked inside Rocinante is about Rocinante. "How do I add a route?" is about the user's project. If it could be either, ask one short question: "Do you mean in Rocinante, or in this project?" — then stop.
2. **Search the guide** with the `grep` tool for the command, key or feature (case-insensitive), e.g. `/localmodel`, `num_ctx`, `@name`, `scrutiny`. Try a synonym if nothing matches.
3. **Read the matching section** with the `read` tool (use an offset near the grep hit) so the answer reflects the whole section, not one line.
4. **Answer from the guide only**: the exact command, key or file path, then one or two sentences on what it does. Quote config as TOML. If the guide does not cover it, say so plainly — never invent a command or key.
5. **Changing config** is a different job: point the user to `/config <request>` (the agent edits `~/.rocinante/config.toml`), or follow the rocinante-config skill if you are asked to make the change yourself.
