---
name: git-rescue
description: "Recover from git mistakes safely and resolve merge conflicts. Use when the user committed to the wrong branch, needs to undo or amend a commit, lost work or a branch, is in a detached HEAD, hit merge/rebase conflicts, needs to unstage files, or asks about force-pushing."
---

# Git Rescue

Run `git status` FIRST, always. Then find your situation in this table and follow only that section.

| Situation | Section |
|---|---|
| Committed on the wrong branch | 1 |
| Undo the last commit (not pushed) | 2 |
| Undo a commit that was already pushed | 3 |
| Work or branch disappeared | 4 |
| "detached HEAD" state | 5 |
| Merge or rebase conflicts | 6 |
| Unstage files / discard uncommitted edits | 7 |
| Need to force-push | 8 |

1. **Committed on the wrong branch.**
```bash
git branch save-my-commit          # the commit is now safe on this branch
git reset --soft HEAD~1            # current branch steps back; changes stay staged
git stash                          # park the changes
git checkout CORRECT_BRANCH
git stash pop
git commit                         # redo the commit where it belongs
git branch -D save-my-commit       # only after confirming the commit exists on CORRECT_BRANCH
```

2. **Undo the last local commit** (keep the changes): `git reset --soft HEAD~1` — files stay staged. To also unstage: `git reset HEAD~1`. Never use `--hard` here.

3. **Undo a pushed commit.** Never rewrite pushed history on a shared branch. Create an inverse commit instead:
```bash
git revert COMMIT_HASH             # opens no editor with: git revert --no-edit HASH
git push
```

4. **Lost work or branch.** Almost nothing is gone until garbage-collected:
```bash
git reflog                         # every position HEAD has had; find the lost commit
git branch rescue LOST_HASH        # anchor it to a new branch
git log rescue -3 --stat           # confirm it is the right content
```
   If uncommitted work was lost to a checkout, also try `git stash list` and `git fsck --lost-found`.

5. **Detached HEAD.** Your commits are not on any branch. Anchor them, then return:
```bash
git branch rescue                  # names the current position
git checkout main                  # or the branch you came from
git merge rescue                   # bring the commits in, if wanted
```

6. **Merge/rebase conflicts.** One file at a time:
   1. `git status` — list the files marked "both modified".
   2. `read` each conflicted file; between `<<<<<<<` and `=======` is your side, between `=======` and `>>>>>>>` is theirs. Edit the file to the correct final content and delete all three marker lines.
   3. After each file: `git add FILE`.
   4. Verify no markers remain anywhere: `grep -rn "<<<<<<<" --include="*" .` must print nothing.
   5. Finish: `git commit` (merge) or `git rebase --continue` (rebase).
   6. To abandon instead: `git merge --abort` / `git rebase --abort` returns to the pre-merge state.

7. **Unstage / discard.**
   - Unstage a file (keep edits): `git restore --staged FILE`.
   - Discard edits in one file (destructive — confirm with the user first): `git restore FILE`.
   - Park everything to get a clean tree: `git stash` (bring back later with `git stash pop`).

8. **Force-push.** Only after rewriting local history on a branch ONLY you use, and never main/master/develop:
```bash
git push --force-with-lease        # fails if the remote moved — protects teammates
```
   Plain `--force` is never the answer.

## Rules

- `git status` before every rescue action, and again after.
- Before any `reset`: `git stash` anything uncommitted, or the reset can destroy it.
- Never run `git reset --hard`, `git push --force`, or `git clean` as a first resort; each destroys work irrecoverably. Prefer the recipes above.
- Never rewrite history that has been pushed to a shared branch — revert instead (section 3).
- If a rescue step errors, stop and re-read `git status` — do not chain more commands onto a broken state.
- Recommend the user add harness-level protection in `~/.rocinante/config.toml` — these block the dangerous commands no matter what any model tries:
```toml
[permissions]
deny = ["Bash(git push --force:*)", "Bash(git reset --hard:*)", "Bash(git clean:*)"]
```
