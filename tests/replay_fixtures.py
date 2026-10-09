#!/usr/bin/env python3
"""Replay every committed product recording through a built simulator, in both modes.

The simulator owns parsing, controlled bootstrap, recorded ingress, and every frame grade.
macOS additionally denies outbound networking with sandbox-exec. Evidence roots are retained.

The replays are independent processes and run side by side (`--jobs`, `PLX_REPLAY_JOBS`; 1 is
serial). What makes that sound: each replay gets its own `PLXNATIVE_RUNTIME_DIR` (triggers, FIFO,
event log, evidence) from `mkdtemp`, reads its recording and the assets read-only, is pointed at
`127.0.0.1:9` with outbound networking denied (so it binds and dials nothing), and is graded on
the recorded clock (`app::clock::set_replay`), never on wall time. Each replay's output is printed
whole when it finishes, so concurrent logs never interleave, and a failing replay never stops the
others.

Each replay opens a 1920x1080 simulator window unless `--window WxH` / `PLX_REPLAY_WINDOW` says
otherwise. The canvas, and everything graded, is 1920x1080 logical either way; a smaller window
only means fewer pixels for a software-GL runner to fill (the macOS CI job uses 960x540).
"""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import functools
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

# Replays are CPU-bound and each one is a full renderer-backed simulator, so the default stops at
# the core count and never past this: more workers than cores only stretch every replay, and the
# per-replay timeout runs from the replay's own start.
MAX_DEFAULT_JOBS = 4
MODES = ('targets', 'resolve')
JOBS_ENV = 'PLX_REPLAY_JOBS'
WINDOW_ENV = 'PLX_REPLAY_WINDOW'
# The window the simulator opens for a replay, `PLXNATIVE_WIN`. The authored canvas is 1920x1080
# logical whatever the window is (`surface::scale`/`viewport` map it; layout, hit resolution and
# the graded hash never see pixels), so this only sets how many pixels a software GL context fills.
DEFAULT_WINDOW = '1920x1080'
MAX_WINDOW_W = 7680

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


def resolve_jobs(flag, environ, cpus):
    """Worker count: `--jobs` wins, then `PLX_REPLAY_JOBS`, then min(cpus, MAX_DEFAULT_JOBS)."""
    if flag is None and environ.get(JOBS_ENV, '') != '':
        try:
            flag = int(environ[JOBS_ENV])
        except ValueError:
            raise ValueError(f'{JOBS_ENV} must be a positive integer, got {environ[JOBS_ENV]!r}')
        source = JOBS_ENV
    else:
        source = '--jobs'
    if flag is None:
        return max(1, min(cpus or 1, MAX_DEFAULT_JOBS))
    if flag < 1:
        raise ValueError(f'{source} must be a positive integer, got {flag}')
    return flag


def parse_window(value):
    """A replay window as `<w>x<h>`, or ValueError.

    Strict on purpose: plain decimal digits around a lowercase `x`, no sign, space or zero, at most
    8K wide, and exactly 16:9. The simulator would accept a looser string (and any positive size),
    but an aspect other than the canvas's letterboxes the viewport, which switches the page-capture
    path from render-into-texture to copy (`FrameCache::render_available`) -- a different render
    than the one the committed recordings were graded against."""
    match = re.fullmatch(r'([1-9][0-9]*)x([1-9][0-9]*)', value) if isinstance(value, str) else None
    if match is None:
        raise ValueError(f'replay window must look like 1920x1080 (digits x digits), got {value!r}')
    width, height = int(match.group(1)), int(match.group(2))
    if width > MAX_WINDOW_W or width * 9 != height * 16:
        raise ValueError(f'replay window must be 16:9 and at most {MAX_WINDOW_W} wide '
                         f'(the canvas is 1920x1080), got {value!r}')
    return value


def resolve_window(flag, environ):
    """Window size: `--window` wins, then `PLX_REPLAY_WINDOW`, then the full-size default."""
    if flag is None and environ.get(WINDOW_ENV, '') != '':
        flag, source = environ[WINDOW_ENV], WINDOW_ENV
    else:
        source = '--window'
    if flag is None:
        return DEFAULT_WINDOW
    try:
        return parse_window(flag)
    except ValueError as error:
        raise ValueError(f'{source}: {error}') from None


def run_fixture(binary, assets, fixture, mode, output, timeout, window=DEFAULT_WINDOW):
    runtime = Path(tempfile.mkdtemp(prefix=fixture.name + '-' + mode + '-', dir=output))
    (runtime / 'plxnative-recplay').write_text('v1\n' + mode + '\n' + str(fixture))
    env = dict(os.environ, PLXNATIVE_RUNTIME_DIR=str(runtime), PLXNATIVE_APP_DIR=str(assets),
               PLXNATIVE_WIN=window)
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
    return f'{fixture.name}/{mode}: {summary}'


def main():
    repo = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sim', type=Path, required=True)
    parser.add_argument('--fixtures', type=Path, default=repo / 'tests/fixtures/replay')
    parser.add_argument('--assets', type=Path, default=repo / 'pkg')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--timeout', type=float, default=300,
                        help='seconds each replay may run, counted from its own start')
    parser.add_argument('--jobs', type=int,
                        help=f'replays to run at once (default: ${JOBS_ENV}, else the CPU count '
                             f'capped at {MAX_DEFAULT_JOBS}; 1 runs them one after another)')
    parser.add_argument('--window', metavar='WxH',
                        help=f'simulator window for every replay (default: ${WINDOW_ENV}, else '
                             f'{DEFAULT_WINDOW}); 16:9 only. The canvas is 1920x1080 logical '
                             'whatever this is, so a smaller one only cuts software-GL fill cost')
    args = parser.parse_args()
    try:
        fixtures = discover(args.fixtures.resolve())
    except (ValueError, OSError) as error:
        parser.error(str(error))
    if args.timeout <= 0:
        parser.error('--timeout must be positive')
    try:
        jobs = resolve_jobs(args.jobs, os.environ, os.cpu_count())
    except ValueError as error:
        parser.error(str(error))
    try:
        window = resolve_window(args.window, os.environ)
    except ValueError as error:
        parser.error(str(error))
    output = args.output or Path(tempfile.mkdtemp(prefix='plxnative-replay-gate-'))
    output.mkdir(parents=True, exist_ok=True)
    failures = run_all(args.sim.resolve(), args.assets.resolve(), fixtures, output.resolve(),
                       args.timeout, jobs, functools.partial(run_fixture, window=window))
    print(f'{len(fixtures)} fixtures, {len(fixtures) * len(MODES)} replays, {len(failures)} failures; '
          f'{jobs} parallel; window {window}; evidence {output}', flush=True)
    return bool(failures)


def replay_order(fixtures):
    """Every (fixture, mode) replay, longest recording first so the last worker is not left
    holding the biggest one. Ties keep discovery order (sorted() is stable)."""
    cost = {fixture: expected_counts(fixture)[0] for fixture in fixtures}
    return sorted(((fixture, mode) for fixture in fixtures for mode in MODES),
                  key=lambda replay: -cost[replay[0]])


def run_all(binary, assets, fixtures, output, timeout, jobs, run=run_fixture):
    """Run every replay with at most `jobs` at once; return the failures.

    A replay's report is printed in one call when it finishes (stdout for a pass, stderr for a
    FAIL), so output stays readable at any width. Nothing short-circuits: all replays run."""
    failures = []

    def one(replay):
        fixture, mode = replay
        began = time.monotonic()
        try:
            report, error = run(binary, assets, fixture, mode, output, timeout), None
        except (ValueError, OSError) as caught:
            report, error = None, caught
        except Exception as caught:  # a crash in one replay must not take the others down
            report, error = None, ValueError(f'{fixture.name}/{mode}: {type(caught).__name__}: {caught}')
        return report, error, time.monotonic() - began

    with ThreadPoolExecutor(max_workers=jobs) as pool:
        for done in as_completed([pool.submit(one, replay) for replay in replay_order(fixtures)]):
            report, error, seconds = done.result()
            if error is None:
                print(f'{report} [{seconds:.0f} s]', flush=True)
            else:
                print(f'FAIL: {error} [{seconds:.0f} s]', file=sys.stderr, flush=True)
                failures.append(error)
    return failures


if __name__ == '__main__':
    sys.exit(main())
