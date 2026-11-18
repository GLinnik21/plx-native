#!/usr/bin/env python3
"""Prepare per-checkout Cargo target links on a locally configured build volume."""
import argparse
import ctypes
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile

CONFIG = Path.home() / ".config/plxnative/build-root"
HOOK = """#!/bin/sh
# plxnative-build-root v1
case "$1" in *[!0]*) exit 0 ;; esac
exec python3 "$(dirname "$0")/plxnative-build-root.py" prepare
"""
TARGETS = ("target", "target-fast", "target-hostsim", "target-sim", "target-sim-lab",
           "target-sim-shots", "target-sim-video", "target-fuzz", "target-macapp",
           "target-release", "target-storage", "target-release-storage")


def git(cwd, *args):
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_") or k.startswith("GIT_CONFIG")}
    return subprocess.check_output(["git", "-C", cwd, *args], env=env, text=True).rstrip("\n")


def atomic_write(path, data, mode):
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as f:
        f.write(data)
        os.fchmod(f.fileno(), mode)
    os.replace(f.name, path)


def install(checkout, root):
    if not root.is_absolute() or not root.is_dir() or root != root.resolve():
        raise ValueError("the build root must be an existing, absolute physical directory")
    if root == checkout or checkout in root.parents or root in checkout.parents:
        raise ValueError("choose a dedicated directory outside the checkout")
    if CONFIG.exists() and CONFIG.read_text().strip() != str(root):
        raise ValueError(f"{CONFIG} already names a different build root")
    hooks = Path(git(checkout, "rev-parse", "--path-format=absolute", "--git-path", "hooks"))
    common = Path(git(checkout, "rev-parse", "--path-format=absolute", "--git-common-dir"))
    if hooks != common / "hooks":
        raise ValueError("custom hooksPath configured; refusing to replace it")
    hook = hooks / "post-checkout"
    if os.path.lexists(hook) and (hook.is_symlink() or hook.read_text() != HOOK):
        raise ValueError(f"existing hook at {hook}")
    CONFIG.parent.mkdir(parents=True, exist_ok=True)
    hooks.mkdir(exist_ok=True)
    atomic_write(CONFIG, (str(root) + "\n").encode(), 0o600)
    atomic_write(hooks / "plxnative-build-root.py", Path(__file__).read_bytes(), 0o644)
    exclude = common / "info/exclude"
    exclude.parent.mkdir(exist_ok=True)
    # Older branches ignore only directories. These links must also stay out of their index.
    with exclude.open("a+") as f:
        f.seek(0)
        existing = f.read().splitlines()
        for pattern in ("/rust-modules/target", "/rust-modules/target-*", "/fuzz/target"):
            if pattern not in existing:
                f.write("\n" + pattern + "\n")
    atomic_write(hook, HOOK.encode(), 0o755)


def install_harness(checkout, harness):
    common = Path(git(checkout, "rev-parse", "--path-format=absolute", "--git-common-dir"))
    helper = common / "hooks/plxnative-build-root.py"
    if not helper.is_file():
        raise ValueError("install the clone-local hook first")
    # Native isolated sessions can bypass post-checkout. Use the installed, trusted
    # copy so the same hook works on older branches and ignores unrelated clones.
    if harness == "claude":
        settings = Path.home() / ".claude/settings.json"
        events = ("SessionStart", "CwdChanged", "SubagentStart")
    else:
        settings = Path.home() / ".codex/hooks.json"
        events = ("SessionStart", "SubagentStart")
    settings.parent.mkdir(parents=True, exist_ok=True)
    config = json.loads(settings.read_text()) if settings.exists() else {}
    command = "python3 " + shlex.quote(str(helper)) + (" claude-event" if harness == "claude" else " session-event")
    entry = {"hooks": [{"type": "command", "command": command}]}
    for event in events:
        entries = config.setdefault("hooks", {}).setdefault(event, [])
        if entry not in entries:
            entries.append(entry)
    atomic_write(settings, (json.dumps(config, indent=2, ensure_ascii=False) + "\n").encode(), 0o600)


def session_event():
    event = json.load(sys.stdin)
    cwd = event.get("new_cwd") or event.get("cwd")
    if not cwd or not CONFIG.is_file():
        return
    common = Path(__file__).resolve().parent.parent
    result = subprocess.run(["git", "-C", cwd, "rev-parse", "--path-format=absolute", "--git-common-dir",
                             "--show-toplevel"], capture_output=True, text=True)
    if result.returncode:
        return
    repo, top = result.stdout.splitlines()
    if Path(repo).resolve() == common:
        prepare(Path(top).resolve())


def location(checkout):
    common = Path(git(checkout, "rev-parse", "--path-format=absolute", "--git-common-dir"))
    if not (common / "hooks/plxnative-build-root.py").is_file():
        return None
    root = Path(CONFIG.read_text().strip())
    if not root.is_absolute() or not root.is_dir() or root.resolve() != root:
        raise ValueError(f"configured build root unavailable: {root}")
    if root.parts[1:2] == ("Volumes",) and not os.path.ismount(Path(*root.parts[:3])):
        raise ValueError(f"build volume is unmounted: {root}")
    if checkout.stat().st_dev == root.stat().st_dev and root.parts[1:2] == ("Volumes",):
        return None  # A checkout already on this DAS keeps its native paths.
    admin = Path(git(checkout, "rev-parse", "--absolute-git-dir"))
    directory = root / hashlib.sha256(os.fsencode(admin)).hexdigest()
    # Earlier installs keyed the directory by checkout path. Adopt the existing
    # directory in place, including after Git has moved its checkout links.
    legacy = root / hashlib.sha256(os.fsencode(checkout)).hexdigest()
    link = checkout / "rust-modules/target"
    if link.is_symlink():
        dest = Path(os.readlink(link))
        if dest.name == "target" and dest.parent.parent == root:
            legacy = dest.parent
    marker = legacy / ".checkout"
    if (not legacy.is_symlink() and not marker.is_symlink() and marker.is_file()
            and hashlib.sha256(marker.read_bytes()).hexdigest() == legacy.name):
        identity = legacy / ".git-dir"
        if (identity.is_file() and identity.read_bytes() == os.fsencode(admin)
                or not identity.exists() and (marker.read_bytes() == os.fsencode(checkout)
                                              or not os.path.lexists(os.fsdecode(marker.read_bytes())))):
            directory = legacy
    return directory, admin


def prepare(checkout, tree=None, migrate=False):
    if not CONFIG.exists():
        return
    destination = location(checkout)
    if destination is None:
        return
    directory, admin = destination
    module = checkout / "rust-modules"
    if module.is_symlink() or not (module / "Cargo.toml").is_file():
        return
    names = set(TARGETS)
    if tree is not None:
        names = {tree.name} if tree.parent == module and tree.name in TARGETS else set()
        if tree == module / "target":
            names.add("target-storage")
    if migrate and tree is None:
        names.update(p.name for p in module.glob("target*") if p.is_dir())
    if any(not re.fullmatch(r"target(?:-[a-zA-Z0-9_-]+)?", n) for n in names):
        raise ValueError("unexpected Cargo target directory name")
    sources = {module / name: name for name in names}
    fuzz = checkout / "fuzz"
    if tree in (None, fuzz / "target") and not fuzz.is_symlink() and (fuzz / "Cargo.toml").is_file():
        sources[fuzz / "target"] = "fuzz-target"
    if not sources:
        return  # Explicit external overrides keep their own target location.
    if directory.is_symlink():
        raise ValueError(f"unexpected symlink: {directory}")
    directory.mkdir(mode=0o700, exist_ok=True)
    # Lock the directory inode. Setup, hook retries and migration share this lock.
    fd = os.open(directory, os.O_RDONLY)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        owner = directory / ".checkout"
        identity = directory / ".git-dir"
        if identity.is_symlink() or owner.is_symlink():
            raise ValueError(f"unexpected identity link at {directory}")
        if identity.exists() and identity.read_bytes() != os.fsencode(admin):
            raise ValueError(f"worktree identity mismatch at {directory}")
        if not identity.exists():
            atomic_write(identity, os.fsencode(admin), 0o600)
        if not owner.exists() or (owner.read_bytes() != os.fsencode(checkout)
                                  and directory.name == hashlib.sha256(os.fsencode(admin)).hexdigest()):
            atomic_write(owner, os.fsencode(checkout), 0o600)
        tracked = git(checkout, "ls-files", "--", *[str(src.relative_to(checkout)) for src in sources])
        if tracked:
            raise ValueError("a selected target contains tracked files")
        for src, name in sorted(sources.items()):
            dst = directory / name
            if dst.is_symlink():
                raise ValueError(f"unexpected destination link: {dst}")
            if src.is_symlink():
                if Path(os.readlink(src)) != dst or not dst.is_dir():
                    raise ValueError(f"unexpected or broken target link: {src}")
                if migrate and os.path.lexists(src.with_name("." + src.name + ".plx-original")):
                    move_idle(checkout, src, dst)
                if tree == src:
                    with tempfile.TemporaryFile(dir=dst):
                        pass  # Check the actual harness can write through the link.
                continue
            if src.exists():
                if migrate:
                    move_idle(checkout, src, dst)
                elif tree == src:
                    raise ValueError(f"{src} needs migration before building")
                continue
            dst.mkdir(exist_ok=True)
            src.symlink_to(dst, target_is_directory=True)
    finally:
        os.close(fd)


def move_idle(checkout, src, dst):
    # Only the explicit rollout command copies or reclaims data. Setup hooks never do.
    if not src.is_dir():
        raise ValueError(f"unexpected target type: {src}")
    tools = Path(__file__).resolve().parent
    def idle(path):
        cmd = [sys.executable, str(tools / "disk-budget.py"), "probe", "--root", str(checkout),
               "--tree", str(path), "--checkout", str(checkout), "--class", "lane"]
        result = subprocess.run(cmd, capture_output=True, text=True)
        if result.returncode:
            print(f"deferred {src}: {result.stdout.strip() or result.stderr.strip()}", file=sys.stderr)
        return result.returncode == 0
    if not idle(src):
        return
    if sys.platform != "darwin":
        raise ValueError("live tree migration requires macOS atomic path exchange")
    import runpy
    cold = runpy.run_path(str(tools / "cold-store.py"))
    backup = src.with_name("." + src.name + ".plx-original")
    if src.is_symlink():
        if src.resolve() != dst or backup.is_symlink() or not backup.is_dir():
            raise ValueError(f"previous migration needs inspection: {backup}")
    else:
        if os.path.lexists(backup) and not (backup.is_symlink() and Path(os.readlink(backup)) == dst):
            raise ValueError(f"previous migration needs inspection: {backup}")
        if not dst.exists():
            with tempfile.TemporaryDirectory(prefix=".copy-", dir=dst.parent) as temp:
                copied = Path(temp) / "tree"
                cold["copy_tree"](str(src), str(copied))
                copied.rename(dst)
        # Retry after interruption between publishing the copy and switching.
        if not identical(src, dst):
            raise ValueError(f"source and DAS copy differ; both retained: {src}, {dst}")
        if not idle(src):
            return
        swap = ctypes.CDLL(None, use_errno=True).renamex_np
        if not backup.is_symlink():
            backup.symlink_to(dst)
        if swap(os.fsencode(src), os.fsencode(backup), 2) != 0:
            backup.unlink()
            raise OSError(ctypes.get_errno(), f"cannot exchange {src} and {backup}")
    if not idle(backup):
        print(f"retained original: {backup}", file=sys.stderr)
        return
    if not identical(backup, dst):
        raise ValueError(f"tree changed during cutover; retained original: {backup}")
    cold["flush_cold_volume"](str(dst.parent / ".checkout"))
    shutil.rmtree(backup)
    print(f"migrated {src}", flush=True)


def identical(src, dst):
    # Include extra files, symlink targets, file modes and content checksums.
    changes = subprocess.check_output(["rsync", "-nrcpli", "--delete", str(src) + "/", str(dst) + "/"])
    return not changes


def inventory(checkout, orphans):
    if not CONFIG.is_file():
        return
    common = Path(git(checkout, "rev-parse", "--path-format=absolute", "--git-common-dir"))
    if not (common / "hooks/plxnative-build-root.py").is_file():
        return
    root = Path(CONFIG.read_text().strip())
    if not root.is_absolute() or not root.is_dir() or root.resolve() != root:
        return
    registered = {line[9:] for line in git(checkout, "worktree", "list", "--porcelain").splitlines()
                  if line.startswith("worktree ")}
    # Keep pre-upgrade caches referenced by a moved checkout even before its
    # first prepare call has written the Git worktree identity.
    linked = set()
    for path in registered:
        for link in (Path(path) / "rust-modules").glob("target*"):
            if link.is_symlink():
                linked.add(Path(os.readlink(link)).parent)
    for directory in sorted(root.iterdir()):
        marker = directory / ".checkout"
        if (not re.fullmatch(r"[a-f0-9]{64}", directory.name) or directory.is_symlink()
                or marker.is_symlink() or not marker.is_file()):
            continue
        owner = marker.read_bytes()
        identity = directory / ".git-dir"
        admin = identity.read_bytes() if not identity.is_symlink() and identity.is_file() else None
        if (hashlib.sha256(owner).hexdigest() != directory.name
                and (not admin or hashlib.sha256(admin).hexdigest() != directory.name)):
            continue
        path = os.fsdecode(owner)
        gone = (directory not in linked and path not in registered and not os.path.lexists(path)
                and not (admin and os.path.lexists(os.fsdecode(admin))))
        if orphans:
            if gone:
                for tree in directory.iterdir():
                    if ((tree.name == "fuzz-target" or re.fullmatch(r"target(?:-[a-zA-Z0-9_-]+)?", tree.name))
                            and not tree.is_symlink() and tree.is_dir()):
                        print(tree)
        else:
            print(f"  {path} -> {directory}" + (" (orphan)" if gone else ""))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("install", "prepare", "migrate", "inventory", "orphans", "install-claude", "install-codex", "session-event", "claude-event"))
    parser.add_argument("path", type=Path, nargs="?")
    args = parser.parse_args()
    if args.command in ("claude-event", "session-event"):
        session_event()
        return
    checkout = Path(git(Path.cwd(), "rev-parse", "--show-toplevel")).resolve()
    if args.command == "install":
        if args.path is None:
            parser.error("install requires the existing build root directory")
        install(checkout, args.path)
    elif args.command in ("install-claude", "install-codex"):
        install_harness(checkout, args.command.split("-")[1])
    elif args.command in ("inventory", "orphans"):
        inventory(checkout, args.command == "orphans")
    else:
        prepare(checkout, args.path, args.command == "migrate")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"build-root: {error}")
