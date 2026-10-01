#!/usr/bin/env python3
"""Seed a new lane's cargo target dir with an APFS clone of third-party build output.

A linked worktree ("lane") has its own cargo target dirs (`rust-modules/target`,
`target-release`, ...). Cold, each one recompiles every registry crate and the build-std
sysroot (std/core/alloc built from source) even though those artifacts are byte-for-byte what
the previous lane already built: cargo's metadata hash for a registry crate does not depend on
the path of the checkout. About 40% of a lane's target bytes (0.9 GB host, 0.44 GB ARM release) are exactly that.

    cargo-seed.py restore KIND TDIR     clone the seed into TDIR, only if TDIR does not exist
    cargo-seed.py harvest KIND TDIR     refresh the seed from TDIR, only if the seed is stale or
                                        lacks a top-level directory (`debug/`, `arm-.../`) TDIR has
    cargo-seed.py prune [--days N]      delete seeds unused for N days (`build-gc.sh --cache`)

What makes this safe, in order of importance:

* **The app crate is never in the seed.** Every entry whose name contains `plxnative` (the
  package, the storage helper, their fingerprints, build-script output, test binaries and their
  `*.rcgu.o` objects) is deleted from the harvested copy, and so is `.lib-artifacts.json`. Cargo
  decides whether a PATH package needs rebuilding by mtime, and its metadata hash is the same in
  every lane, so a cloned app artifact newer than the lane's sources would be linked silently. A
  cloned third-party artifact, by contrast, is validated by cargo's own fingerprint and is at worst
  recompiled, so a wrong seed costs time and never bytes.
* **Restore only ever creates an ABSENT directory.** A lane that has a target dir keeps it; the
  clone is made next to its destination and renamed into place, so a failure never leaves a
  partial tree behind.
* **It clones or it does nothing.** `clonefile(2)` is called directly (not `cp -c`, which quietly
  falls back to a full copy, spending the very disk this exists to save). Another volume, a
  filesystem without cloning, or any platform other than macOS: a no-op, never a copy.
* **Linked worktrees only.** The main checkout keeps its own incremental cache and full DWARF,
  which no lane could share, so the Makefile does not call this there and the script refuses
  too. `PLX_CARGO_SEED=off` disables it everywhere.
* **A key guards the seed**: the sha256 of the toolchain (`rustc +nightly -vV` and `rustc -vV`),
  `Cargo.lock`, the manifests (minus the app's own `version =` line), and every cargo config that
  applies. A seed with a different key is not restored, and the next lane to finish a build
  replaces it.

The seed lives under `$PLX_BUILD_CACHE/cargo-seed/<kind>/` (default `~/.cache/plxnative`) beside
the shared FFmpeg cache, and `tools/build-gc.sh --cache` (30 days) and `--seed` (7 days, also run by `--auto` under disk
pressure) prune it by age. `du` counts a clone's
bytes in full, so a lane that was seeded reads as big as one that was not: `df` is the truth.

Every failure here is non-fatal by design (exit 0 with one warning line); a build never waits on,
or fails because of, the seed.
"""
import argparse
import ctypes
import fcntl
import hashlib
import os
import re
import shutil
import subprocess
import sys
import time

# The build trees worth seeding. `-lab`, `-sym` and `-shots` variants are rare, large and
# feature-specific; they build cold. `target-sim` (`make sim`) is left out because reuse across
# lanes has not been proven for it (its host FFmpeg and libass builds make that a long
# experiment). The storage helper's trees (`target-storage`, `target-release-storage`) are left out
# for a measured reason: the Makefile links that helper with
# CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABI_LINKER set to an ABSOLUTE path inside the lane
# (ci/arm-cc.py), and cargo hashes the linker setting into every unit's fingerprint ("dirty:
# ConfigSettingsChanged"), so a seed made in one lane recompiles the whole helper tree, std
# included, in the next. It is also the smallest tree (~0.25 GB, a 13 s compile).
KINDS = ("target", "target-release")
APP_MARK = "plxnative"
KEY_FILE = ".plx-seed-key"
USED_FILE = ".last-used"
CARGO_LOCKS = (".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock")
# An interrupted harvest/restore leaves `<kind>.tmp.<pid>` / `.restore.<pid>` / `.old.<pid>`.
LEFTOVER_MARKS = (".tmp.", ".restore.", ".old.", ".tombstone.")
LEFTOVER_MAX_AGE = 24 * 3600


def say(msg):
    print(f"cargo-seed: {msg}", file=sys.stderr)


class CloneUnsupported(OSError):
    pass


def clone_tree(src, dst):
    """Clone the directory tree `src` to the absent path `dst` with clonefile(2), or raise.

    Never falls back to a copy. Tests replace this function with a plain copy so they can run on
    filesystems (and platforms) that cannot clone.
    """
    if sys.platform != "darwin":
        raise CloneUnsupported(0, f"no clonefile(2) on {sys.platform}")
    libc = ctypes.CDLL(None, use_errno=True)
    fn = libc.clonefile
    fn.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint32]
    fn.restype = ctypes.c_int
    if fn(os.fsencode(src), os.fsencode(dst), 0) != 0:
        err = ctypes.get_errno()
        shutil.rmtree(dst, ignore_errors=True)
        raise OSError(err, os.strerror(err))


def seeds_root():
    cache = os.environ.get("PLX_BUILD_CACHE") or os.path.expanduser("~/.cache/plxnative")
    return os.path.join(cache, "cargo-seed")


def default_root():
    return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def linked_worktree(root):
    # In a linked worktree `.git` is a file ("gitdir: ..."); in the main checkout it is a dir.
    return os.path.isfile(os.path.join(root, ".git"))


def run_text(argv):
    try:
        return subprocess.run(argv, capture_output=True, text=True, timeout=30).stdout
    except (OSError, subprocess.SubprocessError):
        return None


def read_bytes(path):
    try:
        with open(path, "rb") as fh:
            return fh.read()
    except OSError:
        return b""


def manifest_bytes(path):
    """A Cargo.toml without the app's own top-level `version = "x.y.z"` line.

    A release bump edits only that line, and it changes no third-party hash; counting it would
    discard a perfectly good seed on every version bump.
    """
    lines = read_bytes(path).decode("utf-8", "replace").splitlines()
    return "\n".join(l for l in lines if not l.startswith('version = "')).encode()


def cargo_configs(root):
    """Every config file cargo merges for a build run from `<root>/rust-modules`, nearest first."""
    out = []
    d = os.path.join(root, "rust-modules")
    while True:
        for name in ("config.toml", "config"):
            p = os.path.join(d, ".cargo", name)
            if os.path.isfile(p):
                out.append(p)
        parent = os.path.dirname(d)
        if parent == d:
            break
        d = parent
    home = os.environ.get("CARGO_HOME") or os.path.expanduser("~/.cargo")
    for name in ("config.toml", "config"):
        p = os.path.join(home, name)
        if os.path.isfile(p):
            out.append(p)
    return out


def seed_key(root, kind):
    """The identity of the third-party artifacts a seed holds, or None if it cannot be computed."""
    toolchain = os.environ.get("RUST_NIGHTLY", "nightly")
    pinned = run_text(["rustc", f"+{toolchain}", "-vV"])
    if not pinned:
        return None
    h = hashlib.sha256()

    def part(label, data):
        h.update(label.encode() + b"\0" + data + b"\0")

    part("kind", kind.encode())
    part("rustc+toolchain", pinned.encode())
    part("rustc", (run_text(["rustc", "-vV"]) or "").encode())
    part("lock", read_bytes(os.path.join(root, "rust-modules", "Cargo.lock")))
    part("manifest", manifest_bytes(os.path.join(root, "rust-modules", "Cargo.toml")))
    part("manifest-storage", manifest_bytes(os.path.join(root, "rust-modules", "storage", "Cargo.toml")))
    for p in cargo_configs(root):
        # CONTENT ONLY, never the path: a path would make every lane's key differ from every
        # other's, and a seed would match only the lane that made it.
        part("config", read_bytes(p))
    return h.hexdigest()


def sweep_leftovers(base):
    now = time.time()
    try:
        names = os.listdir(base)
    except OSError:
        return
    for name in names:
        if any(m in name for m in LEFTOVER_MARKS):
            p = os.path.join(base, name)
            try:
                if now - os.lstat(p).st_mtime > LEFTOVER_MAX_AGE:
                    shutil.rmtree(p, ignore_errors=True)
            except OSError:
                pass


def local_packages(root):
    """Names of every `Cargo.lock` package that has no `source =` line (a path or `[patch]` crate).

    Cargo judges such a package by the mtimes of its sources alone, and its unit hash is the same
    in every lane, so a cloned artifact of it could be reported fresh and linked silently. That
    holds for every one of them, not just the app, so the strip works from the lock rather than
    from a name. None when the lock lists no package at all (unreadable or not a lock): the
    caller then refuses to harvest rather than guess what is local.
    """
    text = read_bytes(os.path.join(root, "rust-modules", "Cargo.lock")).decode("utf-8", "replace")
    names, seen = set(), 0
    for block in text.split("[[package]]")[1:]:
        seen += 1
        name, has_source = None, False
        for line in block.splitlines():
            line = line.strip()
            if line.startswith("[") and not line.startswith("[[package"):
                break
            m = re.match(r'name\s*=\s*"([^"]+)"', line)
            if m and name is None:
                name = m.group(1)
            if re.match(r"source\s*=", line):
                has_source = True
        if name and not has_source:
            names.add(name)
    return names if seen else None


def local_matcher(names):
    """A predicate over a target-dir entry name: does it belong to one of the local packages?

    Cargo names a package's output `<name>-<hash>` (fingerprint and build dirs), `<crate>-<hash>.*`
    and `lib<crate>-<hash>.*` (deps), with `-` written as `_` in the crate name. The app's own
    binaries (`plxnative-sim`) carry no package name, hence the standing APP_MARK test. A third-party
    crate whose name merely starts with a local one is stripped too: that costs a rebuild, never
    a wrong byte.
    """
    stems = set()
    for n in names:
        for t in {n, n.replace("-", "_")}:
            stems.update((t, "lib" + t))

    def match(entry):
        if APP_MARK in entry:
            return True
        return any(entry == t or entry.startswith(t + "-") or entry.startswith(t + ".") for t in stems)

    return match


def strip_app(tree, local):
    """Delete everything of the local packages (the app and the storage helper, and any other
    path crate `local_packages` found) from a cloned target tree. `local` is `local_matcher(...)`."""
    for dirpath, dirnames, filenames in os.walk(tree, topdown=True):
        keep = []
        for d in dirnames:
            full = os.path.join(dirpath, d)
            if local(d) or (d == "incremental" and os.path.dirname(full) != tree):
                shutil.rmtree(full, ignore_errors=True)
            else:
                keep.append(d)
        dirnames[:] = keep
        for f in filenames:
            if local(f):
                try:
                    os.unlink(os.path.join(dirpath, f))
                except OSError:
                    pass
    try:
        os.unlink(os.path.join(tree, ".lib-artifacts.json"))
    except OSError:
        pass


class Flock:
    """A non-blocking advisory lock that records whether it was obtained."""

    def __init__(self, path, exclusive):
        self.path, self.exclusive, self.fd, self.held = path, exclusive, None, False

    def __enter__(self):
        try:
            self.fd = os.open(self.path, os.O_RDONLY | os.O_CREAT, 0o644)
            fcntl.flock(self.fd, (fcntl.LOCK_EX if self.exclusive else fcntl.LOCK_SH) | fcntl.LOCK_NB)
            self.held = True
        except OSError:
            self.held = False
        return self

    def __exit__(self, *exc):
        if self.fd is not None:
            os.close(self.fd)


class CargoLocks:
    """Shared locks on every cargo build lock under `tdir`, held for the whole `with` block.

    `idle` is False (and nothing stays held) when a cargo is building there. While the block runs,
    a cargo that starts blocks on its own exclusive lock instead of writing into the tree being
    cloned, so the clone is never a mix of two builds' states. clonefile(2) of a directory is not
    an atomic snapshot, which is why holding these across it matters.
    """

    def __init__(self, tdir):
        self.tdir, self.fds, self.idle = tdir, [], True

    def __enter__(self):
        for dirpath, dirnames, filenames in os.walk(self.tdir):
            if dirpath[len(self.tdir):].count(os.sep) >= 3:
                dirnames[:] = []
            for f in filenames:
                if f in CARGO_LOCKS:
                    try:
                        fd = os.open(os.path.join(dirpath, f), os.O_RDONLY)
                    except OSError:
                        continue
                    self.fds.append(fd)
                    try:
                        fcntl.flock(fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
                    except OSError:
                        self.idle = False
                        self.release()
                        return self
        return self

    def release(self):
        for fd in self.fds:
            os.close(fd)
        self.fds = []

    def __exit__(self, *exc):
        self.release()


def resolve_tdir(tdir, env_var, env_base):
    if env_var and os.environ.get(env_var):
        tdir = os.environ[env_var]
        if not os.path.isabs(tdir):
            tdir = os.path.join(env_base or os.getcwd(), tdir)
    return os.path.abspath(tdir)


def eligible(kind, root):
    if kind not in KINDS:
        return False
    if os.environ.get("PLX_CARGO_SEED") == "off":
        return False
    if not linked_worktree(root):
        return False
    return True


def restore(kind, tdir, root):
    if not eligible(kind, root):
        return
    if os.path.lexists(tdir):
        return  # a lane keeps the target dir it has; restore creates, never merges
    base = seeds_root()
    seed = os.path.join(base, kind)
    key = seed_key(root, kind)
    if key is None or read_bytes(os.path.join(seed, KEY_FILE)).decode(errors="replace").strip() != key:
        return  # no seed, or one for another toolchain / lock / config: the lane builds cold
    parent = os.path.dirname(tdir)
    os.makedirs(parent, exist_ok=True)
    if os.stat(parent).st_dev != os.stat(seed).st_dev:
        say(f"{kind}: {tdir} is on another volume than the seed; not seeding")
        return
    with Flock(os.path.join(base, f"{kind}.lock"), exclusive=False) as lock:
        if not lock.held:
            return  # a harvest is swapping the seed this moment; build cold rather than wait
        tmp = os.path.join(base, f"{kind}.restore.{os.getpid()}")
        shutil.rmtree(tmp, ignore_errors=True)
        try:
            clone_tree(seed, tmp)
            for f in (KEY_FILE, USED_FILE):
                try:
                    os.unlink(os.path.join(tmp, f))
                except OSError:
                    pass
            os.rename(tmp, tdir)
        except OSError as e:
            shutil.rmtree(tmp, ignore_errors=True)
            say(f"{kind}: not seeded ({e.strerror or e}); this lane builds cold")
            return
    try:
        os.utime(os.path.join(seed, USED_FILE))
    except OSError:
        pass
    say(f"{kind}: seeded {tdir} from the cargo seed (APFS clone, third-party crates only)")


def top_level_dirs(path):
    try:
        return {n for n in os.listdir(path) if os.path.isdir(os.path.join(path, n)) and not os.path.islink(os.path.join(path, n))}
    except OSError:
        return set()


def harvest(kind, tdir, root):
    """Make the seed hold this lane's third-party output.

    A seed whose key matches is only ever ADDED to, one top-level directory at a time: the host
    `target` is filled by `make check` (`debug/`) and by an ARM dev build (`arm-.../`) in different
    lanes, and neither should evict the other. A seed with another key is replaced whole.
    """
    if not eligible(kind, root) or not os.path.isdir(tdir):
        return
    base = seeds_root()
    os.makedirs(base, exist_ok=True)
    seed = os.path.join(base, kind)
    key = seed_key(root, kind)
    if key is None:
        return
    sweep_leftovers(base)
    current = read_bytes(os.path.join(seed, KEY_FILE)).decode(errors="replace").strip() == key
    missing = top_level_dirs(tdir) - top_level_dirs(seed) if current else top_level_dirs(tdir)
    if current and not missing:
        try:
            os.utime(os.path.join(seed, USED_FILE))
        except OSError:
            pass
        return  # nothing this lane has that the seed lacks
    if not missing or os.stat(tdir).st_dev != os.stat(base).st_dev:
        return
    local = local_packages(root)
    if local is None:
        say(f"{kind}: not harvested (Cargo.lock lists no packages, so what is local cannot be told)")
        return
    matcher = local_matcher(local)
    with Flock(os.path.join(base, f"{kind}.lock"), exclusive=True) as lock:
        if not lock.held:
            return  # somebody is restoring/harvesting: try next time
        tmp = os.path.join(base, f"{kind}.tmp.{os.getpid()}")
        old = os.path.join(base, f"{kind}.old.{os.getpid()}")
        shutil.rmtree(tmp, ignore_errors=True)
        with CargoLocks(tdir) as cargo:
            if not cargo.idle:
                return  # cargo is mid-build in this tree: try next time
            try:
                clone_tree(tdir, tmp)
            except OSError as e:
                shutil.rmtree(tmp, ignore_errors=True)
                say(f"{kind}: not harvested ({e.strerror or e})")
                return
        strip_app(tmp, matcher)
        if current:
            for name in sorted(missing):
                if os.path.isdir(os.path.join(tmp, name)):
                    os.rename(os.path.join(tmp, name), os.path.join(seed, name))
            shutil.rmtree(tmp, ignore_errors=True)
            verb = "extended"
        else:
            with open(os.path.join(tmp, KEY_FILE), "w") as fh:
                fh.write(key + "\n")
            open(os.path.join(tmp, USED_FILE), "w").close()
            verb = "refreshed" if os.path.lexists(seed) else "created"
            if os.path.lexists(seed):
                os.rename(seed, old)
            os.rename(tmp, seed)
            shutil.rmtree(old, ignore_errors=True)
    say(f"{kind}: {verb} the cargo seed from {tdir}")


def prune(days, dry):
    """Delete seeds not used for `days` days, and interrupted-run leftovers older than a day.

    Run by `tools/build-gc.sh --cache`. A seed is "used" when a lane restores from it or finds it
    fresh at the end of a build, which touches its `.last-used`. Takes the seed's own exclusive
    lock, so a restore or harvest in flight keeps its seed (reported as in use, not removed).
    """
    base = seeds_root()
    try:
        names = sorted(os.listdir(base))
    except OSError:
        return
    now = time.time()
    for name in names:
        path = os.path.join(base, name)
        if name.endswith(".lock") or not os.path.isdir(path):
            continue
        leftover = any(m in name for m in LEFTOVER_MARKS)
        stamp = os.path.join(path, USED_FILE)
        try:
            age = now - os.lstat(stamp if os.path.exists(stamp) else path).st_mtime
        except OSError:
            continue
        if age <= (LEFTOVER_MAX_AGE if leftover else days * 86400):
            continue
        kb = (run_text(["du", "-sk", path]) or "0").split()[0]
        size = f"{int(kb) / 1024:.0f} MB" if kb.isdigit() else "?"
        with Flock(os.path.join(base, f"{name.split('.')[0]}.lock"), exclusive=True) as lock:
            if not lock.held:
                print(f"  in use, skipped  {path}")
                continue
            if dry:
                print(f"  would remove {size:>8}  {path}")
                continue
            dead = f"{path}.tombstone.{os.getpid()}"
            try:
                os.rename(path, dead)
            except OSError:
                continue
            shutil.rmtree(dead, ignore_errors=True)
            print(f"  removed {size:>8}  {path}")


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="action", required=True)
    for action in ("restore", "harvest"):
        p = sub.add_parser(action)
        p.add_argument("kind")
        p.add_argument("tdir")
        p.add_argument("--root", default=default_root(), help="repository root (default: this script's)")
        p.add_argument("--env-var", help="if set in the environment, names the real target dir (CARGO_TARGET_DIR)")
        p.add_argument("--env-base", help="directory a relative --env-var value is relative to")
    p = sub.add_parser("prune")
    p.add_argument("--days", type=int, default=30)
    p.add_argument("--dry-run", action="store_true")
    a = ap.parse_args(argv)
    try:
        if a.action == "prune":
            prune(a.days, a.dry_run)
        else:
            tdir = resolve_tdir(a.tdir, a.env_var, a.env_base)
            (restore if a.action == "restore" else harvest)(a.kind, tdir, os.path.abspath(a.root))
    except Exception as e:  # noqa: BLE001 - the seed must never fail a build
        say(f"{getattr(a, 'kind', 'cache')}: {a.action} skipped ({type(e).__name__}: {e})")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
