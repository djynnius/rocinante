---
name: code-review
description: Review a diff or file across correctness, edge cases, error handling, security, performance, tests, and readability, reporting findings most-severe first with file:line and a concrete fix for each. Use when the user asks for a code review, wants feedback on a change or PR, or asks whether code is correct or safe to merge.
---

# Code review

Review the target change methodically, one dimension at a time.

1. Read the full diff or file first. Understand the intent before judging the code.
2. Check each dimension in order:
   - Correctness: does it do what it claims? Trace the logic on real inputs.
   - Edge cases: empty, null, zero, negative, overflow, concurrent, boundary values.
   - Error handling: are failures caught, propagated, and surfaced? Any swallowed errors or unhandled paths?
   - Security: input validation, injection, auth, secrets, unsafe deserialization, path traversal.
   - Performance: needless allocation, N+1 loops, blocking calls, quadratic scaling on hot paths.
   - Tests: does the change add or update tests? Do they cover the new behavior and its failure modes?
   - Readability: naming, dead code, unclear control flow, missing invariants.
3. Optionally delegate a second adversarial pass with a `task` call to `agent: bobbie`, then merge its findings with yours (dedupe, keep the sharper wording).
4. Report findings most-severe first. For each: the file:line, what is wrong, why it matters, and a concrete fix.
5. Separate must-fix (bugs, security, data loss) from nits (style, naming). Label them clearly.

Rules:
- Every finding needs a location and an actionable fix, not just "this looks off."
- Do not invent problems to pad the review. If it is clean, say so.
- If `bobbie` is not available, do the adversarial pass yourself: re-read the change assuming it is broken and try to prove it.
