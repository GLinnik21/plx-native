---
name: land-pr
description: >
  Land a pull request: check that its CI says it may land, wait for CI, squash-merge it with a
  proper message, and restack a branch that sat on top of it. Use whenever the task is to merge,
  land or squash a PR, "wait for CI and merge", "is it green yet", "watch the checks", "merge when
  green", "re-base / restack the stacked PR after the one below it squashed", or when a command is
  refused with "BLOCKED: a pull request merged by hand". A raw `gh pr merge` is refused by the
  outbound-guard hook in Claude sessions, and a merge chained after a watcher once merged a red PR
  (#387); `tools/pr-land.py` is the way through. It decides nothing about WHETHER a PR may merge:
  that is still the owner's authorization.
---

# land-pr: `tools/pr-land.py`

Run it from a checkout of the repository. It merges only a head whose checks say it may land, then
verifies the merge reached the base branch. It calls `gh pr merge --squash --match-head-commit`
itself (never `--auto`, which waits for the required checks only), so the hook never sees that call.

**It grants no merge authority.** Merge only what the owner has authorized (a per-PR or per-series
authorization). `main` takes squash merges only, and the squash is made by GitHub. A green gate is permission to merge a PR you were asked to merge,
never a reason to merge one you were not.

## Commands

    tools/pr-land.py gate  N [--sha S]
    tools/pr-land.py wait  N --sha S [--timeout 45m]
    tools/pr-land.py merge N --sha S --subject "Title (#N)" (--body-file F | --body-from-pr) [--timeout 45m]
    tools/pr-land.py restack --worktree W --upper BRANCH --lower PR [--dry-run] [--push]

`S` is the full head SHA you pushed and want landed. `gate N` without `--sha` prints the head to use.
A gate is evidence about that exact commit, so a SHA you did not push or review is not green.
`gate` is the read-only verdict (foreground is fine; it re-polls GitHub lag for about 60 s at most).
`wait` is the same verdict polled until `--timeout`; `merge` is `wait`, then the merge, then the verify.
The PR must be open, not a draft, based on `main` or `release/v*`, with every required check passing and
`mergeStateStatus` CLEAN. `--help` on each subcommand and the module docstring have the rest.

## Exit codes

| code | meaning | what to do |
|---|---|---|
| 0 | green; `merge`: merged and verified, merge commit's check-runs printed | done |
| 1 | red: a check failed, nothing merged. Or "gh pr merge succeeded;" and the verify failed | red: fix on the branch, push, take the NEW head. Succeeded: the PR IS merged, do not run `merge` again, inspect `origin/main` |
| 2 | usage | fix the command |
| 3 | refused: not open, a draft, BEHIND or DIRTY (merge `origin/<base>` into the branch: `origin/main` unless the PR targets a `release/v*` line), a base that does not land, a required check skipped, a private value in the message, or `gh pr merge` still saying the head differs after 3 retries with the head unmoved | read the reason it names; never route around it |
| 4 | not settled yet: checks pending, none reported, a required one missing, GitHub still computing | nothing merged. Run `wait` again with the same SHA. Checks that never appear mean the workflow did not trigger: look at the PR, do not merge by hand |
| 5 | head moved: the PR is no longer at `S` | someone pushed. Run `gate N`, look at what changed, then start over with the new head |

## Long waits

`wait` and `merge` can poll for 45 minutes. Run them with Bash `run_in_background` and let the exit
notification bring you back; do not poll the output, do not `sleep`, and do not hold other work on it.
Never chain `gh pr merge` after a watcher (`gh pr checks --watch && gh pr merge`), and never read
`gh run list` as success: it has said "completed success" while checks were still running. The only
evidence is the script's own exit code 0.

## The squash message

The repository setting would paste every wip commit, so `merge` requires a body. Write the change's own
account as a `--body-file` (the usual case: a hand-written body differs from the PR body, which carries
Gates sections and bench tables), or pass `--body-from-pr` to send the PR's own body (opt-in).
A `--subject` and a `--body-file` are scanned for private values twice: by outbound-guard on the command
line and the file, and by the script before any `gh` call (exit 3). A `--body-from-pr` body is fetched,
then scanned once, by the script alone, before the merge call; the hook never sees it. `main` is public
and its history permanent: use the stand-ins in `docs/shared-servers.md`, never a gitignored value.

## Head-oid race

`gh pr merge --match-head-commit` can fail just after a push with "expected head oid does not match".
It is a race, not a real mismatch. The script waits 15 s, re-reads the head, retries up to 3 times if it
did not move, and exits 5 if it did; three unchanged retries end in exit 3. By hand it would be: wait about
15 s and retry.

## Stacked PRs: `restack`

After the PR below (`--lower`) squash-merges, the branch above it must be brought onto the squash by
MERGES, never a rebase and never a force-push. `restack` runs these four with `git -C W`, in this order,
and stops at the first that fails, naming it:

1. `git merge <the lower PR's final head>` (read from `refs/pull/L/head`; the branch is deleted on merge)
2. `git merge <squash>^`
3. `git merge --no-ff -s ours <squash>`
4. `git merge origin/<base>`

Step 3 without step 2 would drop whatever `main` gained before the squash; step 4 brings in what it
gained after. It refuses (3) unless the lower PR is MERGED, W is a clean checkout on `--upper` with no
merge, cherry-pick or revert in progress, and the squash is a one-parent commit on `origin/<base>`; an
`--upper` equal to the base is refused too. On a conflict (exit 1) resolve it, commit, and run the same
command again: finished steps are "Already up to date". `--dry-run` prints the four commands and changes nothing.
`--push` pushes the upper branch only, as a plain `git push` (a branch that moved on origin is rejected,
with git's reason printed); an upstream that is not on `origin` is refused (3) before the first merge.
Then `gate` the upper PR on its new head.

## The refusal and its hatch

outbound-guard refuses `gh pr merge` (any arguments), a non-GET `gh api` call to `.../pulls/N/merge` and a GraphQL
`mergePullRequest` written on the command line or in a heredoc (a query read from a file is not seen); a deny rule in
`.claude/settings.json` blocks the GitHub MCP connector's `merge_pull_request`. All of it is cooperative: do not look
for another route to a merge you were refused. The owner's own manual merge is spelled by prefixing the command with
`PLX_MERGE_BYPASS=1`. An agent does not use it to get past a refusal, a red check or a missing authorization.
