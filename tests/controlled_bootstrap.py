#!/usr/bin/env python3
"""Production controlled-Home gate on macOS. No builds, devices or household network.

The mock is stopped before replay. sandbox-exec denies all outbound networking; a listener
also counts primary-endpoint attempts. Runtime roots/evidence are retained, never cleaned.
Only generated synthetic initial inputs use --write-synthetic-initial. Replay roots never
receive the recording's auth file (the typed recording has no such file).
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import time

REPO = Path(__file__).resolve().parents[1]
CAP_BYTES = 64 * 1024 * 1024


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sim', required=True)
    parser.add_argument('--port', type=int, default=32517)
    parser.add_argument('--capture-only', action='store_true', help='natural capture ordering controls only')
    args = parser.parse_args()
    binary = str(Path(args.sim).resolve())
    sandbox = '/usr/bin/sandbox-exec'
    if not Path(sandbox).is_file():
        raise RuntimeError('this gate requires the macOS outbound-IO sandbox')
    out = Path(tempfile.mkdtemp(prefix='codex-controlled-bootstrap-gate-', dir='/tmp'))
    owned = []
    stopped = threading.Event()
    listener = thread = None
    attempts = []
    results = []

    def publish(value):
        print(json.dumps(value), flush=True)

    def stop(process):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)

    def launch(root, replay=False):
        env = dict(os.environ, PLXNATIVE_RUNTIME_DIR=str(root),
                   PLXNATIVE_APP_DIR=str(REPO / 'pkg'), PLXNATIVE_WIN='1920x1080')
        command = [binary, '127.0.0.1', str(args.port)]
        if replay:
            command = [sandbox, '-p', '(version 1)(allow default)(deny network-outbound)'] + command
        with (root / 'sim.out').open('wb') as log:
            process = subprocess.Popen(command, cwd=REPO, env=env, stdout=log, stderr=subprocess.STDOUT)
        owned.append(process)
        publish({'root': str(root), 'pid': process.pid, 'outbound_denied': replay})
        return process

    def wait_marker(process, path, marker, timeout=25):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if path.exists() and marker in path.read_text(errors='replace'):
                return
            if process.poll() is not None:
                raise RuntimeError('production process exited before its expected boundary')
            time.sleep(.1)
        raise RuntimeError('production boundary deadline exceeded')

    def snapshot(root):
        return {name: (hashlib.sha256((root / name).read_bytes()).hexdigest(), (root / name).stat().st_ino)
                for name in ('auth.json', 'telemetry.json') if (root / name).exists()}

    def ambient(root):
        # Contrasting but wholly invented household-shaped state. Never printed or imported.
        (root / 'auth.json').write_text(json.dumps({
            'client_id': 'UnrelatedSyntheticClient', 'account_token': 'UnrelatedSyntheticToken',
            'server': {'address': '203.0.113.10', 'port': 9, 'origin_url': 'http://203.0.113.10:9',
                       'token': 'UnrelatedSyntheticPmsToken'},
            'user': {'uuid': 'UnrelatedSyntheticProfile', 'title': 'UnrelatedSyntheticName'},
            'playback_quality': 'auto',
        }))
        (root / 'telemetry.json').write_text(json.dumps({
            'asked_version': 4, 'errors': True, 'usage': True,
            'install_id': 'UnrelatedSyntheticInstall', 'errors_id': 'UnrelatedSyntheticErrors',
        }))
        for name, value in [('settings', 'root'), ('consent', 'product'), ('token', 'UnrelatedSyntheticToken'),
                            ('logintest', ''), ('firstrun', ''), ('playbackquality', 'auto'),
                            ('ffprobe', 'http://203.0.113.10:9')]:
            (root / ('plxnative-' + name)).write_text(value)

    def replay_case(name, recording, expected, with_ambient=False, both=False):
        root = out / name
        root.mkdir()
        (root / 'plxnative-recplay').write_text(str(recording))
        if with_ambient:
            ambient(root)
        if both:
            (root / 'plxnative-rec').touch()
        before = snapshot(root)
        process = launch(root, True)
        code = process.wait(timeout=45)
        lines = (root / 'plxnative-events.log').read_text(errors='replace').splitlines()
        assert not any(line.startswith('ff:') for line in lines), name + ': ambient playback probe/resource boot'
        summaries = [line for line in lines if line.startswith('replay: done ')]
        refused = any(line.startswith(('replay: REFUSED', 'rec: REFUSED')) for line in lines)
        if expected == 'REFUSED':
            assert code != 0 and refused and not summaries, name
            assert not any(line.startswith(('video driver:', 'bootstrap: captured')) for line in lines), name
        else:
            assert summaries and summaries[-1].endswith('verdict=' + expected), name
            if expected == 'SAME':
                assert code == 0, name
                assert all(' %s=0' % field in summaries[-1] for field in
                           ('diverged', 'present_diffs', 'input_diffs', 'result_diffs',
                            'land_diffs', 'effect_diffs')), name
        assert before == snapshot(root), name + ': ambient persistence changed or was created'
        assert not attempts, name + ': primary network attempt'
        result = {'case': name, 'exit': code, 'expected': expected, 'nonmutation': True,
                  'primary_attempts': len(attempts), 'summary': summaries[-1] if summaries else 'preflight refusal'}
        results.append(result)
        publish(result)

    def variant(name, original, mutate):
        target = out / ('input-' + name)
        shutil.copytree(original, target)
        manifest_path = target / 'manifest.json'
        manifest = json.loads(manifest_path.read_text())
        segment = target / 'rec-0000.jsonl'
        rows = [json.loads(line) for line in segment.read_text().splitlines() if line.strip()]
        mutate(manifest, rows)
        manifest_path.write_text(json.dumps(manifest))
        segment.write_text('\n'.join(json.dumps(row) for row in rows) + '\n')
        return target

    try:
        with socket.socket() as check:
            check.bind(('127.0.0.1', args.port))
        publish({'out': str(out), 'sim': binary})
        with (out / 'mock.log').open('wb') as log:
            mock = subprocess.Popen(['python3', str(REPO / 'tests/mock_pms.py'), '--port', str(args.port), '--seed', '1'],
                                    cwd=REPO, stdout=log, stderr=subprocess.STDOUT)
        owned.append(mock)
        wait_marker(mock, out / 'mock.log', 'serving', 10)
        normal = out / 'normal-live'
        normal.mkdir()
        (normal / 'plxnative-token').write_text('synthetic-token')
        process = launch(normal)
        wait_marker(process, normal / 'plxnative-events.log', 'hubs: landed')
        assert (normal / 'auth.json').is_file(), 'normal boot still mints/persists its own identity'
        stop(process)
        publish({'case': 'normal-live', 'home_landed': True, 'identity_persisted': True})

        capture_failures = []
        for name in ('unsupported-natural', 'recorder-open-failure', 'fresh-natural', 'plaintext-natural'):
            root = out / name
            root.mkdir()
            (root / 'plxnative-rec').touch()
            if name != 'unsupported-natural':
                (root / 'plxnative-token').write_text('synthetic-token')
            if name == 'plaintext-natural':
                (root / 'auth.json').write_text(json.dumps({'client_id': 's00000031'}))
            if name == 'recorder-open-failure':
                capture_dir = root / 'plxnative-recordings/latest'
                capture_dir.mkdir(parents=True)
                (capture_dir / 'manifest.json').write_text('existing synthetic capture sentinel')
            before = snapshot(root)
            process = launch(root)
            try:
                if name in ('unsupported-natural', 'recorder-open-failure'):
                    assert process.wait(timeout=20) != 0, 'expected capture refusal'
                    assert snapshot(root) == before, 'refused capture changed persistence'
                    if name == 'recorder-open-failure':
                        assert (capture_dir / 'manifest.json').read_text() == 'existing synthetic capture sentinel'
                else:
                    wait_marker(process, root / 'plxnative-events.log', 'hubs: landed')
                    lines = (root / 'plxnative-events.log').read_text().splitlines()
                    assert lines.index('bootstrap: captured pre-effect initial state') < lines.index('bootstrap: captured session persistence applied')
                    assert (root / 'auth.json').is_file()
                    if name == 'plaintext-natural':
                        assert snapshot(root)['auth.json'][1] != before['auth.json'][1], 'atomic migration omitted'
                publish({'case': name, 'capture_ordering': 'PASS'})
            except (AssertionError, RuntimeError, ValueError) as error:
                capture_failures.append(name)
                publish({'case': name, 'capture_ordering': 'FAIL', 'reason': str(error)})
            finally:
                stop(process)
        assert not capture_failures, 'natural capture controls failed: ' + ','.join(capture_failures)
        if args.capture_only:
            publish({'natural_capture_gate': 'PASS', 'cases': 4})
            return

        record = out / 'record'
        record.mkdir()
        subprocess.run([binary, '--write-synthetic-initial', str(record / 'plxnative-app-init'), '1', str(args.port)],
                       cwd=REPO, check=True)
        for flag in ('rec', 'focus', 'noidle'):
            (record / ('plxnative-' + flag)).touch()
        process = launch(record)
        wait_marker(process, record / 'plxnative-events.log', 'hubs: landed')
        time.sleep(2)
        fd = os.open(record / 'plxnative-remote', os.O_RDWR | os.O_NONBLOCK)
        try:
            for token in ('up', 'left', 'down', 'down', 'down'):
                os.write(fd, (token + ' ').encode())
                time.sleep(.9)
        finally:
            os.close(fd)
        time.sleep(3)
        assert process.poll() is None, 'record must remain alive through the flow'
        stop(process)
        stop(mock)
        recording = record / 'plxnative-recordings/latest'
        assert not (record / 'auth.json').exists(), 'typed record must not load/mint an ambient session'
        rows = [json.loads(line) for line in (recording / 'rec-0000.jsonl').read_text().splitlines() if line.strip()]
        assert sum(row['t'] == 'in' and row.get('kind') == 'owned' for row in rows) == 10
        assert not any(row['t'] == 'eff' and 'unsupported' in row.get('payload', {}) for row in rows)
        assert sum(row['t'] == 'eff' and isinstance(row.get('payload', {}).get('delivery'), dict)
                   and row['payload']['delivery'].get('event') == 'input' for row in rows) == 10
        record_lines = (record / 'plxnative-events.log').read_text(errors='replace').splitlines()
        capture = next(i for i, line in enumerate(record_lines) if line == 'bootstrap: captured pre-effect initial state')
        assert all(i > capture for i, line in enumerate(record_lines) if line.startswith(('hubs: source', 'browse: roster', 'plex: server slot')))
        assert recording.stat().st_mode & 0o777 == 0o700
        assert all(path.stat().st_mode & 0o777 == 0o600 for path in recording.iterdir() if path.is_file())
        subprocess.run(['python3', '-B', str(REPO / 'tools/plxnative-rec'), 'check', str(recording)], cwd=REPO, check=True)

        listener = socket.socket()
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(('127.0.0.1', args.port))
        listener.listen()
        listener.settimeout(.2)
        def reject():
            while not stopped.is_set():
                try:
                    client, _ = listener.accept()
                    attempts.append(1)
                    client.close()
                except socket.timeout:
                    pass
        thread = threading.Thread(target=reject)
        thread.start()
        replay_case('fresh', recording, 'SAME')
        replay_case('ambient', recording, 'SAME', with_ambient=True)

        changes = {
            'missing-init': lambda m, r: m['init'].pop('data'),
            'malformed-init': lambda m, r: m['init'].__setitem__('data', False),
            'old-shape': lambda m, r: m.__setitem__('state_fp', m['state_fp'] ^ 1),
            'old-schema': lambda m, r: m.__setitem__('schema', 1),
            'wrong-features': lambda m, r: m.__setitem__('features', []),
            'unsupported-blobs': lambda m, r: m.__setitem__('blobs', True),
            'unsupported-authority': lambda m, r: m['init']['data']['session'].__setitem__('authority', {'Account': {'extras': []}}),
            'missing-state-grade': lambda m, r: r.__setitem__(slice(None), [row for row in r if not (row['t'] == 'st' and row['f'] == 0)]),
            'changed-hidden-init': lambda m, r: m['init']['data']['consent'].__setitem__('usage', True),
            'wrong-binding': lambda m, r: next(row for row in r if row['t'] == 'async')['payload'].__setitem__('client', 77),
            'wrong-address': lambda m, r: next(row for row in r if row['t'] == 'async').__setitem__('to', 'store:9'),
            'unsupported-result': lambda m, r: next(row for row in reversed(r) if row['t'] == 'async')['payload'].__setitem__('kind', 'unimplemented'),
            'overflow-tick': lambda m, r: next(row for row in r if row['t'] == 'tick').__setitem__('ms', 1 << 32),
            'duplicate-tick': lambda m, r: r.insert(1, dict(r[0])),
            'unsupported-marker': lambda m, r: next(row for row in r if row['t'] == 'eff').__setitem__('payload', {'unsupported': 'unsupported Home screen delivery'}),
            'missing-admission': lambda m, r: next(row for row in r if row['t'] == 'eff' and row['e'] == 'Request')['payload'].pop('admitted'),
            'bad-admission-identity': lambda m, r: next(row for row in r if row['t'] == 'eff' and row['e'] == 'Request')['payload'].__setitem__('client', 77),
            'oversized-decoded-init': lambda m, r: m['init']['data'].__setitem__('oversized', [0] * 300000),
            'oversized-decoded-frame': lambda m, r: r.insert(1, {'f':0, 't':'in', 'payload':[0] * 300000}),
            'tiny-many-frames': lambda m, r: r.__setitem__(slice(None), [{'f':f, 't':'tick', 'ms':0, 'dt_us':0} for f in range(60000)]),
            'early-malformed-frame': lambda m, r: r.__setitem__(slice(None), [{'f':f, 't':'timer', 'id':0} for f in range(60000)]),
        }
        for name, change in changes.items():
            replay_case(name, variant(name, recording, change), 'REFUSED', with_ambient=True)
        replay_case('both-triggers', recording, 'REFUSED', with_ambient=True, both=True)

        fifo = variant('fifo', recording, lambda m, r: None)
        (fifo / 'manifest.json').rename(fifo / 'retained-manifest.json')
        os.mkfifo(fifo / 'manifest.json', 0o600)
        replay_case('fifo', fifo, 'REFUSED')
        oversized = variant('oversized', recording, lambda m, r: None)
        with (oversized / 'manifest.json').open('r+b') as file:
            file.truncate(CAP_BYTES + 1)
        replay_case('oversized', oversized, 'REFUSED')

        def changed_effect(manifest, rows):
            effect = next(row['payload']['session_effect']['PublishProfile'] for row in rows
                          if isinstance(row.get('payload', {}).get('session_effect'), dict)
                          and 'PublishProfile' in row['payload']['session_effect'])
            effect['scope'] += 1
        changed = variant('changed-supported-effect', recording, changed_effect)
        replay_case('changed-supported-effect', changed, 'DIVERGED')
        (out / 'results.json').write_text(json.dumps(results, indent=2))
        publish({'gate': 'PASS', 'replay_cases': len(results), 'recording': str(recording)})
    finally:
        for process in reversed(owned):
            stop(process)
        stopped.set()
        if thread:
            thread.join(timeout=2)
        if listener:
            listener.close()
        publish({'all_owned_processes_terminal': all(process.poll() is not None for process in owned)})


if __name__ == '__main__':
    main()
