---
name: writing-tests
description: Write focused tests that assert real behavior and edge cases, matching the project's existing test framework and style, covering failure modes and not just the happy path. Use when the user asks to add tests, improve coverage, or write a test for a function, module, or bug fix.
---

# Writing tests

Test behavior, not implementation. Match what the project already does.

1. Look first. Find existing tests and the framework, file layout, naming, and assertion style in use. Copy those conventions exactly — do not introduce a new framework.
2. Identify the behavior under test: the contract of the function or module, its inputs, and its expected outputs.
3. Write one test per behavior. Name each test for the behavior it checks (e.g. `returns_empty_for_no_matches`), not for the function name alone.
4. Cover the failure modes, not just the happy path: empty and null inputs, boundaries, invalid input, error paths, and any bug the test is meant to lock in.
5. Assert on the meaningful result — the return value, state change, or effect. Do not assert on incidental output (log text, exact whitespace, ordering that is not guaranteed) that will break on unrelated changes.
6. Keep each test independent: no shared mutable state, no reliance on run order, deterministic inputs. Set up and tear down within the test.
7. Run the tests. Confirm they pass, and confirm a new test actually fails when the behavior is broken.

Rules:
- A test that never fails is worthless — make sure each one can fail.
- Prefer several small, focused tests over one that asserts everything.
- If you are testing a bug fix, first write the test that reproduces the bug, then confirm the fix makes it pass.
