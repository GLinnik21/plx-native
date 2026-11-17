#!/usr/bin/env python3
"""Grade the `pr-reviewer` agent against past pull requests whose outcome is known.

    tools/review-eval.py list [--split tune|test]
    tools/review-eval.py check
    tools/review-eval.py materialize ID --into DIR
    tools/review-eval.py grade-prompt ID --report FILE
    tools/review-eval.py score VERDICTS.json [--split tune|test]

Nothing else says whether the reviewer finds real bugs, or whether an edit to its prompt or to a rule
made it better. `tools/review-eval/cases.json` holds the cases, each one squash commit on `main`:

  * `bug`: a pull request that merged with a defect a later commit fixed. `key` is the one-sentence
    answer, taken from the fix's own message (`fixed_by`). The reviewer earns the case only with a
    finding that describes THAT defect; any other finding, however true, earns nothing.
  * `control`: a pull request of similar size that no later commit names as the cause of anything.
    A BLOCKER or MAJOR on a control counts against the reviewer, which is what stops a prompt that
    flags everything from scoring well.

`split` keeps two sets apart. Tune the prompt and the rules against `tune` and read `test` only to
decide whether a change is kept: a gain on `tune` that `test` does not show is overfitting.

A run, per case:

  1. `materialize ID --into DIR` rebuilds the repository as the reviewer would have met it: a clone
     that shares this checkout's objects and holds no ref but `base` (the squash's parent) and `head`
     (the squash) and no reflog, so no branch, tag or log leads to the fix. It prints the task to hand the reviewer.
     The answer key is never printed by this command.
  2. Start `pr-reviewer` with that task and save its report.
  3. `grade-prompt ID --report FILE` prints the grading task (the key and the report) for a second
     reader, a model or a person, who answers with the case's verdict as JSON.
  4. Collect the verdicts as `{"<id>": {"caught": true|false, "blockers": n, "majors": n, "minors": n}}`
     (`caught` only on a bug) and run `score`.

`score` prints recall on the bugs, the share of controls that drew a BLOCKER or MAJOR, and the findings
per case; it exits 1 when a case of the chosen split has no verdict, since a partial run is not a score.
The reviewer under test is the working tree's `.agents/agents/pr-reviewer.md` with the working tree's
rule files, so the task tells it to read its rules HERE and the code in DIR.

`check` validates cases.json: the fields, unique ids (`pr-N`), both kinds in both splits, a one-sentence
key, and, for every commit this clone has (a shallow CI checkout has few), that it is a one-parent commit
whose subject ends `(#N)` and that a bug's fix came after it. Add a case when a fix names the pull request that caused the bug
and the cause is readable in that pull request's diff; never edit a key to fit a report.
"""
import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CASES = ROOT / 'tools' / 'review-eval' / 'cases.json'
SPLITS = ('tune', 'test')
KINDS = ('bug', 'control')
FIELDS = {'bug': {'id', 'kind', 'split', 'commit', 'fixed_by', 'key'},
          'control': {'id', 'kind', 'split', 'commit'}}
LEVELS = ('blockers', 'majors', 'minors')


def git(cwd, *args):
    return subprocess.run(['git', '-C', str(cwd), *args], capture_output=True, text=True,
                          stdin=subprocess.DEVNULL)


def is_sha(value):
    return isinstance(value, str) and len(value) == 40 and all(c in '0123456789abcdef' for c in value)


def load(path=CASES):
    return json.loads(Path(path).read_text(encoding='utf-8'))['cases']


def problems(cases, repo=None):
    """Every way the case list is wrong, as strings. With `repo`, commits that clone has are checked too."""
    found, seen = [], set()
    for n, case in enumerate(cases):
        name = case.get('id') if isinstance(case, dict) else None
        if not isinstance(name, str) or not name:
            found.append('case %d has no id' % n)
            continue
        if name in seen:
            found.append('%s: the id is used twice' % name)
        seen.add(name)
        kind = case.get('kind')
        if kind not in KINDS:
            found.append('%s: kind is %r, not one of %s' % (name, kind, ', '.join(KINDS)))
            continue
        if set(case) != FIELDS[kind]:
            found.append('%s: a %s has exactly the fields %s' % (name, kind, ', '.join(sorted(FIELDS[kind]))))
            continue
        if case['split'] not in SPLITS:
            found.append('%s: split is %r, not one of %s' % (name, case['split'], ', '.join(SPLITS)))
        for field in ('commit', 'fixed_by'):
            if field in case and not is_sha(case[field]):
                found.append('%s: %s is not a full commit id' % (name, field))
        if kind == 'bug':
            key = case['key']
            if not isinstance(key, str) or len(key) < 40 or '\n' in key or not key.endswith('.'):
                found.append('%s: the key is one sentence that names the defect' % name)
        if repo is not None and is_sha(case['commit']) and has(repo, case['commit']):
            subject = git(repo, 'log', '-1', '--format=%s', case['commit']).stdout.strip()
            if not subject.endswith('(#%s)' % name[3:]):    # a fix's BODY names the PR it fixes, too
                found.append('%s: the commit is not that pull request\'s squash (its subject is %r)' % (name, subject))
            if len(git(repo, 'rev-list', '--parents', '-n', '1', case['commit']).stdout.split()) != 2:
                found.append('%s: the commit is not a one-parent (squash) commit' % name)
            fix = case.get('fixed_by')
            if is_sha(fix) and has(repo, fix) and \
                    git(repo, 'merge-base', '--is-ancestor', case['commit'], fix).returncode != 0:
                found.append('%s: fixed_by does not come after the commit' % name)
    for split in SPLITS:
        for kind in KINDS:
            if not any(isinstance(c, dict) and c.get('split') == split and c.get('kind') == kind for c in cases):
                found.append('the %s split has no %s case' % (split, kind))
    return found


def has(repo, sha):
    return git(repo, 'cat-file', '-e', sha + '^{commit}').returncode == 0


def find(cases, name):
    for case in cases:
        if case['id'] == name:
            return case
    sys.exit('no case %s; `list` names them' % name)


def materialize(case, into, repo=ROOT):
    """The repository at review time, in `into`: no ref but `base` and `head`, so nothing leads to the fix."""
    into = Path(into)
    if into.exists():
        sys.exit('%s exists; give a path that does not' % into)
    steps = [(repo, 'clone', '--quiet', '--shared', '--no-checkout', str(repo), str(into))]
    for step in steps:
        done = git(*step)
        if done.returncode != 0:
            sys.exit('git %s failed: %s' % (step[1], done.stderr.strip()))
    git(into, 'remote', 'remove', 'origin')
    for ref in git(into, 'for-each-ref', '--format=%(refname)').stdout.split():
        git(into, 'update-ref', '-d', ref)
    for step in (('update-ref', 'refs/heads/base', case['commit'] + '^'),
                 ('update-ref', 'refs/heads/head', case['commit']),
                 ('checkout', '--quiet', 'head')):
        done = git(into, *step)
        if done.returncode != 0:
            sys.exit('git %s failed in %s: %s' % (step[0], into, done.stderr.strip()))
    # `git clone` logged the source's HEAD, which has the fix behind it: `git reflog` would lead there.
    shutil.rmtree(into / '.git' / 'logs', ignore_errors=True)
    git(into, 'config', 'core.logAllRefUpdates', 'false')
    left = git(into, 'for-each-ref', '--format=%(refname)').stdout.split()
    if sorted(left) != ['refs/heads/base', 'refs/heads/head']:
        sys.exit('%s still holds other refs: %s' % (into, ' '.join(left)))
    return into


def task(into, rules=ROOT):
    return ('Review the range `base...head` in the repository at %(dir)s (run git as `git -C %(dir)s`; '
            'the checkout there is at `head`).\n'
            'This is a replay of a pull request as it stood when it was opened. Read CODE only in %(dir)s. '
            'Read your RULES (AGENTS.md, the module guides, docs/) from the working tree at %(rules)s, which '
            'is the reviewer under test; this replaces "the rules come from the base" for this run. Open no '
            'file under %(rules)s that is not a rule or guide, and no `tools/review-eval` path anywhere.\n'
            'Your own prompt is the file %(rules)s/.agents/agents/pr-reviewer.md as it is on disk NOW, which may be '
            'newer than the copy you were started with: read it first and follow it where the two differ.\n'
            'Report as your prompt says.' % {'dir': into, 'rules': rules})


def grade_task(case, report):
    asks = ('Does any finding describe the defect in the KEY (the same cause in the same code, in any words)? '
            'A finding about something else earns nothing, and neither does a `Recommend:` line.\n'
            'KEY: %s\n' % case['key']) if case['kind'] == 'bug' else \
        'This pull request is a control: no defect is known. Only count the findings.\n'
    shape = '{"caught": true|false, "blockers": n, "majors": n, "minors": n}' if case['kind'] == 'bug' else \
        '{"blockers": n, "majors": n, "minors": n}'
    return ('Grade one code-review report. Count its findings by severity.\n%sAnswer with JSON only: %s\n\n'
            '--- REPORT ---\n%s' % (asks, shape, report))


def score(cases, verdicts, split=None):
    """(lines, complete): the table, and whether every case of the split had a well-formed verdict."""
    chosen = [c for c in cases if split in (None, c['split'])]
    lines, complete = [], True
    for kind in KINDS:
        rows = [c for c in chosen if c['kind'] == kind]
        hit = findings = graded = 0
        for case in rows:
            verdict = verdicts.get(case['id'])
            ok = isinstance(verdict, dict) and all(type(verdict.get(k)) is int and verdict[k] >= 0 for k in LEVELS) \
                and (kind == 'control' or type(verdict.get('caught')) is bool)
            if not ok:
                lines.append('NO VERDICT %s' % case['id'])
                complete = False
                continue
            graded += 1
            findings += sum(verdict[k] for k in LEVELS)
            flagged = verdict['caught'] if kind == 'bug' else verdict['blockers'] + verdict['majors'] > 0
            hit += flagged
            lines.append('%-8s %-5s %-9s %s' % (case['id'], case['split'], kind,
                                              ('caught' if flagged else 'MISSED') if kind == 'bug'
                                              else ('FLAGGED' if flagged else 'clean')))
        if graded:
            what = 'recall (bugs caught)' if kind == 'bug' else 'controls flagged BLOCKER/MAJOR'
            lines.append('%s: %d/%d = %.0f%%; %.1f findings per case' % (what, hit, graded, 100.0 * hit / graded,
                                                                      findings / graded))
    return lines, complete


def main(argv=None):
    parser = argparse.ArgumentParser(prog='review-eval.py', description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='cmd', required=True)
    for name in ('list', 'score'):
        one = sub.add_parser(name)
        one.add_argument('--split', choices=SPLITS)
        if name == 'score':
            one.add_argument('verdicts', type=Path)
    sub.add_parser('check')
    one = sub.add_parser('materialize')
    one.add_argument('id')
    one.add_argument('--into', required=True, type=Path)
    one = sub.add_parser('grade-prompt')
    one.add_argument('id')
    one.add_argument('--report', required=True, type=Path)
    args = parser.parse_args(argv)
    cases = load()
    if args.cmd == 'check':
        found = problems(cases, ROOT)
        for line in found:
            print('review-eval: %s' % line, file=sys.stderr)
        print('review-eval: %d cases, %d problem(s)' % (len(cases), len(found)))
        return 1 if found else 0
    if args.cmd == 'list':
        for case in cases:
            if args.split in (None, case['split']):
                print('%-8s %-5s %-8s %s' % (case['id'], case['split'], case['kind'], case['commit'][:12]))
        return 0
    if args.cmd == 'materialize':
        print(task(materialize(find(cases, args.id), args.into.resolve())))
        return 0
    if args.cmd == 'grade-prompt':
        print(grade_task(find(cases, args.id), args.report.read_text(encoding='utf-8')))
        return 0
    lines, complete = score(cases, json.loads(args.verdicts.read_text(encoding='utf-8')), args.split)
    print('\n'.join(lines))
    return 0 if complete else 1


if __name__ == '__main__':
    sys.exit(main())
