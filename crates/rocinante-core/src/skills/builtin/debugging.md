---
name: debugging
description: Run the disciplined debugging loop — reproduce the failure, read the error, isolate it to a minimal repro, form one hypothesis, make the minimal fix, and verify by re-running. Use when the user reports a bug, a crash, a test failure, or unexpected behavior and wants it diagnosed and fixed.
---

# Debugging

Find the root cause before touching code. Change one thing at a time.

1. Reproduce the failure exactly. Get the precise command, input, and environment that triggers it. If you cannot reproduce it, you cannot fix it — keep narrowing until you can.
2. Read the error. The message, stack trace, and line number usually point close to the cause. Do not skim it.
3. Isolate. Bisect the input, the commits, or the code path until you have the smallest case that still fails. Remove everything that is not required to trigger it.
4. Form ONE hypothesis for the root cause, stated concretely (what is wrong, where, why it produces this symptom).
5. Make the minimal fix that addresses that root cause — nothing more.
6. Verify by re-running the exact repro from step 1. Confirm the failure is gone and no new failure appeared.
7. If the fix did not work, the hypothesis was wrong. Revise it based on what you just learned and repeat. Do not stack more changes on top.

Optionally delegate root-cause analysis with a `task` call to `agent: amos` and use its finding as your hypothesis in step 4.

Rules:
- Never change unrelated code while fixing a bug.
- Never guess-and-check blindly — no shotgun edits hoping one sticks. One hypothesis, one change, one verification.
- If `amos` is not available, do the root-causing yourself by following steps 1-4.
