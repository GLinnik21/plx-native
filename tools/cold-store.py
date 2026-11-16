#!/usr/bin/env python3
"""The cold store: park idle derived build trees on a second volume (a big external disk) and bring one
back when its owner builds again, so a full internal SSD is relieved without a rebuild from nothing.

    cold-store.py status
    cold-store.py list
    cold-store.py park   PATH [-n] [--kind lane|fleet|tmp]
    cold-store.py unpark PATH [-n]
    cold-store.py prune  [-n]
    cold-store.py recover

OFF UNLESS CONFIGURED, and an unconfigured machine is untouched: every entry point returns before it creates
a file or directory anywhere (ci/test_cold_store.py walks the temp tree to prove it). The per-user file
`cold-store` lives in the directory tools/tv-config.sh resolves (PLX_TV_CONFIG_DIR, else
XDG_CONFIG_HOME/plxnative, else ~/.config/plxnative); `key=value` lines, `#` comments, nothing secret in it:

    root=/Volumes/DAS/plxnative-cold   required: the directory the copies live under
    max_days=14                        a parked copy older than this is pruned
    min_free_gib=200                   the cold volume must keep this much free; under it nothing is parked and
                                       prune drops the oldest copies until it is back over
    park_below_gib=40                  W3: park ahead of the ladder when the build volume is under this (and still
                                       at least the comfort line: under it the ladder deletes, nothing parks)
    park_idle_min=120                  W3: a tree must have been idle this long to be parked
    park_max_gib_per_run=20            W3: the most one --auto pass parks
    rate_floor_mibs=40                 the fastest copy rate the unpark cost rule will assume (MiB/s)
    rebuild_secs_<dirname>=N           seconds a COLD rebuild of that tree directory takes (the cost rule
                                       compares the unpark copy time with it); DEFAULT_REBUILD_SECS below

Environment: PLX_COLD_STORE=off disables it for one command, PLX_COLD_STORE=<path> sets the root for one
command, PLX_COLD_STORE_CONFIG=<file> names the config file (the test seam), PLX_BUILD_CACHE (default
~/.cache/plxnative) holds the SSD side. PLX_COLD_STORE_TEST_FREE_KIB / PLX_COLD_STORE_TEST_NOW pretend the cold
volume's free space / the time and are REFUSED unless PLX_BUILD_CACHE and PLX_COLD_STORE_CONFIG (or a
PLX_COLD_STORE root) are named too: a pretend emergency over the real store would park real trees.

LAYOUT. SSD side, under $PLX_BUILD_CACHE/cold-store/ (same volume as the trees, so a rename is atomic):
    index/<escaped origin>.json   one record per parked tree; `/` -> `%2F`, `%` -> `%25`, no hashing
    parking/<id>/                 the tree, renamed out of its checkout while it is copied (outside rust-modules/
                                  on purpose: a sibling there would match `target*` and be inventoried)
    parking/<id>.json, <id>.pid   the record written BEFORE the rename, and the owner's pid
    index/<id>.json.unparking.<pid>   the index record while process <pid> restores that tree: a CLAIM (see below)
Cold side, under <root>/:
    trees/<id>/                   the copy; <id> is the escaped origin path
    trees/<id>.json               the same record, beside it (the SSD index can be rebuilt from these)
    trees/<id>.partial            a copy in progress; <id>.dead.<pid>.<ns> / <id>.consumed.<pid>.<ns>: awaiting a reap
An unpark copies into `<parent>/.<name>.unpark.<pid>` (a hidden sibling: `target*` globs do not match it).

THE PARK PROTOCOL. (1) the caller's liveness callback is asked again; (2) rename the tree into parking/<id>/
(refused when that is another volume); (3) copy to trees/<id>.partial and verify file count + apparent bytes,
record the cold side, rename to trees/<id>/, then FLUSH the cold volume once (`sync` and F_FULLFSYNC through the
record file: a copy still in the drive's write cache is not one to delete the SSD side for); (4) write the SSD
index record; (5) delete the parking copy. A failing step from the record write to the index rolls back: the tree
is renamed back into its checkout, a renamed-in copy becomes `<id>.dead.*` debris, and the outcome is FAILED. A dead
owner's residue is settled by `recover`: a complete cold copy of THIS park -> delete the SSD copy; else the
origin absent -> rename back; else delete the SSD copy. THE UNPARK PROTOCOL only fills an ABSENT tree, only when
the copy is cheaper than a rebuild (`apparent_bytes / min(rate_floor, park_rate) < rebuild_secs`) and the caller
says the free space after it stays over its line; it copies beside the origin, verifies, renames into place and
consumes the record. THE CLAIM: the deciding half (`claim`) ends with an atomic rename of the index record to
`index/<id>.json.unparking.<pid>`, and only the copy half (`unpark(..., claimed=record)`) is long, so a caller
that serialises its decisions behind a lock holds it for the decision and the rename, not for the copy. A claimed
entry is invisible to `records()` / `indexed()` (a second unparker finds nothing to take and is told who has it),
to `recover`'s adoption and to the park pass (`park` refuses an origin with a live claim); the free-space question
of a later claim counts the bytes the live claims are still to copy (`inflight_bytes`). A failed or declined copy
renames the record back (never over a newer one); a claimer that died leaves a claim whose pid is dead, which the
next `claim` of that origin and `recover` put back as the index entry. Python standard library only; `ditto` is
the one copy primitive (macOS), replaced by tests.
"""
import argparse
import errno
import fcntl
import json
import os
import re
import shutil
import subprocess
import sys
import time
from typing import Callable, Dict, Iterable, List, Optional, Set, Tuple

GIB = 1 << 30
MIB = 1 << 20
SCHEMA = 1
PARKED, UNPARKED, DRY_RUN, CLAIMED = "parked", "unparked", "dry-run", "claimed"
SKIPPED, REFUSED, DECLINED, FAILED = "skipped", "refused", "declined", "failed"

DEFAULTS = {"max_days": 14.0, "min_free_gib": 200.0, "park_below_gib": 40.0, "park_idle_min": 120.0,
            "park_max_gib_per_run": 20.0, "rate_floor_mibs": 40.0}
# Seconds of COLD rebuild per tree directory. W0 measured on the owner's Mac a warm `make check-nodef` at 16 s
# (212 MiB) and `lint` 25 s (239 MiB), `target-fast` incremental ~25 s (804 MiB, build only); a lane's real tree
# is bigger and a cold one also recompiles the workspace crates, so these are deliberately high: a wrong guess
# that unparks a tree costs seconds of copying, a wrong guess that declines costs a rebuild the copy would have won.
DEFAULT_REBUILD_SECS = {"target": 300.0, "target-fast": 120.0, "target-hostsim": 300.0, "target-sim": 180.0,
                        "target-release": 600.0, "target-storage": 30.0}
FALLBACK_REBUILD_SECS = 120.0       # a directory name not in the table (a /tmp tree's own name)
TEST_ENV = ("PLX_COLD_STORE_TEST_FREE_KIB", "PLX_COLD_STORE_TEST_NOW")
# The names the reap deletes, matched as a whole SUFFIX: an escaped origin path can carry ".dead." or ".consumed."
# in a directory name of its own, and a loose substring match would reap a live copy. A set-aside is named
# `<id>.dead.<pid>.<ns>` / `<id>.consumed.<pid>.<ns>`: unique, so a name an earlier set-aside left behind (or an
# older version's `<id>.dead.<pid>` and plain `<id>.consumed`) can never make the next rename fail.
DEBRIS_SUFFIX = re.compile(r"\.(dead\.\d+(\.\d+)?|consumed(\.\d+\.\d+)?)$")
# An unparker's claim on an index record: `<id>.json` + CLAIM_INFIX + pid. disk-budget.py's fast path looks for the
# same infix (COLD_CLAIM_INFIX there; ci/test_disk_preflight.py pins the two equal).
CLAIM_INFIX = ".unparking."
CLAIM_NAME = re.compile(r"(.+\.json)" + re.escape(CLAIM_INFIX) + r"(\d+)")


# --- small helpers (the stubbable seams are device_of, free_bytes_of, which_ditto and copy_tree) -----------
def human(n: float) -> str:
    n = float(n)
    for unit, size in (("GiB", GIB), ("MiB", MIB), ("KiB", 1024)):
        if n >= size:
            return "%.1f %s" % (n / size, unit)
    return "%d B" % n


def config_dir(env) -> str:
    """tools/tv-config.sh:18-25 in Python: PLX_TV_CONFIG_DIR, else XDG_CONFIG_HOME/plxnative, else ~/.config/plxnative
    (an empty value counts as unset, like the shell's `:-`)."""
    explicit = env.get("PLX_TV_CONFIG_DIR")
    if explicit:
        return explicit
    xdg = env.get("XDG_CONFIG_HOME") or os.path.join(os.path.expanduser("~"), ".config")
    return os.path.join(xdg, "plxnative")


def build_cache(env) -> str:
    return os.path.realpath(env.get("PLX_BUILD_CACHE") or os.path.expanduser("~/.cache/plxnative"))


def escape(path: str) -> str:
    """A path as one file name, reversible and hash-free: `%` -> `%25`, `/` -> `%2F`. A name over 230 bytes (the
    file system's limit is 255) keeps its tail and a crc32 of the whole path."""
    name = path.replace("%", "%25").replace("/", "%2F")
    if len(name) > 230:
        import zlib
        name = name[-200:] + "~" + format(zlib.crc32(path.encode()), "08x")
    return name


def norm(path: str) -> str:
    """The key a tree is known by: its parent resolved, its own name kept (a symlinked tree is never parked)."""
    path = os.path.abspath(path)
    return os.path.join(os.path.realpath(os.path.dirname(path)), os.path.basename(path))


def nearest_existing(path: str) -> str:
    while path and not os.path.lexists(path):
        parent = os.path.dirname(path)
        if parent == path:
            break
        path = parent
    return path


def device_of(path: str) -> int:
    return os.stat(path).st_dev


def free_bytes_of(path: str, env) -> int:
    override = env.get("PLX_COLD_STORE_TEST_FREE_KIB", "")
    if override.isdigit() and not refuse_test_env(env):
        return int(override) * 1024
    st = os.statvfs(path)
    return st.f_bavail * st.f_frsize


def which_ditto() -> Optional[str]:
    return shutil.which("ditto")


def copy_tree(src: str, dst: str) -> None:
    """The one copy primitive. `ditto` keeps mode, mtime, owner and hard links (man ditto), and `--norsrc` leaves
    out the resource forks, extended attributes and ACLs a build tree has no use for (and a non-APFS volume
    would store as `._` files). `dst` must not exist; its parent must."""
    subprocess.run(["ditto", "--norsrc", src, dst], check=True, stdin=subprocess.DEVNULL,
                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


def flush_cold_volume(path: str) -> None:
    """Make what was just written to the cold volume durable, once, before anything on the SSD is deleted for it.
    `ditto` returns with the copy in the page cache and the USB drive's own write cache, and a power cut or a
    yanked cable then loses it with the SSD copy already gone. `sync` first pushes every dirty buffer to the
    devices, then `F_FULLFSYNC` on `path` (a file just written on the cold volume) flushes that drive's whole cache,
    which a plain `fsync` does not on macOS; one file stands for the volume, so the cost is one flush, not one per
    copied file (measured on the owner's disk: fsync 5-9 s under load, so a per-file one would run for hours). A
    volume that cannot do `F_FULLFSYNC` (or a platform without it) takes `fsync`, the strongest it has; any other
    failure is raised: the caller must not delete the SSD copy on a flush that did not happen."""
    os.sync()
    fd = os.open(path, os.O_RDONLY)
    try:
        full = getattr(fcntl, "F_FULLFSYNC", None)
        if full is None:
            os.fsync(fd)
            return
        try:
            fcntl.fcntl(fd, full)
        except OSError as exc:
            if exc.errno not in (errno.ENOTSUP, errno.EINVAL, errno.ENOTTY):
                raise
            os.fsync(fd)
    finally:
        os.close(fd)


def pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def write_json(path: str, obj) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = "%s.tmp.%d" % (path, os.getpid())
    try:
        with open(tmp, "w") as fh:
            json.dump(obj, fh, sort_keys=True, indent=1)
            fh.write("\n")
        os.replace(tmp, path)
    except BaseException:
        unlink(tmp)                              # this pid is alive, so the reap would not take it for debris
        raise


def read_json(path: str) -> Optional[dict]:
    try:
        with open(path) as fh:
            data = json.load(fh)
    except (OSError, ValueError):
        return None
    return data if isinstance(data, dict) else None


def unlink(path: str) -> None:
    try:
        os.unlink(path)
    except OSError:
        pass


def remove(path: str) -> None:
    """Delete a file, symlink or directory tree; a missing path is fine."""
    if os.path.isdir(path) and not os.path.islink(path):
        shutil.rmtree(path, ignore_errors=True)
    else:
        unlink(path)


def tree_stats(path: str) -> Tuple[int, int]:
    """(entries that are not directories, their summed lstat sizes): the count and the apparent bytes a copy
    must reproduce. A hard-linked name counts once per name, as a faithful copy keeps the names."""
    files = size = 0
    stack = [path]
    while stack:
        with os.scandir(stack.pop()) as it:
            for entry in it:
                if entry.is_dir(follow_symlinks=False):
                    stack.append(entry.path)
                else:
                    files += 1
                    size += entry.stat(follow_symlinks=False).st_size
    return files, size


def private_size(path: str) -> int:
    """Bytes a deletion of `path` frees: allocated blocks of the inodes whose every name is inside it. The caller
    (disk-budget's inventory, which also credits APFS clone sharing) can pass a better number to park()."""
    inodes: Dict[Tuple[int, int], List[int]] = {}
    stack = [path]
    while stack:
        with os.scandir(stack.pop()) as it:
            for entry in it:
                st = entry.stat(follow_symlinks=False)
                if entry.is_dir(follow_symlinks=False):
                    stack.append(entry.path)
                    continue
                row = inodes.setdefault((st.st_dev, st.st_ino), [st.st_blocks * 512, st.st_nlink, 0])
                row[2] += 1
    return sum(r[0] for r in inodes.values() if r[2] >= r[1])


def refuse_test_env(env) -> str:
    """Why a mutating call must not run, or "". A pretend free-space or clock override (TEST_ENV) over the DEFAULT
    roots would act on the machine's real trees as if it were the emergency (disk-budget.refuse_pretend_emergency)."""
    set_names = [n for n in TEST_ENV if env.get(n)]
    if not set_names:
        return ""
    missing = []
    if not env.get("PLX_BUILD_CACHE"):
        missing.append("PLX_BUILD_CACHE")
    if not (env.get("PLX_COLD_STORE_CONFIG") or env.get("PLX_COLD_STORE") not in (None, "", "off")):
        missing.append("PLX_COLD_STORE_CONFIG")
    if missing:
        return ("refusing to run under %s while %s is left to its default: a pretend emergency over the real "
                "store is how a test would park the real trees. Point every root at a disposable directory."
                % ("+".join(set_names), "+".join(missing)))
    return ""


def clock(env) -> float:
    value = env.get("PLX_COLD_STORE_TEST_NOW", "")
    if value and not refuse_test_env(env):
        try:
            return float(value)
        except ValueError:
            pass
    return time.time()


# --- configuration ---------------------------------------------------------------------------------------
class Config:
    def __init__(self, root: str, values: Dict[str, float], rebuild: Dict[str, float]):
        self.root = root
        self.max_days = values["max_days"]
        self.min_free_gib = values["min_free_gib"]
        self.park_below_gib = values["park_below_gib"]
        self.park_idle_min = values["park_idle_min"]
        self.park_max_gib_per_run = values["park_max_gib_per_run"]
        self.rate_floor_mibs = values["rate_floor_mibs"]
        self.rebuild = rebuild

    def rebuild_secs(self, dirname: str) -> float:
        return self.rebuild.get(dirname, FALLBACK_REBUILD_SECS)


def load_config(env) -> Tuple[Optional[Config], str]:
    """(config, "") or (None, why it is off). Reads only; creates nothing."""
    if env.get("PLX_COLD_STORE") == "off":
        return None, "disabled (PLX_COLD_STORE=off)"
    path = env.get("PLX_COLD_STORE_CONFIG") or os.path.join(config_dir(env), "cold-store")
    raw: Dict[str, str] = {}
    try:
        with open(path) as fh:
            for line in fh:
                line = line.strip()
                if line and not line.startswith("#") and "=" in line:
                    key, _, value = line.partition("=")
                    raw[key.strip()] = value.strip()
    except OSError:
        pass
    root = env.get("PLX_COLD_STORE") or raw.get("root", "")
    if not root:
        return None, "not configured"
    root = os.path.expanduser(root)
    if not os.path.isabs(root):
        return None, "the cold-store root is not an absolute path"
    values = dict(DEFAULTS)
    rebuild = dict(DEFAULT_REBUILD_SECS)
    for key, value in raw.items():
        try:
            if key in values:
                values[key] = float(value)
            elif key.startswith("rebuild_secs_"):
                rebuild[key[len("rebuild_secs_"):]] = float(value)
        except ValueError:
            pass
    return Config(os.path.normpath(root), values, rebuild), ""


class PruneFacts:
    """What prune is told about the world (the caller knows the lanes; this module does no git). `gone` and
    `merged` are checkout paths; `built` maps an origin that exists to when it was last built."""

    def __init__(self, gone: Iterable[str] = (), merged: Iterable[str] = (), built: Optional[Dict[str, float]] = None):
        self.gone: Set[str] = set(gone)
        self.merged: Set[str] = set(merged)
        self.built: Dict[str, float] = dict(built or {})

    @classmethod
    def from_disk(cls, records: Iterable[dict]) -> "PruneFacts":
        """The facts the CLI can read for itself: a checkout that is no longer a directory is gone."""
        return cls(gone=[r["checkout"] for r in records if r.get("checkout") and not os.path.isdir(r["checkout"])])


class Outcome:
    def __init__(self, status: str, reason: str = "", record: Optional[dict] = None, secs: float = 0.0):
        self.status, self.reason, self.record, self.secs = status, reason, record, secs

    @property
    def ok(self) -> bool:
        return self.status in (PARKED, UNPARKED, DRY_RUN)

    def __repr__(self) -> str:
        return "Outcome(%s%s)" % (self.status, ": " + self.reason if self.reason else "")


# --- the store ---------------------------------------------------------------------------------------------
class ColdStore:
    def __init__(self, env=None, log: Optional[Callable[[str], None]] = None):
        self.env = os.environ if env is None else env
        self.cfg, self.off_reason = load_config(self.env)
        self.log = log or (lambda msg: print(msg, file=sys.stderr))

    # paths (computing one creates nothing)
    @property
    def ssd(self) -> str:
        return os.path.join(build_cache(self.env), "cold-store")

    def index_path(self, origin: str) -> str:
        return os.path.join(self.ssd, "index", escape(origin) + ".json")

    def parking_path(self, ident: str) -> str:
        return os.path.join(self.ssd, "parking", ident)

    def trees_path(self, ident: str = "") -> str:
        return os.path.join(self.cfg.root, "trees", ident) if ident else os.path.join(self.cfg.root, "trees")

    def configured(self) -> bool:
        return self.cfg is not None

    def indexed(self, origin: str) -> bool:
        """Is a parked copy of `origin` recorded? One stat of the SSD, no cold-volume touch: the fast path's question."""
        return self.configured() and os.path.exists(self.index_path(norm(origin)))

    def root_reachable(self) -> bool:
        """Is the cold root's volume mounted right now? Reads the mount table only, writes nothing."""
        parts = self.cfg.root.split(os.sep)
        return not (len(parts) > 2 and parts[1] == "Volumes" and not os.path.ismount(os.sep.join(parts[:3])))

    # --- is the cold volume usable ---
    def unusable(self) -> Optional[str]:
        """The reason the cold store cannot be used AT ALL right now, from the checks that touch no volume (the
        config, PATH and the mount table), or None. `available`'s first three steps: a caller that would wait
        behind a lock for the store asks this first."""
        if self.cfg is None:
            return self.off_reason
        if which_ditto() is None:
            return "ditto is not on PATH (the cold store is macOS-only)"
        if not self.root_reachable():
            return "%s is not mounted" % os.sep.join(self.cfg.root.split(os.sep)[:3])
        return None

    def available(self, build_dev: int, write_probe: bool = True) -> Optional[str]:
        """The reason the cold volume cannot be used for a tree on device `build_dev`, or None. Cheap checks first;
        the free-space and probe-write checks touch the cold volume, so only --auto and an unpark that already
        found an index record call this. `write_probe=False` (a dry run) reads only: it proves the volume is
        mounted and has room, but creates nothing on it, so it cannot prove the volume is writable."""
        why = self.unusable()
        if why:
            return why
        root = self.cfg.root
        anchor = nearest_existing(root)
        try:
            if device_of(anchor) == build_dev:
                return "the cold store is on the same volume as the build tree: parking frees nothing"
            free = free_bytes_of(anchor, self.env)
        except OSError as exc:
            return "cannot read the cold volume: %s" % exc
        if free < self.cfg.min_free_gib * GIB:
            return "the cold volume has %s free, under min_free_gib=%g" % (human(free), self.cfg.min_free_gib)
        if not write_probe:
            return None
        probe = os.path.join(root, ".probe.%d" % os.getpid())
        try:
            os.makedirs(root, exist_ok=True)
            with open(probe, "w") as fh:
                fh.write("probe\n")
            os.unlink(probe)
        except OSError as exc:
            return "cannot write to %s: %s" % (root, exc)
        return None

    # --- claims: an unparker's hold on an index record ---
    def claim_path(self, origin: str, pid: Optional[int] = None) -> str:
        """`index/<id>.json.unparking.<pid>`: where the record of `origin` lives while process `pid` (default: this
        one) restores it. `origin` is already normalised."""
        return "%s%s%d" % (self.index_path(origin), CLAIM_INFIX, os.getpid() if pid is None else pid)

    def claims(self, origin: Optional[str] = None) -> List[Tuple[int, str, str]]:
        """(pid, claim path, index path it returns to) for every claim on the SSD, or only those on `origin`.
        One listing of the index directory; creates nothing."""
        base = os.path.join(self.ssd, "index")
        only = os.path.basename(self.index_path(norm(origin))) if origin else None
        try:
            names = sorted(os.listdir(base))
        except OSError:
            return []
        out = []
        for name in names:
            match = CLAIM_NAME.fullmatch(name)
            if match and (only is None or match.group(1) == only):
                out.append((int(match.group(2)), os.path.join(base, name), os.path.join(base, match.group(1))))
        return out

    def claimed_by(self, origin: str) -> int:
        """The pid of a LIVE process that is restoring `origin`, else 0 (a dead claimer's claim is nobody's)."""
        return next((pid for pid, _, _ in self.claims(origin) if pid_alive(pid)), 0)

    def inflight_bytes(self, build_dev: int) -> int:
        """What the live claims on the build volume (device `build_dev`) are still to copy in: each one's whole
        apparent size. A copy under way has written part of that already, so this overstates and can only make a
        caller decline, which is the safe side of "does it still fit"."""
        total = 0
        for pid, claim, _ in self.claims():
            rec = read_json(claim) if pid_alive(pid) else None
            if not rec or not rec.get("origin"):
                continue
            try:
                if device_of(nearest_existing(rec["origin"])) != build_dev:
                    continue
            except OSError:
                continue
            total += int(rec.get("apparent_bytes", 0))
        return total

    @staticmethod
    def _restore_claim(claim: str, index: str) -> bool:
        """Give a claim's record back as the index entry and drop the claim, never over a newer entry of the same
        origin (a later park wrote one: that record wins and the older one is simply dropped). False when the
        claim could not be settled (already gone, or neither a link nor a rename worked)."""
        try:
            os.link(claim, index)
        except FileExistsError:
            pass
        except FileNotFoundError:
            return False                         # somebody else settled it first
        except OSError:
            try:
                if not os.path.exists(index):
                    os.rename(claim, index)      # a volume without hard links
                    return True
            except OSError:
                return False
        unlink(claim)
        return True

    def release(self, origin: str) -> None:
        """Give THIS process's claim on `origin` back as the index entry (nothing to do when it holds none)."""
        origin = norm(origin)
        self._restore_claim(self.claim_path(origin), self.index_path(origin))

    # --- records ---
    def records(self) -> List[dict]:
        """Every SSD index record, oldest first. Reads the SSD only. A claimed record is not one: it is being restored."""
        out = []
        base = os.path.join(self.ssd, "index")
        try:
            names = sorted(os.listdir(base))
        except OSError:
            return []
        for name in names:
            if name.endswith(".json"):
                rec = read_json(os.path.join(base, name))
                if rec and rec.get("origin"):
                    out.append(rec)
        return sorted(out, key=lambda r: r.get("parked_at", 0))

    def list(self) -> List[dict]:
        return self.records() if self.configured() else []

    def parking_residue(self) -> List[Tuple[str, int, bool]]:
        """(id, apparent bytes, owner alive) for every tree in parking/: what `make disk` lists."""
        out = []
        base = os.path.join(self.ssd, "parking")
        try:
            names = sorted(os.listdir(base))
        except OSError:
            return []
        for name in names:
            path = os.path.join(base, name)
            if os.path.isdir(path) and not os.path.islink(path):
                out.append((name, tree_stats(path)[1], self._owner_alive(name)))
        return out

    def _owner_alive(self, ident: str) -> bool:
        try:
            with open(self.parking_path(ident) + ".pid") as fh:
                return pid_alive(int(fh.read().strip()))
        except (OSError, ValueError):
            return False

    # --- park ---
    def park(self, tree: str, *, kind: str, checkout: str = "", head: str = "",
             live: Optional[Callable[[str], Optional[str]]] = None, private_bytes: Optional[int] = None,
             now: Optional[float] = None, dry_run: bool = False) -> Outcome:
        """Move an idle derived tree to the cold volume. `live(path)` returns a reason the tree must stay (the caller
        wires disk-budget's probe) or None; it is asked once more right before the rename. The tree is back in
        place when anything fails before the cold copy is complete."""
        if self.cfg is None:
            return Outcome(SKIPPED, self.off_reason)
        refusal = refuse_test_env(self.env)
        if refusal:
            return Outcome(REFUSED, refusal)
        origin = norm(tree)
        holder = self.claimed_by(origin)
        if holder:                               # a restore is copying this origin's record back: its tree is not ours to move
            return Outcome(REFUSED, "being restored by pid %d" % holder)
        if not os.path.isdir(origin) or os.path.islink(origin):
            return Outcome(REFUSED, "not a directory (or a symlink): %s" % origin)
        build_dev = device_of(origin)
        why = self.available(build_dev, write_probe=not dry_run)
        if why:
            return Outcome(REFUSED, why)
        parking_dir = os.path.join(self.ssd, "parking")
        if not dry_run:
            os.makedirs(parking_dir, exist_ok=True)      # a dry run creates nothing, on either volume
        if device_of(nearest_existing(parking_dir)) != build_dev:
            return Outcome(REFUSED, "the build cache (%s) is on another volume than the tree: a rename cannot park it"
                           % parking_dir)
        ident = escape(origin)
        parked = self.parking_path(ident)
        if os.path.lexists(parked):
            if self._owner_alive(ident):
                return Outcome(REFUSED, "already being parked by another process")
            self._settle_residue(ident)          # a dead owner's residue: put it right first
        if not os.path.isdir(origin):
            return Outcome(REFUSED, "the previous parking residue took the tree")
        files, apparent = tree_stats(origin)
        parked_at = now if now is not None else clock(self.env)
        record = {"schema": SCHEMA, "id": ident, "origin": origin, "kind": kind, "checkout": checkout, "head": head,
                  "parked_at": parked_at, "files": files, "apparent_bytes": apparent,
                  "private_bytes_freed": private_bytes if private_bytes is not None else private_size(origin),
                  "park_rate": 0.0, "root": self.cfg.root}
        if dry_run:
            return Outcome(DRY_RUN, "would park %s (%d files)" % (human(apparent), files), record)
        if live is not None:
            held = live(origin)
            if held:
                return Outcome(REFUSED, "not idle: %s" % held)
        write_json(parked + ".json", record)
        with open(parked + ".pid", "w") as fh:
            fh.write("%d\n" % os.getpid())
        try:
            os.rename(origin, parked)
        except OSError as exc:
            self._drop_parking_files(ident)
            return Outcome(REFUSED, "cannot move the tree out of its checkout: %s" % exc)
        partial = self.trees_path(ident + ".partial")
        started = time.time()
        try:
            os.makedirs(self.trees_path(), exist_ok=True)
            remove(partial)
            copy_tree(parked, partial)
            got = tree_stats(partial)
            if got != (files, apparent):
                raise OSError("copy verification failed: %d files / %d bytes, expected %d / %d"
                              % (got[0], got[1], files, apparent))
        except (OSError, subprocess.SubprocessError) as exc:
            remove(partial)
            self._rename_back(ident, origin)
            return Outcome(FAILED, "copy to the cold volume failed: %s" % exc)
        secs = max(time.time() - started, 0.001)
        record["park_rate"] = apparent / secs
        record["copy_secs"] = round(secs, 3)
        final = self.trees_path(ident)
        cold_json = self.trees_path(ident + ".json")
        dead = None
        wrote = published = False
        try:
            if os.path.lexists(final):           # an older copy of the same origin, recorded or not
                dead = self._aside(ident, "dead")
                os.rename(final, dead)
                unlink(cold_json)
            write_json(cold_json, record)
            wrote = True
            os.rename(partial, final)
            published = True
            flush_cold_volume(cold_json)         # the copy, its record and the rename are on the disk, not in its cache
            write_json(self.index_path(origin), record)
        except (OSError, subprocess.SubprocessError) as exc:
            self._undo_publish(ident, origin, partial, wrote, published)
            return Outcome(FAILED, "recording the cold copy failed: %s" % exc)
        remove(parked)
        self._drop_parking_files(ident)
        if dead:
            remove(dead)
        return Outcome(PARKED, "", record, secs)

    def _undo_publish(self, ident: str, origin: str, partial: str, wrote: bool, published: bool) -> None:
        """A step after the copy failed: put the tree back FIRST (a rename on the SSD, so the lane is not left
        without it while the cold disk is slow), then settle the cold side without deleting anything big. Before
        `partial` became the copy it is just a partial again; after, the copy is renamed to
        `<id>.dead.<pid>.<ns>` (a name the reap deletes; left as `<id>/` with its record, `recover` would ADOPT
        a copy of a tree that is back in its checkout). The record goes when this park wrote it. Every step is
        best effort: a failure here leaves debris for the reap, never a traceback."""
        self._rename_back(ident, origin)
        if wrote:
            unlink(self.trees_path(ident + ".json"))
        if published:
            try:
                os.rename(self.trees_path(ident), self._aside(ident, "dead"))
            except OSError:
                pass
        else:
            remove(partial)

    def _aside(self, ident: str, kind: str) -> str:
        """A fresh name to set a copy aside under, for the reap to delete later (`kind`: "dead" or "consumed")."""
        return self.trees_path("%s.%s.%d.%d" % (ident, kind, os.getpid(), time.time_ns()))

    def _drop_parking_files(self, ident: str) -> None:
        for suffix in (".json", ".pid"):
            unlink(self.parking_path(ident) + suffix)

    def _rename_back(self, ident: str, origin: str) -> None:
        try:
            os.rename(self.parking_path(ident), origin)
        except OSError:
            return                               # leave the residue for `recover`: never delete on a failed undo
        self._drop_parking_files(ident)

    # --- recovery of a dead owner's parking residue ---
    def _settle_residue(self, ident: str) -> str:
        """Settle parking/<id>/ whose owner is dead: "deleted-copy-done" (the cold copy is complete), "renamed-back",
        "deleted-stale" (the origin exists again, or its checkout is gone) or "kept" (no record names it)."""
        parked = self.parking_path(ident)
        record = read_json(parked + ".json")
        if record is None or not record.get("origin"):
            return "kept"                        # never delete a tree we cannot name
        origin = record["origin"]
        cold = read_json(self.trees_path(ident + ".json")) if self.root_reachable() else None
        # complete = a cold copy of THIS park (an older copy of the same origin is not this tree)
        complete = bool(cold) and cold.get("parked_at") == record.get("parked_at") and os.path.isdir(self.trees_path(ident))
        if complete:
            if not os.path.exists(self.index_path(origin)):
                write_json(self.index_path(origin), cold)
            remove(parked)
            result = "deleted-copy-done"
        elif not os.path.lexists(origin):
            try:
                os.rename(parked, origin)
                result = "renamed-back"
            except OSError:
                remove(parked)                   # its checkout is gone: nothing to put it back into
                result = "deleted-stale"
        else:
            remove(parked)
            result = "deleted-stale"
        if not os.path.lexists(parked):
            self._drop_parking_files(ident)
            if self.root_reachable():
                remove(self.trees_path(ident + ".partial"))
        return result

    def recover(self, stop: Optional[Callable[[], bool]] = None) -> List[Tuple[str, str]]:
        """Settle every dead owner's residue, adopt cold records the SSD index lost, drop stale unpark siblings and
        reap cold-side debris. Returns (what, detail) pairs. A live owner's parking copy is left alone. `stop()`
        is asked before each piece of deleting or moving and ends the sweep when it says True (the park pass:
        "a preflight is waiting for the lock"); what is left waits for the next pass."""
        out: List[Tuple[str, str]] = []
        if self.cfg is None or refuse_test_env(self.env):
            return out
        for pid, claim, index in self.claims():  # a dead restorer's claim: its record goes back, then the rest is judged
            if not pid_alive(pid) and self._restore_claim(claim, index):
                out.append(("restored-unpark-claim", (read_json(index) or {}).get("origin") or index))
        base = os.path.join(self.ssd, "parking")
        try:
            names = sorted(n for n in os.listdir(base) if not n.endswith((".json", ".pid")))
        except OSError:
            names = []
        for ident in names:
            if not self._owner_alive(ident):
                if stop is not None and stop():
                    return out
                out.append((self._settle_residue(ident), ident))
        for rec in self.records():               # an interrupted unpark's hidden sibling
            parent, name = os.path.split(rec["origin"])
            try:
                siblings = os.listdir(parent)
            except OSError:
                continue
            for entry in siblings:
                if entry.startswith(".%s.unpark." % name):
                    try:
                        alive = pid_alive(int(entry.rsplit(".", 1)[1]))
                    except ValueError:
                        alive = False
                    if not alive:
                        if stop is not None and stop():
                            return out
                        remove(os.path.join(parent, entry))
                        out.append(("removed-unpark-residue", os.path.join(parent, entry)))
        if self.root_reachable() and os.path.isdir(self.trees_path()):
            for name in sorted(os.listdir(self.trees_path())):
                if name.endswith(".json"):
                    rec = read_json(self.trees_path(name))
                    if rec and rec.get("origin") and os.path.isdir(self.trees_path(name[:-5])) \
                            and not os.path.exists(self.index_path(rec["origin"])) and not self.claims(rec["origin"]):
                        write_json(self.index_path(rec["origin"]), rec)
                        out.append(("adopted", rec["origin"]))
            out.extend(("reaped", p) for p in self._reap(stop))
        return out

    def _reap(self, stop: Optional[Callable[[], bool]] = None) -> List[str]:
        """Delete cold-side debris: a copy in progress with no live owner, rotated or consumed copies, a record
        whose copy is gone, a record temp file of a dead writer. An unrecorded directory is left alone. `stop()`
        is asked before each deletion (a whole tree on a slow disk) and ends the sweep when it says True."""
        gone = []
        base = self.trees_path()
        for name in sorted(os.listdir(base)):
            path = os.path.join(base, name)
            if name.endswith(".partial"):
                stem = name[:-len(".partial")]
                debris = not (os.path.lexists(self.parking_path(stem)) and self._owner_alive(stem))
            elif DEBRIS_SUFFIX.search(name):
                debris = True
            elif ".json.tmp." in name:
                try:
                    debris = not pid_alive(int(name.rsplit(".", 1)[1]))
                except ValueError:
                    debris = True
            elif name.endswith(".json"):
                debris = not os.path.isdir(path[:-5])
            else:
                debris = False
            if debris:
                if stop is not None and stop():
                    break
                remove(path)
                gone.append(path)
        return gone

    # --- unpark ---
    @staticmethod
    def rate_for(record: dict, floor_mibs: float) -> float:
        """The copy rate (bytes/s) the cost rule divides by: the SLOWER of the record's measured park rate and the
        configured floor (a park that happened to run on an idle disk must not promise an unpark more than the floor);
        a record with no measurement takes the floor."""
        floor = floor_mibs * MIB
        measured = float(record.get("park_rate") or 0.0)
        return min(floor, measured) if measured > 0 else floor

    def unpark(self, origin: str, rebuild_secs: Optional[float] = None,
               free_after_ok: Optional[Callable[[int], bool]] = None, *, dry_run: bool = False,
               claimed: Optional[dict] = None) -> Outcome:
        """Fill an ABSENT tree from its parked copy when that beats a rebuild: `claim` (the decision, see there)
        and then the copy. With `claimed` (the record this process's own `claim` returned) only the copy runs: the
        caller took the claim under whatever lock it serialises its decisions with and copies without it. A copy
        that fails, or finds the tree made meanwhile, gives the claim back; UNPARKED carries the copy time and
        consumes the record. Without `claimed` the outcomes are `claim`'s (SKIPPED/REFUSED/DECLINED touch nothing)."""
        if self.cfg is None:
            return Outcome(SKIPPED, self.off_reason)
        origin = norm(origin)
        if claimed is None:
            got = self.claim(origin, rebuild_secs, free_after_ok, dry_run=dry_run)
            if got.status != CLAIMED:
                return got
            claimed = got.record
        if not os.path.exists(self.claim_path(origin)):
            return Outcome(SKIPPED, "this process holds no claim on %s" % origin)
        try:
            out = self._copy_back(origin, claimed)
        except BaseException:
            self.release(origin)
            raise
        if out.status != UNPARKED:
            self.release(origin)
        return out

    def claim(self, origin: str, rebuild_secs: Optional[float] = None,
              free_after_ok: Optional[Callable[[int], bool]] = None, *, dry_run: bool = False) -> Outcome:
        """The deciding half of an unpark: is the tree absent, is the cold volume usable, is the copy cheaper than
        a rebuild (`rebuild_secs` defaults to the config's figure for the directory name) and does the caller's
        `free_after_ok(bytes)` say the free space after it stays over its line. `bytes` is this copy's apparent
        size plus what the other live claims are still to copy. Only then is the index record CLAIMED (renamed to
        `claim_path`, atomically) and CLAIMED returned with it; hand that record to `unpark(claimed=...)`.
        SKIPPED/REFUSED/DECLINED touch nothing, and an entry another live process has claimed is a SKIPPED that
        names it. A claim whose process is dead is put back as the entry first."""
        if self.cfg is None:
            return Outcome(SKIPPED, self.off_reason)
        refusal = refuse_test_env(self.env)
        if refusal:
            return Outcome(REFUSED, refusal)
        origin = norm(origin)
        if not dry_run:
            for pid, claim, index in self.claims(origin):
                if not pid_alive(pid):
                    self._restore_claim(claim, index)
        record = read_json(self.index_path(origin))
        if record is None:
            return self._nothing_to_claim(origin)
        if os.path.lexists(origin):
            return Outcome(REFUSED, "%s exists: unpark only fills an absent tree" % origin)
        parent = os.path.dirname(origin)
        if not os.path.isdir(parent):
            return Outcome(REFUSED, "%s does not exist" % parent)
        build_dev = device_of(parent)
        why = self.available(build_dev)
        if why:
            return Outcome(REFUSED, why)
        ident = record.get("id") or escape(origin)
        if not os.path.isdir(self.trees_path(ident)):
            unlink(self.index_path(origin))     # the cold copy was pruned: the record is a lie
            return Outcome(SKIPPED, "the cold copy is gone; dropped its record")
        apparent = int(record.get("apparent_bytes", 0))
        budget = rebuild_secs if rebuild_secs is not None else self.cfg.rebuild_secs(os.path.basename(origin))
        estimate = apparent / self.rate_for(record, self.cfg.rate_floor_mibs)
        if estimate >= budget:
            return Outcome(DECLINED, "copying %s would take ~%.0f s, a rebuild ~%.0f s" % (human(apparent), estimate, budget),
                           record)
        if free_after_ok is not None and not free_after_ok(apparent + self.inflight_bytes(build_dev)):
            return Outcome(DECLINED, "free space after copying %s would fall under the caller's line" % human(apparent),
                           record)
        if dry_run:
            return Outcome(DRY_RUN, "would unpark %s (~%.0f s)" % (human(apparent), estimate), record)
        try:
            os.rename(self.index_path(origin), self.claim_path(origin))
        except FileNotFoundError:
            return self._nothing_to_claim(origin)        # another process took the entry between the read and here
        except OSError as exc:
            return Outcome(REFUSED, "cannot claim the parked copy of %s: %s" % (origin, exc))
        return Outcome(CLAIMED, "", record)

    def _nothing_to_claim(self, origin: str) -> Outcome:
        holder = self.claimed_by(origin)
        if holder:
            return Outcome(SKIPPED, "being restored by pid %d" % holder)
        return Outcome(SKIPPED, "nothing parked for %s" % origin)

    def _copy_back(self, origin: str, record: dict) -> Outcome:
        """The copy half: this process holds the claim on `origin`. UNPARKED has the tree in place and the claim
        consumed; anything else leaves the claim for the caller to give back."""
        ident = record.get("id") or escape(origin)
        apparent = int(record.get("apparent_bytes", 0))
        rate = self.rate_for(record, self.cfg.rate_floor_mibs)
        sibling = os.path.join(os.path.dirname(origin), ".%s.unpark.%d" % (os.path.basename(origin), os.getpid()))
        self.log("cold-store: unpark %s from %s (est %.0f s at %.0f MiB/s): %s" % (
            human(apparent), self.cfg.root, apparent / rate, rate / MIB, origin))
        started = time.time()
        try:
            remove(sibling)
            copy_tree(self.trees_path(ident), sibling)
            got = tree_stats(sibling)
            if got != (int(record.get("files", -1)), apparent):
                raise OSError("copy verification failed: %d files / %d bytes, expected %s / %d"
                              % (got[0], got[1], record.get("files"), apparent))
            if os.path.lexists(origin):          # a build recreated it while we copied: it wins
                remove(sibling)
                return Outcome(REFUSED, "%s appeared while copying" % origin)
            os.rename(sibling, origin)
        except (OSError, subprocess.SubprocessError) as exc:
            remove(sibling)
            return Outcome(FAILED, "unpark failed: %s" % exc)
        took = time.time() - started
        self._consume(ident, origin)
        self.log("cold-store: unpark %s in %.1f s: %s" % (human(apparent), took, origin))
        return Outcome(UNPARKED, "", record, took)

    def _consume(self, ident: str, origin: str) -> None:
        """The origin has the tree again: end this process's claim (the record is forgotten) and set the cold copy
        aside for the next reap (a rename, so the build that paid for the unpark does not also wait for a delete
        on a slow disk)."""
        unlink(self.claim_path(origin))
        unlink(self.trees_path(ident + ".json"))
        try:
            os.rename(self.trees_path(ident), self._aside(ident, "consumed"))
        except OSError:
            pass                                 # the next park of this origin sets the stale copy aside itself

    # --- prune ---
    def prune(self, now: Optional[float] = None, facts: Optional[PruneFacts] = None, *,
              dry_run: bool = False, stop: Optional[Callable[[], bool]] = None) -> List[Tuple[str, str, int]]:
        """Drop parked copies that no longer earn their space; returns (origin, why, apparent bytes). Rules: the
        origin exists and was built after the park; its lane is gone or merged (per `facts`); older than max_days;
        then, while the cold volume is under min_free_gib, the oldest left. Reaps cold-side debris too. `stop()` is
        asked before each copy is deleted (and before each reap) and ends the sweep when it says True, returning
        what was dropped so far: the rest waits for the next pass."""
        if self.cfg is None or refuse_test_env(self.env) or not self.root_reachable():
            return []
        now = clock(self.env) if now is None else now
        facts = facts if facts is not None else PruneFacts()
        dropped: List[Tuple[str, str, int]] = []
        keep: List[dict] = []
        for rec in self.records():
            origin, why = rec["origin"], ""
            built = facts.built.get(origin)
            if built is None and os.path.lexists(origin):
                try:
                    built = os.lstat(origin).st_mtime
                except OSError:
                    built = None
            if built is not None and os.path.lexists(origin) and built > rec.get("parked_at", 0):
                why = "the tree was rebuilt since it was parked"
            elif rec.get("checkout") in facts.gone:
                why = "its lane is gone"
            elif rec.get("checkout") in facts.merged:
                why = "its lane is merged"
            elif now - rec.get("parked_at", 0) > self.cfg.max_days * 86400:
                why = "older than max_days=%g" % self.cfg.max_days
            if why:
                if not dry_run and stop is not None and stop():
                    return dropped
                dropped.append((origin, why, int(rec.get("apparent_bytes", 0))))
                if not dry_run:
                    self._forget(rec)
            else:
                keep.append(rec)
        floor = self.cfg.min_free_gib * GIB
        anchor = nearest_existing(self.cfg.root)
        free = free_bytes_of(anchor, self.env) if os.path.isdir(anchor) else floor
        for rec in keep:                         # oldest first
            if free >= floor:
                break
            if not dry_run and stop is not None and stop():
                return dropped
            size = int(rec.get("apparent_bytes", 0))
            dropped.append((rec["origin"], "the cold volume is under min_free_gib=%g" % self.cfg.min_free_gib, size))
            if dry_run:
                free += size
            else:
                self._forget(rec)
                free = free_bytes_of(anchor, self.env)
        if not dry_run and os.path.isdir(self.trees_path()):
            self._reap(stop)
        return dropped

    def _forget(self, rec: dict) -> None:
        ident = rec.get("id") or escape(rec["origin"])
        unlink(self.index_path(rec["origin"]))
        unlink(self.trees_path(ident + ".json"))
        remove(self.trees_path(ident))


# --- the CLI -----------------------------------------------------------------------------------------------
def tree_kind(path: str, env) -> str:
    fleet = env.get("PLX_FLEET_DIR")
    fleet = os.path.expanduser("~/plx-fleet") if fleet is None else fleet
    real = os.path.realpath(path)
    if fleet and real.startswith(os.path.realpath(fleet) + os.sep):
        return "fleet"
    if real.startswith(("/private/tmp/", "/private/var/")):
        return "tmp"
    return "lane"


def checkout_of(path: str) -> str:
    cur = os.path.dirname(os.path.realpath(path))
    while cur != os.sep:
        if os.path.exists(os.path.join(cur, ".git")):
            return cur
        cur = os.path.dirname(cur)
    return ""


def head_of(checkout: str) -> str:
    if not checkout:
        return ""
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    try:
        out = subprocess.run(["git", "-C", checkout, "rev-parse", "HEAD"], capture_output=True, text=True, env=env,
                             timeout=30)
    except (OSError, subprocess.SubprocessError):
        return ""
    return out.stdout.strip() if out.returncode == 0 else ""


def disk_budget_probe(store: ColdStore, kind: str, checkout: str) -> Callable[[str], Optional[str]]:
    """The liveness callback the CLI hands park(): disk-budget.py's own `probe` (the executor's checks), with the
    configured idle time for a /tmp tree and for the lane that owns a lane or fleet tree. `disk-budget.py park`
    (build-gc's `--auto` and `--park`) calls `probe_tree` in process instead."""
    script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "disk-budget.py")

    def live(path: str) -> Optional[str]:
        idle = str(int(store.cfg.park_idle_min * 60))
        cmd = [sys.executable, script, "probe", "--tree", path, "--class", kind,
               "--min-age-secs", idle, "--lane-idle-secs", idle]
        if checkout:
            cmd += ["--checkout", checkout]
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
        return None if out.returncode == 0 else (out.stdout.strip() or out.stderr.strip() or "probe failed")
    return live


def main(argv: Optional[List[str]] = None) -> int:
    ap = argparse.ArgumentParser(prog="cold-store.py", description=__doc__.split("\n")[1])
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("status")
    sub.add_parser("list")
    sub.add_parser("recover")
    for name in ("park", "unpark"):
        p = sub.add_parser(name)
        p.add_argument("path")
        p.add_argument("-n", "--dry-run", action="store_true")
        if name == "park":
            p.add_argument("--kind", choices=("lane", "fleet", "tmp"))
    p = sub.add_parser("prune")
    p.add_argument("-n", "--dry-run", action="store_true")
    args = ap.parse_args(argv)
    store = ColdStore(os.environ, log=print)
    if not store.configured():
        print("cold store: %s" % store.off_reason)
        return 0
    if args.cmd == "status":
        why = store.available(device_of(nearest_existing(build_cache(store.env))))
        recs = store.records()
        print("cold store: root %s" % store.cfg.root)
        print("available:  %s" % ("yes" if why is None else "no, " + why))
        print("parked:     %d trees, %s" % (len(recs), human(sum(r.get("apparent_bytes", 0) for r in recs))))
        for ident, size, alive in store.parking_residue():
            print("parking:    %s %s (owner %s)" % (human(size), ident, "alive" if alive else "dead: run `recover`"))
        for pid, claim, index in store.claims():
            print("restoring:  %s (pid %d %s)" % ((read_json(claim) or {}).get("origin") or os.path.basename(index), pid,
                                                  "alive" if pid_alive(pid) else "dead: run `recover`"))
        return 0
    if args.cmd == "list":
        now = time.time()
        for rec in store.records():
            print("%9s  %5.1f d  %-5s %s" % (human(rec.get("apparent_bytes", 0)),
                                              (now - rec.get("parked_at", now)) / 86400, rec.get("kind", "?"), rec["origin"]))
        return 0
    if args.cmd == "recover":
        for what, detail in store.recover():
            print("%s: %s" % (what, detail))
        return 0
    if args.cmd == "prune":
        for origin, why, size in store.prune(facts=PruneFacts.from_disk(store.records()), dry_run=args.dry_run):
            print("%s %s: %s (%s)" % ("would drop" if args.dry_run else "dropped", human(size), origin, why))
        return 0
    if args.cmd == "unpark":
        out = store.unpark(args.path, dry_run=args.dry_run)
        print("%s%s" % (out.status, ": " + out.reason if out.reason else ""))
        return 0 if out.status in (UNPARKED, DRY_RUN, SKIPPED, DECLINED) else 1
    tree = norm(args.path)
    kind = args.kind or tree_kind(tree, store.env)
    checkout = checkout_of(tree)
    live = None if args.dry_run else disk_budget_probe(store, kind, checkout)
    out = store.park(tree, kind=kind, checkout=checkout, head=head_of(checkout), live=live, dry_run=args.dry_run)
    print("%s%s" % (out.status, ": " + out.reason if out.reason else ""))
    return 0 if out.ok else 1


if __name__ == "__main__":
    sys.exit(main())
