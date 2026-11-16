#!/usr/bin/env bash
# Push HEAD to BRANCH and open (or reuse) a pull request from it into BASE, then start CI on it.
#
#   ci/bot-pr.sh BASE BRANCH TITLE BODY_FILE
#
# Shared by the workflows that open pull requests themselves: cut-line.yml (moving `main` on when a
# release line is cut), backport.yml (a merged fix onto a release line) and release.yml (a line's
# completed release audit back to `main`). Needs GH_TOKEN with contents, pull-requests and actions
# write, and a checkout whose `origin` can push BRANCH.
#
# WHY IT DISPATCHES CI: a pull request opened with a workflow's GITHUB_TOKEN starts no workflow run
# (GitHub's rule against runs triggering runs), so `ci.yml`'s `pull_request` trigger never fires
# and the required checks of `main` and of `release/v*` would never report. A `workflow_dispatch`
# is the one event that token may start, and its checks land on the same commit, which is what the
# rulesets read.
set -euo pipefail
[ $# -eq 4 ] || { echo "usage: $0 BASE BRANCH TITLE BODY_FILE" >&2; exit 2; }
base=$1 branch=$2 title=$3 body=$4

git push --force origin "HEAD:refs/heads/$branch"
existing=$(gh pr list --repo "$GITHUB_REPOSITORY" --head "$branch" --base "$base" --state open \
  --json url --jq '.[0].url // empty')
if [ -n "$existing" ]; then
  url=$existing
  echo "updated $url"
else
  url=$(gh pr create --repo "$GITHUB_REPOSITORY" --base "$base" --head "$branch" \
    --title "$title" --body-file "$body")
  echo "opened $url"
fi
gh workflow run ci.yml --repo "$GITHUB_REPOSITORY" --ref "$branch"
echo "- $title: $url" >> "${GITHUB_STEP_SUMMARY:-/dev/null}"
