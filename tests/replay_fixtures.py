#!/usr/bin/env python3
"""Replay every committed product recording through a built simulator, in both modes.

The simulator owns parsing, controlled bootstrap, recorded ingress, and every frame grade.
macOS additionally denies outbound networking with sandbox-exec. Evidence roots are retained.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

DIFFS = ('diverged', 'present_diffs', 'input_diffs', 'result_diffs', 'land_diffs',
         'effect_diffs', 'focus_diffs', 'hit_diffs')


def discover(fixtures):
    recordings = []
    for path in sorted(fixtures.iterdir()):
        if not path.is_dir():
            continue
        if not (path / 'manifest.json').is_file():
            raise ValueError(f'{path.name}: recording directory has no manifest')
        expected_counts(path)
        recordings.append(path)
    if not recordings:
        raise ValueError('no committed recordings found')
    return recordings


def expected_counts(fixture):
    ticks, grades = set(), set()
    segments = sorted(fixture.glob('rec-*.jsonl'))
    if not segments:
        raise ValueError(f'{fixture.name}: no recording segments')
    for segment in segments:
        for line in segment.read_text().splitlines():
            row = json.loads(line)
            if row.get('t') == 'tick':
                ticks.add(row['f'])
            elif row.get('t') == 'st':
                grades.add(row['f'])
    if not ticks or not grades:
        raise ValueError(f'{fixture.name}: empty frame/grade ledger')
    return len(ticks), len(grades)


def verdict(log, returncode, counts):
    lines = log.splitlines()
    summaries = [line for line in lines if line.startswith('replay: done ')]
    if len(summaries) != 1:
        raise ValueError('simulator did not complete exactly one replay')
    if returncode != 0:
        raise ValueError(f'simulator exited with status {returncode}: {summaries[0]}')
    if any(line.startswith(('replay: REFUSED', 'rec: REFUSED')) for line in lines):
        raise ValueError('simulator refused controlled replay')
    pairs = re.findall(r'(\w+)=(\w+)', summaries[0])
    fields = dict(pairs)
    if len(fields) != len(pairs) or fields.get('verdict') != 'SAME':
        raise ValueError('replay summary is ambiguous or diverged')
    if any(fields.get(field) != '0' for field in DIFFS):
        raise ValueError('replay summary has a missing or nonzero difference counter')
    if (fields.get('frames'), fields.get('graded')) != tuple(map(str, counts)):
        raise ValueError('replay did not grade the complete committed recording')
    return summaries[0]


def run_fixture(binary, assets, fixture, mode, output, timeout):
    runtime = Path(tempfile.mkdtemp(prefix=fixture.name + '-' + mode + '-', dir=output))
    (runtime / 'plxnative-recplay').write_text('v1\n' + mode + '\n' + str(fixture))
    env = dict(os.environ, PLXNATIVE_RUNTIME_DIR=str(runtime), PLXNATIVE_APP_DIR=str(assets),
               PLXNATIVE_WIN='1920x1080')
    command = [str(binary), '127.0.0.1', '9']
    if sys.platform == 'darwin':
        command = ['/usr/bin/sandbox-exec', '-p',
                   '(version 1)(allow default)(deny network-outbound)'] + command
    with (runtime / 'sim.out').open('wb') as out:
        try:
            process = subprocess.run(command, env=env, stdout=out, stderr=subprocess.STDOUT,
                                     timeout=timeout, check=False)
        except subprocess.TimeoutExpired as error:
            raise ValueError(f'{fixture.name}/{mode}: timeout; evidence {runtime}') from error
    log = runtime / 'plxnative-events.log'
    try:
        summary = verdict(log.read_text() if log.exists() else '', process.returncode,
                          expected_counts(fixture))
    except ValueError as error:
        raise ValueError(f'{fixture.name}/{mode}: {error}; evidence {runtime}') from error
    print(f'{fixture.name}/{mode}: {summary}', flush=True)


def main():
    repo = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sim', type=Path, required=True)
    parser.add_argument('--fixtures', type=Path, default=repo / 'tests/fixtures/replay')
    parser.add_argument('--assets', type=Path, default=repo / 'pkg')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--timeout', type=float, default=300)
    args = parser.parse_args()
    try:
        fixtures = discover(args.fixtures.resolve())
    except (ValueError, OSError) as error:
        parser.error(str(error))
    if args.timeout <= 0:
        parser.error('--timeout must be positive')
    output = args.output or Path(tempfile.mkdtemp(prefix='plxnative-replay-gate-'))
    output.mkdir(parents=True, exist_ok=True)
    failures = []
    for fixture in fixtures:
        for mode in ('targets', 'resolve'):
            try:
                run_fixture(args.sim.resolve(), args.assets.resolve(), fixture, mode,
                            output.resolve(), args.timeout)
            except (ValueError, OSError) as error:
                print(f'FAIL: {error}', file=sys.stderr, flush=True)
                failures.append(error)
    print(f'{len(fixtures)} fixtures, {len(fixtures) * 2} replays, {len(failures)} failures; '
          f'evidence {output}', flush=True)
    return bool(failures)


if __name__ == '__main__':
    sys.exit(main())
