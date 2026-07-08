---
name: deep-research
description: Fan out parallel researcher subagents to investigate a question across the codebase and web, verify load-bearing claims, and synthesize a structured, cited answer. Use when the user asks for deep or thorough research, a multi-source investigation, or a fact-checked report that spans several files or topics.
---

# Deep research

Investigate a hard question by decomposing it, researching in parallel, verifying, and synthesizing.

1. Decompose the question into 2-5 independent sub-questions. Each should be answerable on its own. If they are not independent, split differently until they are.
2. Fan out. In ONE message, issue one `task` call per sub-question — use `agent: naomi` for codebase exploration, `agent: miller` for external/web research. All these calls run in parallel, so send them together, not one at a time.
3. Collect the briefs each subagent returns. Note the specific sources (file:line, URLs, doc names) behind each finding.
4. Verify every load-bearing claim — anything the final answer depends on. Re-check the source yourself, or delegate a fresh verification pass (another `task` to `miller`/`naomi`) that confirms it independently. Discard claims you cannot source.
5. Synthesize a structured answer: lead with the direct conclusion, then supporting sections. Cite each fact inline (file:line or URL). Keep the reasoning traceable.
6. State what remains uncertain, what you could not verify, and what a follow-up would need.

Rules:
- Prefer primary sources over summaries. One confirmed source beats three vague ones.
- Do not present an unverified claim as fact — mark it as tentative.
- If the subagents are not available, do the sub-steps yourself sequentially: research each sub-question in turn, then verify, then synthesize.
