#!/usr/bin/env python3
"""Native HUD package invocation and immutable snapshots for authoring tools.

The Rust SDK is the only packer and validator. This module never constructs a plugin ZIP.
"""
from __future__ import annotations

from contextlib import contextmanager
from dataclasses import dataclass
import hashlib
import json
import os
import stat
from pathlib import Path
import subprocess
import tempfile


def native_json(binary, *arguments, env=None, findings=False):
    result = subprocess.run([str(binary), *map(str, arguments)], env=env,
                            capture_output=True, text=True, timeout=300)
    try:
        report = json.loads(result.stdout)
    except (ValueError, TypeError) as error:
        raise ValueError(f"native HUD command did not return JSON ({result.returncode}): {result.stderr.strip()}") from error
    if result.returncode not in ((0, 1) if findings else (0,)):
        raise ValueError(f"native HUD command failed ({result.returncode}): {report}")
    if findings and (report.get('protocol') != 1 or bool(report.get('findings')) != bool(result.returncode)):
        raise ValueError('native HUD findings and exit status disagree')
    return report


@dataclass(frozen=True)
class Snapshot:
    path: Path
    content: bytes
    sha256: str
    info: dict
    inspection: dict


@contextmanager
def snapshot(binary, source, *, env=None):
    """Freeze a directory once through Rust, or copy one bounded archive before any inspection."""
    source = Path(source)
    info = native_json(binary, '--hud-info', env=env)
    pairs = info.get('supported_formats', [])
    if info.get('protocol') != 1 or not pairs:
        raise ValueError('the native SDK must advertise bundle support')
    maxima = [info['limits']['package'][pair['package']]['artifact_bytes'] for pair in pairs]
    if any(type(value) is not int or value <= 0 for value in maxima):
        raise ValueError('native artifact limits must be positive integers')
    maximum = max(maxima)
    if source.is_symlink():
        raise ValueError('HUD input cannot be a symlink')
    with tempfile.TemporaryDirectory(prefix='hud-package-') as temporary:
        frozen = Path(temporary) / 'hud.plxhud'
        built = None
        if source.is_dir():
            built = native_json(binary, '--hud-pack', source.resolve(), '--out', frozen, '--format', 'json', env=env)
            if built.get('protocol') != 1 or built.get('findings'):
                raise ValueError('native source snapshot did not pass admission')
        else:
            if not source.is_file():
                raise ValueError('HUD archive must be a regular file')
            # O_NONBLOCK prevents a file swapped for a FIFO between stat and open from hanging.
            descriptor = os.open(source, os.O_RDONLY | os.O_NONBLOCK | getattr(os, 'O_NOFOLLOW', 0))
            with os.fdopen(descriptor, 'rb') as stream:
                if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                    raise ValueError('HUD archive must be a regular file')
                content = stream.read(maximum + 1)
            if len(content) > maximum:
                raise ValueError('HUD input exceeds the native artifact limit')
            frozen.write_bytes(content)
        with frozen.open('rb') as stream:
            content = stream.read(maximum + 1)
        if len(content) > maximum:
            raise ValueError('native packer output exceeds its artifact limit')
        digest = hashlib.sha256(content).hexdigest()
        if built is not None and (built.get('package_sha256') != digest or built.get('bytes') != len(content)):
            raise ValueError('native pack report does not identify its complete artifact')
        frozen.chmod(0o444)
        inspection = native_json(binary, '--hud-inspect', frozen, env=env)
        manifest = inspection.get('manifest', {})
        if (inspection.get('protocol') != 1 or inspection.get('package_sha256') != digest
            or {'package': manifest.get('package_format'), 'hud': manifest.get('hud', {}).get('format')} not in pairs
            or built is not None and built.get('manifest') != manifest):
            raise ValueError('native inspection differs from the frozen artifact identity or manifest')
        yield Snapshot(frozen, content, digest, info, inspection)
