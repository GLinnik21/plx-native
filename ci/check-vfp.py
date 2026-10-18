#!/usr/bin/env python3
"""Fail when our Rust code stops compiling to VFP instructions for the TV.

WHY. The ARM build uses `armv7-unknown-linux-gnueabi`, whose target spec is `+soft-float`: no VFP at
all, every float operation a call into the runtime (`__aeabi_fmul`). The Makefile's codegen flags
(`-C target-cpu=cortex-a9 -C target-feature=-neon,-soft-float -C force-frame-pointers=yes`) give OUR
crates VFP back under the same soft calling convention. On STABLE rustc, `-soft-float` is an
unstable target feature: rustc prints "unstable feature specified for -Ctarget-feature: soft-float
... its behavior can change in the future" and is free to stop honouring it in any release. When it
does, nothing breaks, the binary still links and runs, and every float multiply in the UI quietly
becomes a libcall: a performance regression no test sees. This gate sees it.

WHAT IT DOES (about 0.3 s, no cargo, no linking, no device):

 1. The flags are the Makefile's exact `RUST_CODEGEN` (`make -s print-rust-codegen`, which is also
    what `RUST_ENV`'s RUSTFLAGS starts with) and must equal the `rustflags` of
    rust-modules/.cargo/config.toml, the fallback list for cargo invoked another way. The two lists
    drifting apart is how a flag gets dropped without anyone deciding to.
 2. It compiles a tiny `#![no_std]` library with `rustc --target armv7-unknown-linux-gnueabi -O
    --emit=asm` and those flags: a Rust-ABI f32 and f64 multiply-add. The assembly must contain VFP
    arithmetic (`vmla`/`vmul` on s/d registers) and NO `__aeabi_f*`/`__aeabi_d*` arithmetic call.
 3. NEGATIVE CONTROL: the same probe with `-soft-float` removed must show the libcalls. Without it
    a probe that compiled to nothing, or an assembler listing in another shape, would read as "clean"
    forever. If the control does not show them, the gate says its scan is not live.

`ci/check-elf.sh` asks the same question of the linked binary (VFP count against `__aeabi_fmul`
calls, next to its CP15 assertion), but only where a binary exists; this runs in `make check` and in
CI's host-lint job. Exit 0 pass, 1 fail, and with --skip-if-missing a toolchain or target that is not
installed is a skip (exit 0, said out loud) instead of a failure: `make check`'s Python step uses that
so a host-python runner without Rust is not red, while CI's host-lint job (which installs the
toolchain) runs it strict.

    python3 ci/check-vfp.py [--toolchain <release>] [--skip-if-missing] [--rustflags '<flags>']
"""
from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_toolchain import TARGET, pinned  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
CONFIG = ROOT / "rust-modules" / ".cargo" / "config.toml"

PROBE = """#![no_std]
// Rust-ABI float arithmetic, not `extern "C"`: the question is what OUR crates compile to.
#[no_mangle]
pub fn plx_vfp_probe_f32(a: f32, b: f32, c: f32) -> f32 { a * b + c }
#[no_mangle]
pub fn plx_vfp_probe_f64(a: f64, b: f64, c: f64) -> f64 { a * b + c }
"""

# VFP arithmetic on s (f32) / d (f64) registers. NEON would print q registers or `.f32 d`-pair forms
# of vmul on d registers too, so the f32 form is pinned to an s register: that is VFP, not NEON.
VFP_F32 = re.compile(r"^\s*v(?:mul|mla|nmla|nmul|add|sub|div|fma)\.f32\s+s\d+", re.M)
VFP_F64 = re.compile(r"^\s*v(?:mul|mla|nmla|nmul|add|sub|div|fma)\.f64\s+d\d+", re.M)
# The libcalls a soft-float multiply/add becomes: the ARM EABI helpers and the generic libgcc names.
SOFT_CALL = re.compile(r"^\s*bl\s+(?:__aeabi_[fd](?:mul|add|sub|div)|__(?:mul|add|sub|div)[sd]f3)\b", re.M)


class Missing(Exception):
    """The toolchain or its ARM target is not installed here."""


def codegen_flags() -> str:
    """The Makefile's RUST_CODEGEN, exactly as the ARM recipe passes it."""
    out = subprocess.run(["make", "-s", "print-rust-codegen"], cwd=ROOT, capture_output=True, text=True)
    flags = out.stdout.strip()
    if out.returncode != 0 or "target-cpu" not in flags:
        raise SystemExit("check-vfp: FAIL: `make -s print-rust-codegen` gave no codegen flags:\n" + out.stderr)
    return flags


def config_flags(text: str | None = None) -> str:
    """The `rustflags` list of the armv7 target table in rust-modules/.cargo/config.toml, space-joined."""
    text = CONFIG.read_text() if text is None else text
    table = re.search(r"^\[target\." + re.escape(TARGET) + r"\]\s*\n(?P<body>(?:(?!\[).*\n?)*)", text, re.M)
    if not table:
        raise ValueError(f"no [target.{TARGET}] table in {CONFIG}")
    flags = re.search(r"^rustflags\s*=\s*\[(?P<items>[^\]]*)\]", table.group("body"), re.M)
    if not flags:
        raise ValueError(f"[target.{TARGET}] has no rustflags list")
    return " ".join(re.findall(r'"([^"]*)"', flags.group("items")))


def classify(asm: str) -> dict[str, int]:
    return {"vfp_f32": len(VFP_F32.findall(asm)), "vfp_f64": len(VFP_F64.findall(asm)),
            "soft_calls": len(SOFT_CALL.findall(asm))}


def compile_probe(toolchain: str, flags: str, workdir: Path, name: str) -> str:
    src = workdir / "probe.rs"
    if not src.exists():
        src.write_text(PROBE)
    out = workdir / (name + ".s")
    env = dict(os.environ, RUSTUP_AUTO_INSTALL="0")
    cmd = ["rustc", f"+{toolchain}", "--target", TARGET, "--crate-type=lib", "--emit=asm", "-O",
           "--cap-lints=allow", *flags.split(), str(src), "-o", str(out)]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, env=env, cwd=workdir)
    except FileNotFoundError:
        raise Missing("no rustc on PATH")
    if proc.returncode != 0:
        if "is not installed" in proc.stderr or "can't find crate for `core`" in proc.stderr:
            raise Missing(proc.stderr.strip().splitlines()[0])
        raise SystemExit("check-vfp: FAIL: the probe did not compile:\n" + proc.stderr)
    return out.read_text()


def without_soft_float_off(flags: str) -> str:
    """The same flags with `-soft-float` dropped from -C target-feature (the target default, +soft-float)."""
    out = re.sub(r",?-soft-float", "", flags)
    return out.replace("target-feature=,", "target-feature=")


def judge(on: dict[str, int], control: dict[str, int]) -> list[str]:
    """Problems with the two probe results; empty when the gate passes. Pure, so the tests drive it."""
    problems = []
    if on["vfp_f32"] < 1 or on["vfp_f64"] < 1:
        problems.append(f"the probe compiled with the Makefile's flags has no VFP arithmetic (f32 ops "
                        f"{on['vfp_f32']}, f64 ops {on['vfp_f64']}): `-C target-feature=-soft-float` no longer takes effect")
    if on["soft_calls"]:
        problems.append(f"the probe compiled with the Makefile's flags calls the software float helpers "
                        f"{on['soft_calls']} time(s): every float op in our crates would be a libcall")
    if control["soft_calls"] < 2 or control["vfp_f32"] or control["vfp_f64"]:
        problems.append(f"the negative control (the same probe WITHOUT -soft-float, i.e. the target default) did not "
                        f"look soft-float (libcalls {control['soft_calls']}, VFP ops {control['vfp_f32'] + control['vfp_f64']}): "
                        f"this scan can no longer tell the two apart, so a pass above proves nothing")
    return problems


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--toolchain", default=pinned(), help="default: the pin in rust-modules/rust-toolchain.toml")
    ap.add_argument("--skip-if-missing", action="store_true",
                    help="exit 0 with a note when the toolchain or its armv7 target is not installed")
    ap.add_argument("--rustflags", help="codegen flags to probe instead of the Makefile's (used by the tests)")
    args = ap.parse_args(argv)

    flags = args.rustflags or codegen_flags()
    if not args.rustflags:
        cfg = config_flags()
        if cfg != flags:
            print("check-vfp: FAIL: rust-modules/.cargo/config.toml's rustflags and the Makefile's RUST_CODEGEN differ:\n"
                  f"  config.toml: {cfg}\n  Makefile:    {flags}\n"
                  "  Keep the two lists in step (the RUSTFLAGS environment variable REPLACES the config list).")
            return 1
    if "-soft-float" not in flags:
        print(f"check-vfp: FAIL: the codegen flags carry no `-soft-float`: {flags}")
        return 1
    work = Path(tempfile.mkdtemp(prefix="check-vfp."))
    try:
        try:
            on = classify(compile_probe(args.toolchain, flags, work, "on"))
            control = classify(compile_probe(args.toolchain, without_soft_float_off(flags), work, "control"))
        except Missing as why:
            if args.skip_if_missing:
                print(f"check-vfp: SKIP: {args.toolchain} with the {TARGET} target is not installed here ({why})")
                return 0
            print(f"check-vfp: FAIL: cannot run the probe: {args.toolchain} / {TARGET} not installed ({why}).\n"
                  f"  rustup toolchain install {args.toolchain} --profile minimal --target {TARGET}")
            return 1
    finally:
        shutil.rmtree(work, ignore_errors=True)
    problems = judge(on, control)
    if problems:
        print("check-vfp: FAIL")
        for p in problems:
            print("  " + p)
        return 1
    print(f"check-vfp: ok ({args.toolchain}: {on['vfp_f32']} f32 + {on['vfp_f64']} f64 VFP ops, 0 soft-float calls; "
          f"control without -soft-float: {control['soft_calls']} libcalls)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
