#!/usr/bin/env python3
"""Build and run the `cargo fuzz` package over the HUD package and registry parsers and the WebSocket codec (`fuzz/`).

    python3 tools/fuzz-hud.py list
    python3 tools/fuzz-hud.py seed                    # fuzz/seeds/: seed corpora and dictionaries, from files in the tree
    python3 tools/fuzz-hud.py lock                    # fuzz/Cargo.lock: a copy of the shipped lock (build does this itself)
    python3 tools/fuzz-hud.py build [TARGET ...]      # the instrumented binaries; no target = all
    python3 tools/fuzz-hud.py run [--secs N] [TARGET ...]    # seed + build + each target for N seconds, one after another

`make fuzz-hud [T=<target>] [SECS=60]` is `run`. The package is outside the shipped workspace (`fuzz/Cargo.toml`
says where and why), so this is the only door to it; docs/agent-reference.md (the `make fuzz-hud` entry) says
when to run it.

Nothing here is committed that the tree can supply: the seeds are copies of the fixtures, the examples and the
icons, the registry-index seeds are built from the shape `catalog::parse` accepts, and the JSON dictionary is read
from the published schemas, so none of them can drift from the format. They land in `fuzz/seeds/` (gitignored);
what the fuzzer finds lands in `fuzz/corpus/<target>/` and a crash in `fuzz/artifacts/<target>/`, both ignored too
(FUZZ_WORK=<dir> moves all three; see WORK below).

The cargo build tree is `rust-modules/target-fuzz`: inside the checkout and named `target-*`, so `make disk` and
`tools/build-gc.sh` see it and it goes with the worktree. It is separate from every other tree because the
sanitizer and coverage flags change every crate's fingerprint.

Environment: FUZZ_TOOLCHAIN names the nightly (default `nightly`; CI names a dated one), FUZZ_RSS_MB the per-process
memory limit libFuzzer enforces (default 2048). Exit status: 0, or 1 when any target crashed, timed out or ran out
of memory, or a build failed.
"""
import argparse
import base64
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FUZZ = ROOT / "fuzz"
# Seeds, the corpora the fuzzer grows and the crashing inputs live under WORK (default fuzz/, gitignored). A corpus
# is thousands of tiny files read and written all through a run, so a checkout on a slow or shared disk (the
# external one a lane may sit on: ~40 ms a file, a 5 minute run spending two of them loading its corpus) sets
# FUZZ_WORK to a directory on a fast one.
WORK = Path(os.environ["FUZZ_WORK"]).resolve() if os.environ.get("FUZZ_WORK") else FUZZ
LOCK_SRC = ROOT / "rust-modules" / "Cargo.lock"
TARGET_DIR = "rust-modules/target-fuzz"          # relative to ROOT, like every path handed to cargo below

FIXTURES = ROOT / "rust-modules/appkit/src/hud_format/fixtures"
EXAMPLES = ROOT / "site/hud/examples"
SCHEMAS = ROOT / "site/hud/schema"
ICONS = ROOT / "assets/icons"
ORIGIN = "https://huds.plxnative.com"       # data/src/hudpacks/catalog.rs, ORIGIN
CATALOG = ROOT / "rust-modules/data/src/hudpacks/catalog.rs"
CATALOG_TESTING = ROOT / "rust-modules/data/src/hudpacks/catalog/testing.rs"

# target -> (dictionary, -max_len). max_len is above the bound the parser checks first (a bundle's 2 MiB is a
# plain length compare, covered in-crate; an SVG's 16 KiB and an address's 2048 bytes are reached), because
# libFuzzer otherwise stops at the longest seed. The seeds of a target are WORK/seeds/<target>/.
TARGETS = {
    "admit": ("hud", 65536),
    "bundle_read": ("hud", 65536),
    "archive_read": ("zip", 65536),
    "svg_check": ("svg", 20000),
    "address_parse": ("address", 2200),
    "catalog_parse": ("catalog", 65536),
    "catalog_verify": ("catalog", 65536),
    "catalog_cache": ("catalog", 65536),
    "ws_frame": ("ws", 20000),
}

WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
WS_NONCE = bytes([7] * 16)

ADDRESSES = [
    "gh:GLinnik21/plxnative-huds", "gh:owner/repo@v1.2.3", "GH:Owner-1/Repo_name.x@1.0.0+build",
    "https://example.com/hud.plxhud", "https://example.com:8443/a/b.plxhud?x=1#frag",
    "https://[2001:db8::1]:443/hud", "https://user:pass@example.com/hud", "http://192.168.1.20:8000/hud.plxhud",
    "http://10.0.0.5/hud", "http://172.16.0.1/", "http://8.8.8.8/hud", "http://localhost/hud", "  gh:a/b  ",
    "ftp://example.com/x", "https://exa mple.com", "https://example.com/%zz", "https://example.com:0/", "",
]


def say(message):
    print(f"fuzz-hud: {message}", flush=True)


def targets_named(names):
    unknown = [n for n in names if n not in TARGETS]
    if unknown:
        sys.exit(f"fuzz-hud: unknown target {unknown[0]!r}; the targets are: {', '.join(TARGETS)}")
    return names or list(TARGETS)


# --- seeds -----------------------------------------------------------------------------------------------

def sha256_hex(data):
    return hashlib.sha256(data).hexdigest()


def put(directory, name, data):
    directory.mkdir(parents=True, exist_ok=True)
    (directory / name).write_bytes(data if isinstance(data, bytes) else data.encode())


def ws_frame(opcode, payload, fin=True):
    n = len(payload)
    length = bytes([n]) if n < 126 else bytes([126]) + n.to_bytes(2, "big") if n <= 0xFFFF else bytes([127]) + n.to_bytes(8, "big")
    return bytes([(0x80 if fin else 0) | opcode]) + length + payload


def ws_upgrade():
    key = base64.b64encode(WS_NONCE).decode()
    accept = base64.b64encode(hashlib.sha1((key + WS_GUID).encode()).digest()).decode()
    return (f"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Accept: {accept}\r\n\r\n").encode()


def ws_seeds():
    euro = "€".encode()
    return {
        "rfc6455-5-7": b"\x00" + ws_frame(0x1, b"Hello") + ws_frame(0x1, b"Hel", fin=False) + ws_frame(0x0, b"lo")
                       + ws_frame(0x9, b"Hello") + ws_frame(0x2, bytes(range(256))),
        "fragments-around-a-ping": b"\x02" + ws_frame(0x1, b"price " + euro[:1], fin=False) + ws_frame(0x9, b"p")
                                   + ws_frame(0x0, euro[1:], fin=False) + ws_frame(0x0, b"5"),
        "close-with-reason": b"\x04" + ws_frame(0x8, (1000).to_bytes(2, "big") + b"bye"),
        "close-empty": b"\x00" + ws_frame(0x8, b""),
        "invalid-text": b"\x00" + ws_frame(0x1, b"\xce\xba\xff"),
        "length-past-the-limit": b"\x00" + bytes([0x82, 0x7E, 0x04, 0x01]),
        "unknown-opcode": b"\x00" + bytes([0x83, 0x00]),
        "upgrade-then-text": b"\x01" + ws_upgrade() + ws_frame(0x1, b'{"type":"joined"}'),
        "upgrade-refused": b"\x01" + b"HTTP/1.1 403 Forbidden\r\n\r\n",
        "empty": b"\x00",
    }


def byte_entries(entries):
    return "".join('"%s"\n' % "".join("\\x%02x" % b for b in entry) for entry in entries)


def index_seed(entries=2, revoked=1, serial=1000):
    """A registry index `catalog::parse` accepts: the shape `catalog/tests.rs` builds, with several entries. A
    `serial` of None leaves it out, as a bare `index.json` has none (`verify` refuses such a signed index)."""
    packs = []
    for n in range(entries):
        sha = sha256_hex(f"package {n}".encode())
        art = sha256_hex(f"preview {n}".encode())
        packs.append({
            "id": f"com.example.hud{n}", "name": f"Example {n}", "version": f"1.{n}.0", "author": "Author",
            "license": "CC0-1.0", "package_format": "1.0", "hud_format": "1.0",
            "app": {"min": "0.9.0"} if n else {}, "bytes": 2000 + n, "sha256": sha, "url": f"{ORIGIN}/p/{sha}.plxhud",
            "previews": [{"state": "compact", "lang": "en", "file": f"{art}.png", "sha256": art,
                          "url": f"{ORIGIN}/previews/{sha}/{art}.png"}],
            "unfit": [{"from": "0.10.1", "langs": ["fr"]}, {"from": "0.10.2", "langs": []}] if n else [],
            "sdk": {"source_sha": "ignored provenance"},
        })
    index = {"format": 1, "entries": packs,
             "revoked": [{"sha256": sha256_hex(f"revoked {n}".encode()), "reason": "withdrawn"} for n in range(revoked)]}
    if serial is not None:
        index = {"format": 1, "serial": serial, **{k: v for k, v in index.items() if k != "format"}}
    return index


def cache_seed(body, **fields):
    record = {"protocol": 1, "checked_at": 1_700_000_000, "attempted_at": 1_700_000_100, "etag": '"abc123"',
              "body": body}
    record.update(fields)
    return json.dumps(record, separators=(",", ":"))


# --- the signed registry index (PR14) ------------------------------------------------------------------------
# `index.signed` is one header line, a newline, and the index bytes; the header carries an Ed25519 signature over
# those bytes by a key whose first 8 bytes of SHA-256 name it (`catalog.rs`, `verify`). The seeds that reach what
# FOLLOWS the signature check are signed with the TEST ONLY keys the data layer's own tests use
# (`catalog/testing.rs`: seeds 7, 8 and 9, public knowledge, none of them the registry's). The standard library has
# no Ed25519, so this is the RFC 8032 reference signer (a dozen signatures a run; ci/test_ci_workflows.py pins it to
# the RFC's test vectors). No private key of the registry exists in this repository, and none is read here.
_P = 2**255 - 19
_Q = 2**252 + 27742317777372353535851937790883648493
_D = -121665 * pow(121666, _P - 2, _P) % _P
_SQRT_M1 = pow(2, (_P - 1) // 4, _P)


def _inv(x):
    return pow(x, _P - 2, _P)


def _recover_x(y, sign):
    x2 = (y * y - 1) * _inv(_D * y * y + 1) % _P
    x = pow(x2, (_P + 3) // 8, _P)
    if (x * x - x2) % _P:
        x = x * _SQRT_M1 % _P
    return _P - x if x & 1 != sign else x


_GY = 4 * _inv(5) % _P
_G = (_recover_x(_GY, 0), _GY, 1, _recover_x(_GY, 0) * _GY % _P)


def _add(a, b):
    p, q = (a[1] - a[0]) * (b[1] - b[0]) % _P, (a[1] + a[0]) * (b[1] + b[0]) % _P
    c, d = 2 * a[3] * b[3] * _D % _P, 2 * a[2] * b[2] % _P
    e, f, g, h = q - p, d - c, d + c, q + p
    return (e * f % _P, g * h % _P, f * g % _P, e * h % _P)


def _mul(scalar, point):
    out = (0, 1, 1, 0)
    while scalar:
        if scalar & 1:
            out = _add(out, point)
        point = _add(point, point)
        scalar >>= 1
    return out


def _compress(point):
    zi = _inv(point[2])
    x, y = point[0] * zi % _P, point[1] * zi % _P
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


def ed25519_sign(seed, message):
    """(public key, signature) of `message` for a 32-byte `seed`: RFC 8032, section 5.1.6."""
    h = hashlib.sha512(seed).digest()
    a = int.from_bytes(h[:32], "little") & ((1 << 254) - 8) | (1 << 254)
    public = _compress(_mul(a, _G))
    r = int.from_bytes(hashlib.sha512(h[32:] + message).digest(), "little") % _Q
    r_point = _compress(_mul(r, _G))
    k = int.from_bytes(hashlib.sha512(r_point + public + message).digest(), "little") % _Q
    return public, r_point + ((r + k * a) % _Q).to_bytes(32, "little")


def test_only_seeds():
    """{'PRIMARY': 7, 'STANDBY': 8, 'STRANGER': 9}: read from the data layer's test module, so they cannot drift."""
    found = re.findall(r"const TEST_ONLY_([A-Z]+): u8 = (\d+);", CATALOG_TESTING.read_text())
    seeds = {name: int(value) for name, value in found}
    missing = {"PRIMARY", "STANDBY", "STRANGER"} - set(seeds)
    if missing:
        sys.exit(f"fuzz-hud: {CATALOG_TESTING.relative_to(ROOT)} no longer names TEST_ONLY_{sorted(missing)[0]}")
    return seeds


def real_key_ids():
    """The 16-hex-digit ids of `ACCEPTED_KEYS`, the way a signed header names them. Public values."""
    ids = re.findall(r"id: \[((?:0x[0-9a-f]{2},? ?){8})\]", CATALOG.read_text())
    return [bytes(int(b, 16) for b in re.findall(r"0x([0-9a-f]{2})", ident)).hex() for ident in ids]


def signed_index(seed, index, key_id=None):
    """`index.signed`: the header line, a newline, `index`, signed by the key made from `seed`. `key_id` names a
    different key in the header (a real one, whose signature then cannot verify)."""
    public, signature = ed25519_sign(bytes([seed]) * 32, index)
    key = key_id or hashlib.sha256(public).digest()[:8].hex()
    return f'{{"format":1,"key":"{key}","sig":"{signature.hex()}"}}\n'.encode() + index


def schema_words():
    """Every property name and enum/const string of the published schemas: the dictionary of the JSON targets."""
    words = set()

    def walk(node, in_properties=False):
        if isinstance(node, dict):
            for key, value in node.items():
                if in_properties:
                    words.add(key)
                if key in ("enum", "const"):
                    for item in value if isinstance(value, list) else [value]:
                        if isinstance(item, str):
                            words.add(item)
                walk(value, key == "properties")
        elif isinstance(node, list):
            for item in node:
                walk(item)

    for path in sorted(SCHEMAS.glob("*/*.json")):
        walk(json.loads(path.read_text()))
    return sorted(w for w in words if 0 < len(w) <= 64 and w.isprintable())


def dict_entries(words):
    return "".join('"%s"\n' % w.replace("\\", "\\\\").replace('"', '\\"') for w in words)


def write_seeds():
    seeds = WORK / "seeds"
    shutil.rmtree(seeds, ignore_errors=True)

    bundles = sorted(FIXTURES.glob("*.plxhud"))
    for set_name in ("admit", "bundle_read", "archive_read"):
        for path in bundles:
            put(seeds / set_name, path.name, path.read_bytes())
    # Wrong-length and wrong-shape starts for the container's own refusals: an empty file, the end record alone.
    for set_name in ("admit", "bundle_read", "archive_read"):
        put(seeds / set_name, "empty", b"")
        put(seeds / set_name, "end-record-only", b"PK\x05\x06" + b"\0" * 18)

    svgs = sorted(ICONS.glob("*.svg")) + sorted(FIXTURES.glob("*/assets/*.svg")) \
        + sorted(EXAMPLES.glob("invalid/*/assets/*.svg"))
    for path in svgs:
        put(seeds / "svg_check", f"{path.parent.parent.name if path.parent.name == 'assets' else 'icons'}-{path.name}", path.read_bytes())
    # The leading `<?xml ... ?>` is skipped by policy, so words the subset forbids elsewhere are admitted inside it:
    # the regression input for the target's own scan, which once flagged them (a false crash, not a parser bug).
    put(seeds / "svg_check", "prolog-holds-forbidden-words",
        '<?xml version="1.0" stroke href url( data: opacity http://example.test/ ?>\n'
        '<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path d="M0 0L1 1z"/></svg>\n')

    for n, text in enumerate(ADDRESSES):
        put(seeds / "address_parse", f"address-{n:02d}", text)

    good = json.dumps(index_seed(), separators=(",", ":"))
    one = json.dumps(index_seed(1, 0), separators=(",", ":"))
    put(seeds / "catalog_parse", "index-two-entries", good)
    put(seeds / "catalog_parse", "index-one-entry", one)
    put(seeds / "catalog_parse", "index-empty", '{"format":1,"entries":[],"revoked":[]}')

    # The signed index (PR14). The TEST ONLY keys are not the app's, so under the shipped keys these are refused
    # as an unknown key: what they give the fuzzer is a header and a framing that are well formed to mutate. A real
    # key's id on a test signature reaches the shipped key's verification, which refuses it as a bad signature.
    seeds_of = test_only_seeds()
    index = good.encode()
    real_ids = real_key_ids()
    signed = {
        "signed-test-primary": signed_index(seeds_of["PRIMARY"], index),
        "signed-test-standby": signed_index(seeds_of["STANDBY"], index),
        "signed-stranger": signed_index(seeds_of["STRANGER"], index),
        "signed-one-entry": signed_index(seeds_of["PRIMARY"], one.encode()),
        "signed-no-serial": signed_index(seeds_of["PRIMARY"], json.dumps(index_seed(serial=None), separators=(",", ":")).encode()),
        "signed-empty-index": signed_index(seeds_of["PRIMARY"], b""),
        "bare-index": index,
        "empty": b"",
    }
    for n, ident in enumerate(real_ids):
        signed[f"real-key-{n}-test-signature"] = signed_index(seeds_of["PRIMARY"], index, key_id=ident)
    header = signed["signed-test-primary"].split(b"\n", 1)[0]
    signed["header-no-newline"] = header
    signed["header-only"] = header + b"\n"
    for name, data in signed.items():
        put(seeds / "catalog_verify", name, data)

    # The cache file (`CacheRecord::decode`): the same bodies wrapped, a bare index as a PR13-era build left it, and
    # the record that remembers only an attempt (the one that needs no signature).
    for name in ("signed-test-primary", "signed-test-standby", "signed-no-serial", "real-key-0-test-signature"):
        if name in signed:
            put(seeds / "catalog_cache", f"cache-{name}", cache_seed(signed[name].decode()))
    put(seeds / "catalog_cache", "cache-signed-no-validators",
        cache_seed(signed["signed-one-entry"].decode(), checked_at=None, attempted_at=None, etag=None))
    put(seeds / "catalog_cache", "cache-bare-index", cache_seed(good))
    put(seeds / "catalog_cache", "cache-empty", cache_seed("", checked_at=None))
    put(seeds / "catalog_cache", "cache-attempt-only", cache_seed("", checked_at=None, attempted_at=1_700_000_100, etag=None))

    for name, data in ws_seeds().items():
        put(seeds / "ws_frame", name, data)

    words = schema_words()
    dicts = seeds / "dict"
    put(dicts, "hud.dict", dict_entries(words) + '"PK\\x03\\x04"\n"PK\\x01\\x02"\n"PK\\x05\\x06"\n"manifest.json"\n"hud.json"\n"assets/"\n')
    put(dicts, "zip.dict", '"PK\\x03\\x04"\n"PK\\x01\\x02"\n"PK\\x05\\x06"\n"manifest.json"\n"hud.json"\n"assets/"\n"\\x00\\x00"\n"\\xff\\xff\\xff\\xff"\n')
    put(dicts, "svg.dict", dict_entries([
        "<svg", "</svg>", "<path", "/>", "viewBox", "d", "xmlns", "http://www.w3.org/2000/svg", "fill", "none", "#fff",
        "stroke", "opacity", "style", "href", "<g", "<!--", "-->", "<![CDATA[", "<!DOCTYPE", "<?xml", "?>", "&amp;", "&#",
        "url(", "data:", "<script", "<text", "<image", "transform", "M0 0", "z", "'", '"', "0 0 24 24", "1e999", "-0",
    ]))
    put(dicts, "address.dict", dict_entries([
        "gh:", "GH:", "https://", "HTTP://", "http://", "@", "/", ":", "?", "#", "%", "%2f", "%zz", "..", "[::1]",
        "10.0.0.1", "192.168.0.1", "172.16.0.1", "169.254.0.1", "127.0.0.1", "user:pass@", "github.com",
    ]))
    put(dicts, "ws.dict", byte_entries([
        b"\x81", b"\x01", b"\x80", b"\x82", b"\x88", b"\x89", b"\x8a", b"\x7e", b"\x7f", b"\x03\xe8", b"\x03\xed",
        b"\xce\xba", b"\xed\xa0\x80", b"\xf4\x90\x80\x80", b"\xe2\x82\xac", b"\r\n\r\n", b"HTTP/1.1 101 ",
        b"Upgrade: websocket\r\n", b"Connection: Upgrade\r\n", b"Sec-WebSocket-Accept: ",
        b"Sec-WebSocket-Extensions: ", b"Sec-WebSocket-Protocol: ",
    ]))
    put(dicts, "catalog.dict", dict_entries([
        "format", "entries", "revoked", "id", "name", "version", "author", "license", "package_format", "hud_format",
        "app", "min", "max", "sha256", "bytes", "url", "previews", "unfit", "state", "lang", "file", "from", "langs",
        "reason", "protocol", "checked_at", "attempted_at", "etag", "body", "sdk", "compact", "serial", "key", "sig",
        '{"format":1,"key":"', '","sig":"', *real_key_ids(),
        "sections-0-movie-paused-no-chapters", ORIGIN, "builtin:classic", "com.example.hud", "1.0", "0.0.0", "null",
    ]) + '"\\x0a"\n')
    say(f"seeds: {sum(1 for p in seeds.rglob('*') if p.is_file())} files in {seeds}")


# --- cargo ----------------------------------------------------------------------------------------------

def environment():
    env = dict(os.environ)
    env["PATH"] = f"{Path.home() / '.cargo/bin'}{os.pathsep}{env.get('PATH', '')}"
    env["CARGO_INCREMENTAL"] = "0"
    # The shipped crates are held to `warnings = "deny"` by their workspace (`make lint`, on the pinned stable). A
    # nightly can add a lint, and a fuzz build must not go red for a warning that gate has not seen: cap it.
    env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") + " --cap-lints=warn").strip()
    env.pop("CARGO_TARGET_DIR", None)
    if sys.platform == "darwin":
        # The instrumented binaries link SDL2 (host_link.rs). Homebrew's SDL2 is sdl2-compat, which dlopens SDL3 by
        # bare name when the image loads, and on failure raises a modal NSAlert nobody can dismiss: the process
        # then sits in dyld forever (seen: a "20 s" run still waiting after ten minutes). The host test runner
        # (tools/cargo-test-parallel.py) hands its binaries the link paths the same way.
        fallback = env.get("DYLD_FALLBACK_LIBRARY_PATH") or f"{Path.home() / 'lib'}:/usr/local/lib:/usr/lib"
        env["DYLD_FALLBACK_LIBRARY_PATH"] = f"/opt/homebrew/lib:{fallback}"
    return env


def cargo_fuzz(sub, extra, tail=()):
    toolchain = os.environ.get("FUZZ_TOOLCHAIN", "nightly")
    return ["cargo", f"+{toolchain}", "fuzz", sub, "--fuzz-dir", "fuzz", "--target-dir", TARGET_DIR, *extra,
            *(["--", *tail] if tail else [])]


def sync_lock():
    """`fuzz/Cargo.lock` starts as a copy of the shipped lock, so the shared crates are the versions that ship;
    cargo then adds only the fuzzer's own. Re-copied only when the shipped lock changed."""
    stamp = FUZZ / ".lock-base"
    base = sha256_hex(LOCK_SRC.read_bytes())
    if (FUZZ / "Cargo.lock").exists() and stamp.exists() and stamp.read_text() == base:
        return
    shutil.copyfile(LOCK_SRC, FUZZ / "Cargo.lock")
    stamp.write_text(base)
    say("fuzz/Cargo.lock: copied from rust-modules/Cargo.lock")


def build(names):
    sync_lock()
    say(f"build: {' '.join(names)} (toolchain {os.environ.get('FUZZ_TOOLCHAIN', 'nightly')}, tree {TARGET_DIR})")
    # `cargo fuzz build` takes one target, or none for all of them.
    for selection in ([[]] if set(names) == set(TARGETS) else [[name] for name in names]):
        code = subprocess.run(cargo_fuzz("build", selection), cwd=ROOT, env=environment()).returncode
        if code:
            sys.exit(f"fuzz-hud: build failed ({code})")


WATCHDOG_SLACK = 120      # seconds past -max_total_time before a run is killed from outside
DONE = re.compile(r"#(\d+)\s+DONE\s+cov:\s*(\d+)\s+ft:\s*(\d+)\s+corp:\s*(\d+)/(\S+)\s+lim:\s*\d+\s+exec/s:\s*(\d+)")


def host_triple():
    toolchain = os.environ.get("FUZZ_TOOLCHAIN", "nightly")
    text = subprocess.run(["rustc", f"+{toolchain}", "-vV"], cwd=ROOT, env=environment(), capture_output=True,
                          text=True, check=True).stdout
    return re.search(r"^host: (\S+)$", text, re.M).group(1)


def run_one(name, secs):
    """One target for `secs` seconds. The binary `cargo fuzz build` left is run directly rather than through
    `cargo fuzz run`, which re-checks the whole build tree first (tens of seconds on a loaded disk) and adds
    nothing: the flags are libFuzzer's own."""
    dictionary, max_len = TARGETS[name]
    corpus, artifacts, seeds = WORK / "corpus" / name, WORK / "artifacts" / name, WORK / "seeds" / name
    dict_file = WORK / "seeds/dict" / (dictionary + ".dict")
    for directory in (corpus, artifacts):
        directory.mkdir(parents=True, exist_ok=True)
    binary = ROOT / TARGET_DIR / host_triple() / "release" / name
    command = [str(binary), f"-artifact_prefix={artifacts}/", f"-max_total_time={secs}", f"-max_len={max_len}",
               "-timeout=10", f"-rss_limit_mb={os.environ.get('FUZZ_RSS_MB', '2048')}", "-use_value_profile=1",
               "-print_final_stats=1", f"-dict={dict_file}", str(corpus), str(seeds)]
    say(f"run {name}: {secs} s")
    done = None
    started = time.monotonic()
    process = subprocess.Popen(command, cwd=ROOT, env=environment(), stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                               text=True, errors="replace", start_new_session=True)
    # libFuzzer's own limits (-max_total_time, -timeout) only count once main() runs. A process stuck before it
    # (the dyld case above) is killed here instead of holding the lane's `make` for ever.
    watchdog = threading.Timer(secs + WATCHDOG_SLACK, lambda: os.killpg(process.pid, signal.SIGKILL))
    watchdog.start()
    try:
        for line in process.stdout:
            sys.stdout.write(line)
            sys.stdout.flush()
            found = DONE.search(line)
            if found:
                done = found
        code = process.wait()
    finally:
        watchdog.cancel()
    return {"target": name, "code": code, "secs": round(time.monotonic() - started),
            "runs": int(done[1]) if done else None, "cov": int(done[2]) if done else None,
            "corpus": f"{done[4]}/{done[5]}" if done else None, "execs": int(done[6]) if done else None}


def summary(results):
    say("summary")
    print(f"{'target':<15}{'exit':>5}{'secs':>6}{'runs':>12}{'exec/s':>9}{'cov':>7}  corpus")
    for r in results:
        print(f"{r['target']:<15}{r['code']:>5}{r['secs']:>6}{str(r['runs']):>12}{str(r['execs']):>9}{str(r['cov']):>7}  {r['corpus']}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["list", "seed", "lock", "build", "run"])
    parser.add_argument("targets", nargs="*")
    parser.add_argument("--secs", type=int, default=60, help="seconds per target for `run` (default 60)")
    args = parser.parse_intermixed_args()
    if args.command == "list":
        print("\n".join(TARGETS))
        return 0
    names = targets_named(args.targets)
    if args.command == "seed":
        write_seeds()
        return 0
    if args.command == "lock":
        sync_lock()
        return 0
    if shutil.which("cargo-fuzz") is None and not (Path.home() / ".cargo/bin/cargo-fuzz").exists():
        sys.exit("fuzz-hud: cargo-fuzz is not installed: cargo install cargo-fuzz --locked")
    if args.command == "build":
        build(names)
        return 0
    write_seeds()
    build(names)
    results = [run_one(name, args.secs) for name in names]
    summary(results)
    failed = [r["target"] for r in results if r["code"]]
    if failed:
        say(f"FAILED: {', '.join(failed)}; the inputs are in {WORK}/artifacts/<target>/ (minimise one with `cargo +nightly fuzz tmin --fuzz-dir fuzz --target-dir {TARGET_DIR} <target> <file>`)")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
