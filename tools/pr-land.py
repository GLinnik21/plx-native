#!/usr/bin/env python3
"""Land a pull request only when its checks say it may land (gate, wait, merge), and restack its dependants.

    tools/pr-land.py gate  N [--sha S]
    tools/pr-land.py wait  N --sha S [--timeout 45m]
    tools/pr-land.py merge N --sha S --subject T (--body-file F | --body-from-pr) [--timeout 45m]
    tools/pr-land.py restack --worktree W --upper BRANCH --lower PR [--dry-run] [--push]

Run it from a checkout of the repository (`gh api` fills in {owner}/{repo} from it and the verify step
fetches origin). Anything that can wait longer than a minute belongs in a Bash call with
`run_in_background`, not in the foreground: `wait` and `merge` poll for up to --timeout.

`gate` is the read-only verdict. It fails closed unless ALL of these hold:

  * the PR is OPEN, not a draft, and based on main or release/v*;
  * its head is S (with --sha; without it the verdict prints the head to pass to `wait` and `merge`);
  * checks are reported, and every required context of the base branch's rules is among them (a base
    that requires none: the same list on two polls in a row);
  * nothing is pending, no check failed or was cancelled, every check is pass or skipping, and every
    required check is pass;
  * mergeStateStatus is CLEAN. UNKNOWN, or BLOCKED or UNSTABLE with every check green, is GitHub still
    computing and is re-polled; BEHIND and DIRTY are refused (merge origin/<base> into the branch);
    any other value is refused by name. UNSTABLE ("mergeable with a non-passing commit status") is
    the required checks passing while a non-required one (CodeQL) still runs or failed: the rows
    decide, so a running one is exit 4, a failed one is exit 1 naming it, and it is never green.

When several things are wrong at once the first of these wins, so one PR yields one code: a refusal
(3: not OPEN, a draft, a base that does not land, BEHIND, DIRTY or an unknown mergeStateStatus, an
unrecognised check bucket); the head is not S (5); a check failed or was cancelled (1); a REQUIRED
check is skipping (3); not yet (4: no checks, one pending, a required one not reported); GitHub-side
lag (4: UNKNOWN, BLOCKED or UNSTABLE with every row passing, or no required checks and a check list
that changed since the last poll); and only then green. A head that moved is judged before the
checks because those checks are about another commit.

`gate` re-polls only that GitHub-side lag, for about 60 s; a check still running is exit 4 at once.
`wait` is the same judgment polled with a 30 s to 60 s backoff until --timeout. `merge` is wait, then
`gh pr merge --squash --match-head-commit S` (never --auto: it waits for the required checks only), then
the verify: the PR is MERGED and its merge commit is an ancestor of origin/<base>. The merge commit's
check-runs are printed. On "expected head oid does not match" it waits 15 s and re-reads the head;
unchanged, it retries (3 times at most, then exit 3); moved, exit 5. Every line of a failed verify
starts "gh pr merge succeeded;", since the merge went through and only the check afterwards failed.

`restack` re-bases a stacked branch by MERGES (never a rebase) after the pull request below it, --lower,
has squash-merged, as .agents/skills/land-pr/SKILL.md ("Stacked PRs: restack") has it. It reads the
lower PR (`gh pr view`) and refuses (3) unless it is MERGED with a merge commit, or when BRANCH is
that PR's base branch (it would merge the lower PR into main); refuses (3) unless W is a clean
checkout on BRANCH (an untracked file is not clean) with no merge, cherry-pick or revert in progress,
not even a resolved and staged one that is not yet committed; fetches the
lower PR's final head from `refs/pull/PR/head` (the branch is deleted on merge) and the PR's base
branch (main) from origin; refuses (3) unless the merge commit is a one-parent commit on origin/<base>;
then runs, with `git -C W`, in this order, stopping at the FIRST step that fails:

  1. `git merge <the lower PR's final head>`
  2. `git merge <squash>^`
  3. `git merge --no-ff -s ours <squash>`
  4. `git merge origin/<base>`

Each takes --no-edit and a message naming the pull request. A step that stops (a conflict, usually) leaves the
tree as git left it, names the step and its command, and exits 1; resolve it, commit, and run the
same command again, since the steps already done are then "Already up to date" no-ops. --dry-run
prints the four commands (SHAs filled in) after the read-only checks and changes nothing, not even by
a fetch. --push then pushes BRANCH only, as a plain `git push origin BRANCH` (never --force, so a
branch that moved on origin is rejected: exit 1; the reason git gave is printed) and only once all
four merges succeeded; without it nothing leaves the machine. It follows BRANCH's upstream: one on
origin (a checkout made by `git checkout -B tmp origin/x`) is pushed as `BRANCH:refs/heads/x`, none
as `BRANCH`, and one on any other remote is refused (3) before the first merge. Run it from the
checkout `gh` should read the lower PR in; W may be any worktree of the repository. No `gh pr merge`
is ever called.

`merge` needs a body, since the repository setting would paste every wip commit: --body-file, or
--body-from-pr to send the PR's own body. The subject and the body are run through
.claude/hooks/outbound-guard.py in this process, and a private value refuses the merge before any
`gh` call (a PR body is fetched first, then scanned before the merge call).

Exit codes: 0 green (merge: merged and verified; restack: restacked, and pushed if --push), 1 red (a
check failed, nothing merged; or merged but the verify failed, which says "gh pr merge succeeded"; for
restack, a merge step stopped or the push was rejected, which says where), 2 usage, 3 refused (a rule
says no, `gh` kept failing or answered in a shape this script does not know, `gh pr merge` kept saying
the head differs while the head had not moved, or a private value was found; for restack, the lower
PR is not merged, W is not clean and on BRANCH or has a merge, cherry-pick or revert in progress,
BRANCH is the lower PR's base branch, a fetch failed, a commit is missing after it, the squash is not
on origin/<base> or has other than one parent, or --push meets an upstream that is not on origin),
4 timeout (not settled yet), 5 head moved (the PR is not at S). This tool grants no merge authority: an
owner's authorization to merge is still needed.
"""
import argparse
import importlib.util
import json
import re
import shlex
import subprocess
import sys
import time
from collections import namedtuple
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

EXIT_OK, EXIT_RED, EXIT_USAGE, EXIT_REFUSED, EXIT_TIMEOUT, EXIT_MOVED = range(6)
NAMES = {EXIT_OK: 'GREEN', EXIT_RED: 'RED', EXIT_REFUSED: 'REFUSED', EXIT_TIMEOUT: 'TIMEOUT',
         EXIT_MOVED: 'HEAD MOVED'}

BASE_OK = re.compile(r'^(?:main|release/v.+)$')
SHA = re.compile(r'^[0-9a-f]{40}$')
BRANCH = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._/-]*$')   # no leading + (a forced refspec) or -
HEAD_RACE = re.compile(r'head oid|head branch was modified', re.I)
GOOD_BUCKETS = ('pass', 'skipping')
BAD_BUCKETS = ('fail', 'cancel')
KNOWN_BUCKETS = GOOD_BUCKETS + BAD_BUCKETS + ('pending',)
VIEW_FIELDS = 'headRefOid,state,isDraft,baseRefName,mergeStateStatus'

POLL_FIRST, POLL_MAX, POLL_GROWTH = 30, 60, 1.5   # seconds between polls: 30, 45, 60, 60, ...
SETTLE_SECONDS = 60                               # how long `gate` re-polls GitHub-side lag
MAX_GH_ERRORS = 5                                 # consecutive gh failures before giving up
GH_TIMEOUT = 120                                  # one gh call, so a hung one cannot outlast --timeout
RACE_WAIT, RACE_RETRIES = 15, 3
VERIFY_TRIES, VERIFY_GAP = 3, 5

Env = namedtuple('Env', 'sleep now root')


class GhError(Exception):
    """gh failed or printed nonsense: worth another poll."""


class Refuse(Exception):
    """gh answered, but not in the shape this script was written against: never retried."""


class Verdict:
    """What one look at the pull request says. `code` is the exit code, and EXIT_TIMEOUT means "not
    yet": `lag` marks the not-yet states that are GitHub still computing, not CI still running."""

    def __init__(self, code, reasons, lag=False):
        self.code, self.reasons, self.lag, self.view = code, reasons, lag, None


def say(text, stream=None):
    print(text, file=stream or sys.stdout, flush=True)


def gh(*args, input=None):
    kw = {'input': input} if input is not None else {'stdin': subprocess.DEVNULL}
    try:
        return subprocess.run(['gh', *args], capture_output=True, text=True, timeout=GH_TIMEOUT, **kw)
    except FileNotFoundError:
        raise Refuse('gh is not on PATH')
    except subprocess.TimeoutExpired:
        raise GhError('gh %s timed out' % ' '.join(args[:2]))


def first_line(text):
    return (text.strip().splitlines() or [''])[0]


def gh_json(*args):
    done = gh(*args)
    if done.returncode != 0:
        raise GhError(first_line(done.stderr) or 'gh %s exited %d' % (' '.join(args[:2]), done.returncode))
    try:
        return json.loads(done.stdout)
    except ValueError:
        raise GhError('gh %s printed no JSON' % ' '.join(args[:2]))


def read_pr(number, fields):
    doc = gh_json('pr', 'view', str(number), '--json', fields)
    missing = [k for k in fields.split(',') if not isinstance(doc, dict) or k not in doc]
    if missing:
        raise Refuse('gh pr view lost the field(s) %s' % ', '.join(missing))
    return doc


def read_checks(number):
    done = gh('pr', 'checks', str(number), '--json', 'name,state,bucket,workflow')
    try:
        rows = json.loads(done.stdout)
    except ValueError:
        rows = None
    if isinstance(rows, list):      # exit 8 is "pending"; a JSON list is the answer whatever the code
        for row in rows:
            if not isinstance(row, dict) or 'name' not in row or 'bucket' not in row:
                raise Refuse('gh pr checks rows lost name or bucket')
        return rows
    if 'no checks reported' in done.stderr:     # exit 1, empty stdout: nothing registered yet
        return []
    raise GhError(first_line(done.stderr) or 'gh pr checks exited %d' % done.returncode)


def read_required(base):
    rules = gh_json('api', 'repos/{owner}/{repo}/rules/branches/' + base)
    if not isinstance(rules, list):
        raise Refuse('the branch rules for %s are not a list' % base)
    names = set()
    for rule in rules:
        if isinstance(rule, dict) and rule.get('type') == 'required_status_checks':
            for entry in (rule.get('parameters') or {}).get('required_status_checks') or []:
                context = entry.get('context') if isinstance(entry, dict) else None
                if not isinstance(context, str):   # fail closed: a dropped name would shrink the required set
                    raise Refuse('a required_status_checks entry of the %s rules has no context' % base)
                names.add(context)
    return frozenset(names)


def identities(checks):
    return frozenset((c.get('workflow', ''), c['name']) for c in checks)


def names_of(rows):
    return ', '.join(sorted({c['name'] for c in rows}))


def judge(view, checks, required, sha, previous):
    """The Verdict for one snapshot. Pure. `previous` is the identities of the poll before it (None
    on the first). One PR yields one code, by this order: refusals, the head, red, a skipping required
    check (also a refusal), not-yet, GitHub-side lag, green. The module docstring says why."""
    base, head, status = view['baseRefName'], view['headRefOid'], view['mergeStateStatus']
    refusals = []
    if view['state'] != 'OPEN':
        refusals.append('the pull request is %s, not OPEN' % view['state'])
    if view['isDraft']:
        refusals.append('the pull request is a draft')
    if not BASE_OK.match(base):
        refusals.append('base is %s; only main and release/v* land' % base)
    if status in ('BEHIND', 'DIRTY'):
        refusals.append('mergeStateStatus is %s: merge origin/%s into the branch' % (status, base))
    elif status not in ('CLEAN', 'UNKNOWN', 'BLOCKED', 'UNSTABLE'):
        refusals.append('mergeStateStatus is %s, which this gate does not know; it needs CLEAN' % status)
    odd = [c for c in checks if c['bucket'] not in KNOWN_BUCKETS]
    if odd:
        refusals.append('unrecognised check bucket on %s: %s' % (names_of(odd), ', '.join(sorted({c['bucket'] for c in odd}))))
    if refusals:
        return Verdict(EXIT_REFUSED, refusals)
    if sha and head != sha:
        return Verdict(EXIT_MOVED, ['the head is %s, not the asserted %s' % (head, sha)])
    bad = [c for c in checks if c['bucket'] in BAD_BUCKETS]
    if bad:
        return Verdict(EXIT_RED, ['%s (%s)' % (c['name'], c.get('state') or c['bucket']) for c in bad])
    by_name = {}
    for c in checks:
        by_name.setdefault(c['name'], []).append(c)
    skipped = sorted(n for n in required if any(c['bucket'] == 'skipping' for c in by_name.get(n, [])))
    if skipped:
        return Verdict(EXIT_REFUSED, ['required check %s is skipping, not pass' % n for n in skipped])
    waiting = []
    if not checks:
        waiting.append('no checks reported yet')
    pending = [c for c in checks if c['bucket'] == 'pending']
    if pending:
        waiting.append('pending: %s' % names_of(pending))
    missing = sorted(n for n in required if n not in by_name)
    if missing:
        waiting.append('required check not reported: %s' % ', '.join(missing))
    if waiting:
        return Verdict(EXIT_TIMEOUT, waiting)
    if not required and identities(checks) != previous:
        return Verdict(EXIT_TIMEOUT, ['no required checks: waiting for the same check list on two polls'], lag=True)
    if status == 'UNKNOWN':
        return Verdict(EXIT_TIMEOUT, ['mergeStateStatus is UNKNOWN: GitHub is still computing it'], lag=True)
    if status != 'CLEAN':       # BLOCKED or UNSTABLE: the rows say green, GitHub has not caught up; never green
        return Verdict(EXIT_TIMEOUT, ['mergeStateStatus is %s although every check passed' % status], lag=True)
    skipping = sum(1 for c in checks if c['bucket'] == 'skipping')
    summary = '%d checks, %d pass, %d skipping' % (len(checks), len(checks) - skipping, skipping)
    if required:
        summary += '; required: %s' % ', '.join(sorted(required))
    return Verdict(EXIT_OK, [summary])


class Poll:
    """Reads the pull request and judges it, once per call; keeps what the next judgment needs."""

    def __init__(self, number, sha):
        self.number, self.sha, self.rules, self.previous = number, sha, {}, None

    def once(self):
        view = read_pr(self.number, VIEW_FIELDS)
        checks = read_checks(self.number)
        base = view['baseRefName']
        if base not in self.rules:
            self.rules[base] = read_required(base)
        verdict = judge(view, checks, self.rules[base], self.sha, self.previous)
        self.previous = identities(checks)
        verdict.view = view
        return verdict


def settle(poll, env, timeout, lag_only=False):
    """Poll until the verdict is final or `timeout` seconds have passed; a verdict still at
    EXIT_TIMEOUT is the answer then. `lag_only` stops at the first not-yet that is not GitHub lag."""
    deadline = env.now() + timeout
    interval, errors, last = POLL_FIRST, 0, None
    while True:
        try:
            verdict = poll.once()
            errors = 0
        except Refuse as exc:
            return Verdict(EXIT_REFUSED, [str(exc)])
        except GhError as exc:
            errors += 1
            if errors >= MAX_GH_ERRORS:
                return Verdict(EXIT_REFUSED, ['gh failed %d times in a row: %s' % (errors, exc)])
            verdict = Verdict(EXIT_TIMEOUT, ['gh failed: %s' % exc], lag=True)
        if verdict.code != EXIT_TIMEOUT:
            return verdict
        remaining = deadline - env.now()
        if remaining <= 0 or (lag_only and not verdict.lag):
            return verdict
        if verdict.reasons != last:
            say('waiting: %s' % '; '.join(verdict.reasons))
            last = verdict.reasons
        env.sleep(min(interval, remaining))
        interval = min(interval * POLL_GROWTH, POLL_MAX)


def report(number, verdict):
    where = ''
    if verdict.view:
        where = ' (%s, head %s)' % (verdict.view['baseRefName'], verdict.view['headRefOid'])
    say('%s #%d%s: %s' % (NAMES[verdict.code], number, where, '; '.join(verdict.reasons)))
    return verdict.code


class Scan:
    """outbound-guard, imported in-process: the same secrets and shape rules the hook applies to a
    command line, applied to the text this script sends on its own."""

    def __init__(self, root):
        path = Path(root) / '.claude' / 'hooks' / 'outbound-guard.py'
        try:
            spec = importlib.util.spec_from_file_location('outbound_guard', path)
            guard = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(guard)
            for name in ('load_secrets', 'default_published', 'findings'):
                if not callable(getattr(guard, name, None)):
                    raise AttributeError('no %s()' % name)
            self.guard = guard
            self.secrets = guard.load_secrets(str(root))
            self.published = guard.default_published(str(root))
        except Exception as exc:        # fail closed: with no scan there is no merge
            raise Refuse('cannot load %s (%s); refusing, since nothing would scan the merge text' % (path, exc))

    def hits(self, label, text):
        return ['%s: %s' % (label, what) for what, _how in self.guard.findings(text, self.secrets, self.published)]


def refuse_secrets(number, hits):
    say('REFUSED #%d: the merge text would publish a private value (outbound-guard); no merge was sent' % number,
        sys.stderr)
    for hit in hits:
        say('  %s' % hit, sys.stderr)
    say('  Use a stand-in (docs/shared-servers.md); the value itself is not printed.', sys.stderr)
    return EXIT_REFUSED


def print_check_runs(oid):
    try:
        runs = gh_json('api', 'repos/{owner}/{repo}/commits/%s/check-runs?per_page=100' % oid)['check_runs']
        if not isinstance(runs, list):
            raise TypeError('check_runs is not a list')
    except (GhError, KeyError, TypeError) as exc:
        say('check-runs of %s unavailable: %s' % (oid[:12], exc))
        return
    say('check-runs on %s so far (%d):' % (oid[:12], len(runs)))
    for run in runs:        # the merge is done: a malformed entry is noted, never a traceback
        name = run.get('name') if isinstance(run, dict) else None
        if not name:
            say('  (skipped a check-run entry without a name)', sys.stderr)
            continue
        say('  %s: %s' % (name, run.get('conclusion') or run.get('status') or 'unknown'))


def git(*args):
    return subprocess.run(['git', *args], capture_output=True, text=True, stdin=subprocess.DEVNULL)


def verify(number, env):
    """After a merge: the PR is MERGED and its merge commit is on origin/<base>. Exit 1 if not, and
    every line of that says the merge itself went through, since exit 1 also means "a check is red"."""
    def failed(why):
        say('VERIFY FAILED #%d: gh pr merge succeeded; %s' % (number, why))
        return EXIT_RED

    try:
        for attempt in range(VERIFY_TRIES):
            doc = read_pr(number, 'state,mergeCommit,baseRefName')
            commit = doc['mergeCommit']
            oid = commit.get('oid') if isinstance(commit, dict) else None
            if doc['state'] == 'MERGED' and isinstance(oid, str) and oid:
                break
            if attempt < VERIFY_TRIES - 1:
                env.sleep(VERIFY_GAP)
        else:
            if doc['state'] == 'MERGED':
                return failed('the pull request is MERGED but gh reports no merge commit for it')
            return failed('the pull request is %s, not MERGED' % doc['state'])
    except (GhError, Refuse) as exc:
        return failed('could not read the merged pull request: %s' % exc)
    base = doc['baseRefName']
    fetched = git('fetch', 'origin', '+refs/heads/%s:refs/remotes/origin/%s' % (base, base))
    if fetched.returncode != 0:
        return failed('git fetch origin %s failed (%s); is this a checkout of the repository?'
                      % (base, first_line(fetched.stderr)))
    ancestor = git('merge-base', '--is-ancestor', oid, 'origin/' + base)
    if ancestor.returncode != 0:
        return failed('merge commit %s is not an ancestor of origin/%s' % (oid, base))
    say('MERGED #%d as %s on origin/%s' % (number, oid, base))
    print_check_runs(oid)
    return EXIT_OK


def land(number, sha, subject, body, env):
    """`gh pr merge`, retrying only the head race, then the verify."""
    retries = 0
    while True:
        try:
            done = gh('pr', 'merge', str(number), '--squash', '--match-head-commit', sha,
                      '--subject', subject, '--body-file', '-', input=body)
        except GhError as exc:      # it may have gone through: do not retry, and say so
            say('REFUSED #%d: gh pr merge did not finish (%s); look at #%d before merging again'
                % (number, exc, number))
            return EXIT_REFUSED
        if done.returncode == 0:
            break
        said = (done.stderr + done.stdout).strip()
        if not HEAD_RACE.search(said):
            say('REFUSED #%d: gh pr merge failed: %s' % (number, first_line(said)))
            return EXIT_REFUSED
        if retries == RACE_RETRIES:     # every re-read showed the head at S, so the PR is not "not at S"
            say('REFUSED #%d: gh still says the head differs after %d retries: %s'
                % (number, RACE_RETRIES, first_line(said)))
            return EXIT_REFUSED
        retries += 1
        env.sleep(RACE_WAIT)
        now = read_pr(number, 'headRefOid,state')
        if now['state'] == 'MERGED':        # the answer was lost, the merge was not
            break
        if now['headRefOid'] != sha:
            say('HEAD MOVED #%d: the head is %s, not the asserted %s' % (number, now['headRefOid'], sha))
            return EXIT_MOVED
    return verify(number, env)


def cmd_gate(args, env):
    return report(args.number, settle(Poll(args.number, args.sha), env, SETTLE_SECONDS, lag_only=True))


def cmd_wait(args, env):
    return report(args.number, settle(Poll(args.number, args.sha), env, args.timeout))


def cmd_merge(args, env):
    scan = Scan(env.root)
    hits = scan.hits('subject', args.subject)
    if args.body is not None:
        hits += scan.hits('body', args.body)
    if hits:
        return refuse_secrets(args.number, hits)
    verdict = settle(Poll(args.number, args.sha), env, args.timeout)
    if verdict.code != EXIT_OK:
        return report(args.number, verdict)
    report(args.number, verdict)
    body = args.body
    if args.body_from_pr:
        body = read_pr(args.number, 'body')['body'] or ''
        hits = scan.hits('pull request body', body)
        if hits:
            return refuse_secrets(args.number, hits)
        if not body.strip():
            say('REFUSED #%d: the pull request body is empty; write a --body-file' % args.number)
            return EXIT_REFUSED
    return land(args.number, args.sha, args.subject, body, env)


def restack_steps(number, head, squash, base):
    """The four merges of .agents/skills/land-pr/SKILL.md ("Stacked PRs: restack") as (name, git
    arguments after `-C W`), in the order they run."""
    return [
        ("merge the lower PR's final head",
         ['merge', '--no-edit', '-m', "Merge the final head of #%d (%s)" % (number, head[:9]), head]),
        ('merge the first parent of the squash',
         ['merge', '--no-edit', '-m', 'Merge %s as it was just before #%d squashed' % (base, number), squash + '^']),
        ('record the squash as merged, keeping this tree',    # --no-ff: `-s ours` must never fast-forward
         ['merge', '--no-edit', '--no-ff', '-s', 'ours', '-m',
          'Record #%d (%s) as merged, keeping this tree' % (number, squash[:9]), squash]),
        ('merge origin/' + base,
         ['merge', '--no-edit', '-m', 'Merge origin/%s after #%d' % (base, number), 'origin/' + base]),
    ]


def describe(worktree, n, total, name, argv):
    return 'restack step %d of %d, %s: %s' % (n, total, name, shlex.join(['git', '-C', worktree, *argv]))


UNFINISHED = (('MERGE_HEAD', 'merge'), ('CHERRY_PICK_HEAD', 'cherry-pick'), ('REVERT_HEAD', 'revert'))


def checked_out(worktree, upper):
    """Refuse unless `worktree` is a clean checkout on `upper` with no merge, cherry-pick or revert
    in progress, looking without taking any lock."""
    status = git('--no-optional-locks', '-C', worktree, 'status', '--porcelain')
    if status.returncode != 0:
        raise Refuse('%s is not a git checkout (%s)' % (worktree, first_line(status.stderr)))
    for pseudo, what in UNFINISHED:           # a resolved, staged merge has nothing left for `status` to show
        if git('-C', worktree, 'rev-parse', '-q', '--verify', pseudo).returncode == 0:
            raise Refuse('a %s is in progress in %s; finish it with `git commit` first (or abort it)'
                         % (what, worktree))
    if status.stdout.strip():
        raise Refuse('%s is not clean (an untracked file counts); commit it or move it first' % worktree)
    on = git('-C', worktree, 'branch', '--show-current').stdout.strip()
    if on != upper:
        raise Refuse('%s is not on %s (it is on %s)' % (worktree, upper, on or 'a detached HEAD'))


def push_refspec(worktree, upper):
    """What `git push origin` takes to update `upper`'s pull request: the branch it tracks on origin (a
    checkout made as `git checkout -B tmp origin/x` is named tmp, and its pull request is x), else its
    own name. An upstream on any other remote is refused: a plain push would put the branch into origin."""
    listed = git('-C', worktree, 'for-each-ref', '--format=%(upstream:remotename) %(upstream:remoteref)',
                 'refs/heads/' + upper)
    if listed.returncode != 0:
        raise Refuse('cannot read the upstream of %s (%s)' % (upper, first_line(listed.stderr)))
    parts = listed.stdout.split()
    if not parts or parts[0] == '.':          # no upstream, or another local branch: origin has no say
        return upper
    remote, ref = (parts + [''])[:2]
    if remote != 'origin' or not ref.startswith('refs/heads/'):
        raise Refuse('--push goes to origin, but the upstream of %s is %s on %s; push it there yourself'
                     % (upper, ref or 'unknown', remote))
    return upper if ref == 'refs/heads/' + upper else '%s:%s' % (upper, ref)


def fetch_lower(worktree, number, head, squash, base):
    """Bring in what the four merges need, then refuse unless it is what `gh` described."""
    for refspec in ('refs/pull/%d/head' % number, '+refs/heads/%s:refs/remotes/origin/%s' % (base, base)):
        fetched = git('-C', worktree, 'fetch', 'origin', refspec)
        if fetched.returncode != 0:
            raise Refuse('git fetch origin %s failed (%s)' % (refspec, first_line(fetched.stderr)))
    for oid in (head, squash):
        if git('-C', worktree, 'cat-file', '-e', oid + '^{commit}').returncode != 0:
            raise Refuse('commit %s is not in %s after the fetch' % (oid, worktree))
    if git('-C', worktree, 'merge-base', '--is-ancestor', squash, 'origin/' + base).returncode != 0:
        raise Refuse('merge commit %s is not an ancestor of origin/%s' % (squash, base))
    parents = git('-C', worktree, 'rev-list', '--parents', '-n', '1', squash).stdout.split()[1:]
    if len(parents) != 1:
        raise Refuse('merge commit %s has %d parents; only a squash commit has one' % (squash, len(parents)))


def stopped(worktree, n, total, name, done):
    """A merge step failed: say where, and what state git left the tree in."""
    midway = git('-C', worktree, 'rev-parse', '-q', '--verify', 'MERGE_HEAD').returncode == 0
    for line in (done.stdout + done.stderr).strip().splitlines()[:10]:
        say('  ' + line)
    where = 'restack step %d of %d (%s)' % (n, total, name)
    if midway:
        say('CONFLICT at %s: the merge is in progress in %s, as git left it. Resolve it and commit, then '
            'run this command again; the steps already done are no-ops. Nothing was pushed.'
            % (where, worktree))
    else:
        say('FAILED at %s: git exited %d and the tree is as git left it. Nothing was pushed.'
            % (where, done.returncode))
    return EXIT_RED


def rejection(stderr):
    """The lines of a failed `git push` that give the reason: `To <url>` comes first and says nothing."""
    keep = [x.strip() for x in stderr.splitlines() if x.strip().startswith(('!', 'error:', 'remote:', 'fatal:'))]
    return keep[:10] or [first_line(stderr) or 'git push printed nothing']


def cmd_restack(args, env):
    number, worktree, upper = args.number, str(args.worktree), args.upper
    doc = read_pr(number, 'state,headRefOid,mergeCommit,baseRefName')
    commit = doc['mergeCommit']
    squash = commit.get('oid') if isinstance(commit, dict) else None
    if doc['state'] != 'MERGED':
        raise Refuse('the pull request is %s, not MERGED; restack runs after the one below has squash-merged'
                     % doc['state'])
    if not (isinstance(squash, str) and SHA.match(squash)):
        raise Refuse('the pull request is MERGED but gh reports no merge commit for it')
    head, base = doc['headRefOid'], doc['baseRefName']
    if not (isinstance(head, str) and SHA.match(head)) or not isinstance(base, str) or not base:
        raise Refuse('gh pr view answered with a final head or base this script does not know')
    if upper == base:
        raise Refuse('--upper %s is the base branch of #%d; restack re-bases a branch stacked on it, and '
                     'this would merge the lower PR into %s' % (upper, number, base))
    checked_out(worktree, upper)
    refspec = push_refspec(worktree, upper) if args.push else upper
    steps = restack_steps(number, head, squash, base)
    push = shlex.join(['git', '-C', worktree, 'push', 'origin', refspec])
    if args.dry_run:
        say('DRY RUN #%d: #%d is MERGED as %s with final head %s; %s is clean and on %s. Nothing is '
            'fetched, merged or pushed. It would run:' % (number, number, squash, head, worktree, upper))
        for n, (name, argv) in enumerate(steps, 1):
            say(describe(worktree, n, len(steps), name, argv))
        say('and then, with --push: ' + push if args.push else 'and stop there, without --push')
        return EXIT_OK
    fetch_lower(worktree, number, head, squash, base)
    for n, (name, argv) in enumerate(steps, 1):
        say(describe(worktree, n, len(steps), name, argv))
        done = git('-C', worktree, *argv)
        if done.returncode != 0:
            return stopped(worktree, n, len(steps), name, done)
        say('  ' + (first_line(done.stdout) or 'done'))
    tip = git('-C', worktree, 'rev-parse', 'HEAD').stdout.strip()
    say('RESTACKED %s on #%d: %s is %s' % (upper, number, upper, tip))
    if not args.push:
        say('not pushed: run it again with --push')
        return EXIT_OK
    say(push)
    pushed = git('-C', worktree, 'push', 'origin', refspec)
    if pushed.returncode != 0:
        for line in rejection(pushed.stderr):
            say('  ' + line)
        say('PUSH FAILED: the merges are done locally and nothing was forced')
        return EXIT_RED
    say('PUSHED %s' % upper)
    return EXIT_OK


COMMANDS = {'gate': cmd_gate, 'wait': cmd_wait, 'merge': cmd_merge, 'restack': cmd_restack}


def duration(text):
    found = re.fullmatch(r'(\d+)([smh]?)', text.strip())
    if not found:
        raise argparse.ArgumentTypeError('%r is not a duration like 90s, 45m or 2h' % text)
    return int(found.group(1)) * {'': 1, 's': 1, 'm': 60, 'h': 3600}[found.group(2)]


def full_sha(text):
    if not SHA.match(text):
        raise argparse.ArgumentTypeError('%r is not a full 40-character head SHA' % text)
    return text


def branch_name(text):
    if not BRANCH.match(text):
        raise argparse.ArgumentTypeError('%r is not a plain branch name' % text)
    return text


def build_parser():
    parser = argparse.ArgumentParser(prog='pr-land.py', description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='cmd', required=True, metavar='{gate,wait,merge,restack}')
    subs = {}
    for name, summary in (('gate', 'read-only verdict'), ('wait', 'poll until the gate is green'),
                          ('merge', 'wait, squash-merge at --sha, verify')):
        one = sub.add_parser(name, help=summary, description=summary)
        one.add_argument('number', type=int, help='pull request number')
        one.add_argument('--sha', type=full_sha, required=name != 'gate',
                         help='the head commit the verdict is about (the full 40 characters)')
        if name != 'gate':
            one.add_argument('--timeout', type=duration, default=duration('45m'),
                             help='how long to poll before exit 4 (90s, 45m, 2h; default 45m)')
        subs[name] = one
    restack = sub.add_parser('restack', help='re-base a stacked branch by merges after the PR below squashed',
                             description='re-base a stacked branch by merges after the PR below it squashed')
    restack.add_argument('--worktree', required=True, type=Path, help='the checkout of the stacked branch')
    restack.add_argument('--upper', required=True, type=branch_name, help='the stacked branch, checked out in --worktree')
    restack.add_argument('--lower', dest='number', required=True, type=int, metavar='PR',
                         help='the pull request below it, already squash-merged')   # `number`: what main() names on a refusal
    restack.add_argument('--dry-run', action='store_true', help='print the four merges, change nothing')
    restack.add_argument('--push', action='store_true', help='then push the stacked branch (never forced)')
    merge = subs['merge']
    merge.add_argument('--subject', required=True, help='the squash commit subject, e.g. "Title (#N)"')
    body = merge.add_mutually_exclusive_group(required=True)
    body.add_argument('--body-file', help='the squash commit body, from this file')
    body.add_argument('--body-from-pr', action='store_true', help="send the pull request's own body instead")
    return parser, subs


def read_body(args, merge):
    """Usage checks that need more than argparse: a one-line subject, a readable and non-blank body."""
    args.subject = args.subject.strip()
    if not args.subject or '\n' in args.subject:
        merge.error('--subject must be one non-empty line')
    args.body = None
    if args.body_file:
        try:
            args.body = Path(args.body_file).read_text(encoding='utf-8')
        except (OSError, UnicodeDecodeError) as exc:
            merge.error('cannot read --body-file: %s' % exc)
        if not args.body.strip():
            merge.error('--body-file is empty; a merge body is required')


def main(argv=None, sleep=time.sleep, now=time.monotonic, root=ROOT):
    parser, subs = build_parser()
    try:
        args = parser.parse_args(argv)
        if args.cmd == 'merge':
            read_body(args, subs['merge'])
    except SystemExit as exc:           # argparse: 2 for a usage error, 0 for --help
        return exc.code if isinstance(exc.code, int) else EXIT_OK if exc.code is None else EXIT_USAGE
    try:
        return COMMANDS[args.cmd](args, Env(sleep, now, root))
    except (GhError, Refuse) as exc:
        say('REFUSED #%d: %s' % (args.number, exc))
        return EXIT_REFUSED


if __name__ == '__main__':
    sys.exit(main())
