---
name: review-pr
description: >
  Review someone's pull request locally and post the review from the owner's own GitHub account,
  after the owner has read the draft: "review #N", "review this PR", "check PR N", "what do you
  think of #N", "post a review on N", "final review", and any external contributor's PR. Reads the
  whole conversation and CI first, analyses with the repo's `pr-reviewer` agent (plus the tests and
  `fw-compat-reviewer` when the diff needs them), drafts in a low-noise maintainer style (`lgtm`
  or a short numbered list, findings inline), and posts one formal GitHub review only on the
  owner's click. Use it instead of the `@claude` GitHub bot when the PR is from a fork and matters,
  when tests have to run, or when the review should be the owner's. For a review of your OWN branch
  before opening a PR, run `pr-reviewer` directly; this skill is for reviewing a PR that exists.
---

# review-pr: a local review, posted as the owner

The workflow and the writing rules follow umputun's review skill
([umputun/cc-thingz](https://github.com/umputun/cc-thingz), `plugins/review`), chosen by the owner
because it knows about bot noise. The analysis is this repo's own: `pr-reviewer` reviews against
AGENTS.md's working rules (read from the base), `docs/review-checklist.md` and the module guides,
and `tools/review-eval.py` grades it against past PRs whose bugs are known. Do not replace it with a
generic reviewer prompt; a change to it is measured with `review-eval.py` first.

The `@claude` bot (`.github/workflows/claude.yml`) is the quick first pass on every thread. This
skill is the deep one: it can run tests and the repo's reviewers, and its review counts as the owner's.

## 1. Context, before reading any code

```bash
gh pr view N --json number,title,body,author,state,isDraft,baseRefName,headRefName,headRefOid,\
headRepositoryOwner,isCrossRepository,additions,deletions,changedFiles,files,mergeable,mergeStateStatus
gh api repos/{owner}/{repo}/issues/N/comments --jq '.[] | "[\(.user.login)] \(.body)"'
gh api repos/{owner}/{repo}/pulls/N/comments  --jq '.[] | "[\(.user.login) on \(.path):\(.line // .original_line)] \(.body)"'
gh api repos/{owner}/{repo}/pulls/N/reviews   --jq '.[] | "[\(.user.login) - \(.state)] \(.body)"'
gh pr checks N
```

Summarize for yourself: what is already resolved, what is still open, what the OWNER already said
(never repeat it), and what bots found (Copilot, the `@claude` bot, Codex). A bot finding goes into
the draft only after you have checked it in the code. A CONFLICTING or BEHIND PR explains odd
"deletions" in the diff; say so early.

Then ask the owner once (AskUserQuestion): **Full** (worktree, `pr-reviewer`, tests when allowed)
or **Quick** (diff only). Quick skips to step 4 with what `gh pr diff N` shows.

## 2. Worktree (never switch any checkout's branch)

```bash
git fetch origin "pull/N/head:refs/remotes/pr/N" && git fetch origin main
git worktree add --detach ".claude/worktrees/review-N" "refs/remotes/pr/N"
```

`gh pr checkout` moves a checkout's branch: do not use it. The worktree goes away in step 6.

## 3. Analysis

**`pr-reviewer`** (Agent tool, `subagent_type: pr-reviewer`), on `origin/main...refs/remotes/pr/N`,
run from the worktree. Hand it the context from step 1 (resolved points, the owner's earlier
comments, open questions) so it does not re-raise them. Its findings carry `file:line`, a severity
and a fix; keep the full head SHA its report names.

**`fw-compat-reviewer`** too when `pr-reviewer` recommends it (FFI, linkage, `dynlib!`, Starfish,
ACB, curl, FFmpeg).

**Tests: who wrote the code decides.**

- The author has write access (`gh api repos/{owner}/{repo}/collaborators/AUTHOR/permission`):
  pick the tier with the `which-tier` skill and run it in the worktree (`make test-crate C=...`,
  `make test-hostsim ...`). Never `make check` in the foreground; it can queue for a long time.
- An outside contributor (a fork, no write access): running their tests runs THEIR code on this
  Mac, next to the owner's tokens, `~/.config/plxnative/` and the TV. Read CI's results instead
  (`gh pr checks N`; a fork's CI holds no secrets). Run anything locally only after you have read
  every changed build input (`build.rs`, `Makefile`, `Cargo.toml`, `ci/`, `tests/`, `tools/`, any
  script the tests call) and the owner has said yes for this PR.

**Also judge**, as umputun's skill does: over-engineering (name the simpler thing), code that does
not match the patterns around it (AGENTS.md rule 1: a shared mechanism exists), and scope creep.
Sort every file as core, supporting, related cleanup or unrelated; unrelated and risky is a finding,
an unrelated lint fix is not.

Show the owner the condensed findings, then ask each open design question one at a time before
drafting. Their answer goes into the draft as their decision.

## 4. The draft

Findings that sit on a line become **inline comments**, one or two sentences each, with a
`suggestion` block when the fix is small and certain. The **body** is:

- `lgtm` when nothing is wrong, or
- a short numbered list of what is not tied to a line (failing CI, a question, scope creep), each
  with `file:line`, then at most one closing line (`otherwise looks good`).

Writing rules (umputun's `writing-style`): casual, lowercase is fine, direct; state an opinion
instead of hedging, say "not sure" when you are not. Never restate what the PR does: the author
knows. No headings, no praise, no "great work", no sign-off, no "let me know". None of:
comprehensive, robust, leverage, utilize, facilitate, seamless, "it's worth noting", "in order to",
"additionally", "furthermore". Code in a suggestion handles its errors. Severity words (BLOCKER,
MAJOR) stay out of the comment; a real bug reads as one plainly ("this accepts `wss://v1/ws`").

Show the whole draft (body and each inline comment with its `path:line`) as one text block, then
ask (AskUserQuestion): **Comment** / **Request changes** / **Approve** / **Edit** / **Cancel**.
Nothing is posted without that answer, every time: a review on someone's PR is the owner speaking.

## 5. Post one review

One formal review, the inline comments attached, pinned to the commit that was reviewed. Write it
to a file and send it; never put the text on the command line.

```bash
cat > "$TMPDIR/review-N.json" <<'JSON'
{
  "commit_id": "<the full head SHA pr-reviewer read>",
  "event": "COMMENT",
  "body": "<body>",
  "comments": [
    {"path": "rust-modules/owp/src/relays.rs", "line": 32, "side": "RIGHT", "body": "<comment>"}
  ]
}
JSON
gh api -X POST "repos/{owner}/{repo}/pulls/N/reviews" --input "$TMPDIR/review-N.json"
```

`event` is `COMMENT`, `REQUEST_CHANGES` or `APPROVE` as the owner chose. A `line` must be in the
diff on that side, or GitHub refuses the whole review (422); move such a comment into the body.
The head moved since the review (`gh pr view N --json headRefOid`): say so and re-review, do not
post findings about code that is no longer there. The repo is public: never quote a value from a
gitignored private file (AGENTS.md); outbound-guard scans the file too.

Record what `pr-reviewer` found on that SHA, so the landing gate reads it:

```bash
tools/pr-land.py reviewed N --sha <SHA> --blockers B --majors M --minors K
```

An **Approve** is not a merge. Landing goes through the `land-pr` skill (`tools/pr-land.py`,
squash only) and needs the owner's authorization to merge, which an approval is not.

## 6. Clean up

```bash
git worktree remove ".claude/worktrees/review-N" && git update-ref -d "refs/remotes/pr/N"
```

Do it after the review is posted or cancelled, not before: follow-up questions need the worktree.
A worktree with build output in it (a test run) goes with `git worktree remove --force` only after
`git -C .claude/worktrees/review-N status --short` shows nothing but ignored files.
