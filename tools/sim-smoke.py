#!/usr/bin/env python3
"""Launch a host simulator, require a clean 1920x1080 PNG capture, then exit.

A failed launch explains itself: the tail of the instance's event log is printed, and when the
simulator died on a signal and `--core-dir` names where the kernel writes core files (CI points
`kernel.core_pattern` there), every thread's backtrace is printed from the core with gdb. An
intermittent crash on a CI runner cannot be rerun under a debugger after the fact, so the stack
has to be captured by the run that crashed.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import resource
import shutil
import signal
import struct
import subprocess
import sys
import time
import zlib


PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def png_size(path: Path) -> tuple[int, int]:
    payload = path.read_bytes()
    if not payload.startswith(PNG_SIGNATURE):
        raise ValueError("capture has no PNG signature")
    offset = len(PNG_SIGNATURE)
    size: tuple[int, int] | None = None
    saw_iend = False
    while offset < len(payload):
        if offset + 12 > len(payload):
            raise ValueError("capture ends inside a PNG chunk")
        length = struct.unpack(">I", payload[offset : offset + 4])[0]
        chunk_end = offset + 12 + length
        if chunk_end > len(payload):
            raise ValueError("capture contains a truncated PNG chunk")
        chunk_type = payload[offset + 4 : offset + 8]
        chunk_data = payload[offset + 8 : offset + 8 + length]
        expected_crc = struct.unpack(">I", payload[offset + 8 + length : chunk_end])[0]
        actual_crc = zlib.crc32(chunk_type)
        actual_crc = zlib.crc32(chunk_data, actual_crc) & 0xFFFFFFFF
        if actual_crc != expected_crc:
            raise ValueError(f"capture has an invalid {chunk_type!r} checksum")
        if offset == len(PNG_SIGNATURE):
            if chunk_type != b"IHDR" or length != 13:
                raise ValueError("capture does not start with a valid IHDR chunk")
            size = struct.unpack(">II", chunk_data[:8])
        if chunk_type == b"IEND":
            if length != 0 or chunk_end != len(payload):
                raise ValueError("capture has an invalid IEND chunk")
            saw_iend = True
            break
        offset = chunk_end
    if size is None or not saw_iend:
        raise ValueError("capture is missing a complete PNG image")
    return size


EVENT_LOG_TAIL_LINES = 80


def allow_core_dumps() -> None:
    """Child pre-exec hook: lift RLIMIT_CORE so a crashing simulator leaves a core behind."""
    _, hard = resource.getrlimit(resource.RLIMIT_CORE)
    resource.setrlimit(resource.RLIMIT_CORE, (hard, hard))


def print_event_log_tail(events: Path) -> None:
    try:
        lines = events.read_text(errors="replace").splitlines()
    except OSError as error:
        print(f"(no event log at {events}: {error})", file=sys.stderr)
        return
    print(f"--- last {EVENT_LOG_TAIL_LINES} lines of {events} ---", file=sys.stderr)
    for line in lines[-EVENT_LOG_TAIL_LINES:]:
        print(line, file=sys.stderr)
    print("--- end of event log ---", file=sys.stderr)


def print_core_backtrace(binary: Path, core_dir: Path | None, since: float) -> None:
    """Print every thread's stack from the newest core written into `core_dir` by this launch."""
    if core_dir is None:
        return
    cores = [
        path
        for path in core_dir.glob("core*")
        if path.is_file() and path.stat().st_mtime >= since
    ]
    if not cores:
        print(f"(no core file from this launch in {core_dir})", file=sys.stderr)
        return
    core = max(cores, key=lambda path: path.stat().st_mtime)
    gdb = shutil.which("gdb")
    if gdb is None:
        print(f"(core written to {core}, but gdb is not installed)", file=sys.stderr)
        return
    print(f"--- backtrace of every thread from {core} ---", file=sys.stderr, flush=True)
    subprocess.run(
        [
            gdb, "-batch", "-nx",
            "-ex", "set pagination off",
            "-ex", "info signals SIGSEGV",
            "-ex", "print $_siginfo",
            "-ex", "info threads",
            "-ex", "thread apply all bt",
            str(binary), str(core),
        ],
        stdout=sys.stderr,
        stderr=subprocess.STDOUT,
        timeout=300,
        check=False,
    )
    print("--- end of backtrace ---", file=sys.stderr, flush=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--runtime", required=True, type=Path)
    parser.add_argument("--assets", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--timeout", type=int, default=45)
    parser.add_argument(
        "--core-dir",
        type=Path,
        help="where kernel.core_pattern writes core files; a signal death prints their stacks",
    )
    args = parser.parse_args()

    binary = args.binary.resolve(strict=True)
    assets = args.assets.resolve(strict=True)
    runtime = args.runtime.resolve()
    output = args.output.resolve()
    runtime.mkdir(parents=True, exist_ok=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.unlink(missing_ok=True)

    env = os.environ.copy()
    env.update(
        PLXNATIVE_RUNTIME_DIR=str(runtime),
        PLXNATIVE_APP_DIR=str(assets),
        PLXNATIVE_WIN="1920x1080",
        PLXNATIVE_SHOT=str(output),
        PLXNATIVE_SHOT_FRAME="3",
        PLXNATIVE_SHOT_EXIT="1",
    )
    events = runtime / "plxnative-events.log"
    launched_at = time.time()
    try:
        result = subprocess.run(
            [str(binary)],
            preexec_fn=allow_core_dumps,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=args.timeout,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        if error.stdout:
            print(error.stdout, file=sys.stderr)
        print(f"simulator did not exit within {args.timeout}s", file=sys.stderr)
        print_event_log_tail(events)
        return 1

    if result.returncode != 0:
        print(result.stdout, file=sys.stderr)
        if result.returncode < 0:
            name = signal.Signals(-result.returncode).name
            print(f"simulator exited with {result.returncode} ({name})", file=sys.stderr)
        else:
            print(f"simulator exited with {result.returncode}", file=sys.stderr)
        print_event_log_tail(events)
        if result.returncode < 0:
            print_core_backtrace(binary, args.core_dir, launched_at)
        return 1
    try:
        size = png_size(output)
    except (OSError, ValueError) as error:
        print(f"invalid simulator capture at {output}: {error}", file=sys.stderr)
        return 1
    if size != (1920, 1080):
        print(f"simulator capture is {size[0]}x{size[1]}, expected 1920x1080", file=sys.stderr)
        return 1
    if not events.is_file() or "shot: wrote 1920x1080" not in events.read_text(errors="replace"):
        print("simulator exited without recording a completed screenshot", file=sys.stderr)
        return 1
    print(f"simulator smoke passed: {output} ({size[0]}x{size[1]})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
