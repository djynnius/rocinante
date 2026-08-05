---
name: github-cli
description: "Work with GitHub from the terminal using the gh CLI: open and review pull requests, create and comment on issues, watch CI/Actions runs, view releases, fork and clone, and script GitHub data with --json. Use when asked to open a PR, review a PR, check CI status, watch a workflow run, file an issue, cut or inspect a release, or otherwise talk to GitHub from the command line."
---

# GitHub CLI (gh)

Run everything with the `bash` tool. `gh` talks to GitHub over HTTPS; `git` moves commits. Use `gh` for pull requests, issues, Actions runs, and releases — not for local history (that is the `git-rescue` skill).

Do step 1 FIRST, every time.

1. **Is gh installed and logged in?**

   ```
   gh --version || echo "gh not installed — install from https://cli.github.com or fall back to plain git + the web UI"
   gh auth status
   ```

   If `gh auth status` says you are not logged in, STOP and tell the user to run `gh auth login` themselves (it is interactive — you cannot complete it). Do not try to pass a token on the command line.

2. **Find your task in this table and follow only that section.**

| Task | Section |
|---|---|
| Open a pull request | 3 |
| View / check out / review a PR | 4 |
| Watch a CI / Actions run | 5 |
| Create or comment on an issue | 6 |
| View or download a release | 7 |
| Fork or clone a repo | 8 |
| Read GitHub data in a script | 9 |

## 3. Open a pull request

Never push to the default branch to open a PR — work on a feature branch. Check first:

```
git rev-parse --abbrev-ref HEAD
```

If that prints `main` or `master`, STOP and ask the user before continuing (create a branch first: `git switch -c FILL_IN_BRANCH`). Otherwise:

```
git push -u origin FILL_IN_BRANCH
gh pr create --title "FILL_IN_TITLE" --body "FILL_IN_BODY" --base main
```

- Add `--draft` for a draft PR.
- `--web` opens the PR form in a browser instead (hand off to the user).
- Verify: `gh pr view --json number,url -q '.url'` prints the new PR URL.

## 4. View, check out, or review a PR

```
gh pr list                                  # open PRs in this repo
gh pr view FILL_IN_NUMBER                    # description + status
gh pr diff FILL_IN_NUMBER                    # the code changes
gh pr checkout FILL_IN_NUMBER                # check it out locally to test
```

Only submit a review verdict when the user asked you to review:

```
gh pr review FILL_IN_NUMBER --comment --body "FILL_IN"   # neutral note
gh pr review FILL_IN_NUMBER --approve  --body "FILL_IN"   # approve
gh pr review FILL_IN_NUMBER --request-changes --body "FILL_IN"
```

Do NOT merge unless the user explicitly says to merge: `gh pr merge FILL_IN_NUMBER --squash`.

## 5. Watch a CI / Actions run

```
gh run list --branch FILL_IN_BRANCH --limit 5
gh run watch FILL_IN_RUN_ID --exit-status        # blocks until done; nonzero exit on failure
gh run view FILL_IN_RUN_ID --json conclusion,jobs # per-job results
gh run view FILL_IN_RUN_ID --log-failed          # only the failing step's log
```

Report the conclusion (`success` / `failure`) and, on failure, the failing job and the first real error line from `--log-failed`.

## 6. Create or comment on an issue

```
gh issue create --title "FILL_IN_TITLE" --body "FILL_IN_BODY"
gh issue list --state open
gh issue view FILL_IN_NUMBER
gh issue comment FILL_IN_NUMBER --body "FILL_IN"
```

## 7. View or download a release

```
gh release list
gh release view FILL_IN_TAG --json tagName,assets -q '.assets[].name'
gh release download FILL_IN_TAG --pattern "FILL_IN_GLOB"
```

Cutting a release (`gh release create`) publishes to everyone — only do it when the user explicitly asks, and confirm the tag first.

## 8. Fork or clone a repo

```
gh repo clone FILL_IN_OWNER/FILL_IN_REPO
gh repo fork FILL_IN_OWNER/FILL_IN_REPO --clone
gh repo view FILL_IN_OWNER/FILL_IN_REPO
```

## 9. Read GitHub data in a script

`gh` returns structured data with `--json` and filters it with `-q` (jq syntax). Prefer this over scraping human output:

```
gh pr list --json number,title,author -q '.[] | "\(.number) \(.title)"'
gh run list --json databaseId,status,conclusion --limit 1
gh api repos/FILL_IN_OWNER/FILL_IN_REPO/commits -q '.[0].sha'   # raw REST for anything gh has no subcommand for
```

## Rules

- Run `gh auth status` before anything else. If not logged in, stop and ask the user to `gh auth login` — never handle a token yourself.
- These actions are outward-facing and hard to undo. Get explicit user confirmation before: pushing to the default branch, merging a PR (`gh pr merge`), publishing a release (`gh release create`), deleting anything (`gh * delete`), or closing someone else's PR/issue.
- Open PRs from a feature branch, never from `main`/`master`. Check the current branch first.
- Prefer `--json … -q …` when you need a specific field; don't parse human-formatted output.
- `gh auth login`, `gh auth token` refresh, and anything opening a browser (`--web`) are interactive — hand them to the user instead of running them.
- Report what happened plainly: the PR/issue URL, or the run conclusion and the failing step on CI failure.
