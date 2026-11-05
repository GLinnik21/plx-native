#!/usr/bin/env python3
"""tools/hud-dev: the address the TV may fetch from, the dev version, a build that lints before it packs, the last
clean bundle that stays up while the source has findings, and a real HTTP GET of it from a local client.

The simulator is a stub (a tiny script that answers the four `--hud-*` commands the tool uses), and every socket binds
loopback on port 0, so the suite needs no simulator build, no RFC 1918 interface and no fixed port. Runs on macOS's
/usr/bin/python3 (3.9) as well as the CI interpreter.
"""
import contextlib
import hashlib
import http.client
import importlib.machinery
import importlib.util
import io
import json
import os
import socket
import stat
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
import zipfile
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
TOOLS = ROOT / "tools"
sys.path.insert(0, str(TOOLS))


def load_tool():
    """`tools/hud-dev` has no .py suffix, so it is loaded by path."""
    kept, sys.dont_write_bytecode = sys.dont_write_bytecode, True
    try:
        loader = importlib.machinery.SourceFileLoader("hud_dev", str(TOOLS / "hud-dev"))
        spec = importlib.util.spec_from_loader("hud_dev", loader)
        module = importlib.util.module_from_spec(spec)
        sys.modules["hud_dev"] = module
        loader.exec_module(module)
        return module
    finally:
        sys.dont_write_bytecode = kept


hud_dev = load_tool()

#: A simulator that answers `--hud-info`, `--hud-pack`, `--hud-inspect` and `--hud-lint` the way the SDK does, as far
#: as `hud_package.snapshot` reads them. A hud.json containing BAD has a lint finding. Every call is logged.
STUB = r'''
import hashlib, json, sys
from pathlib import Path

args = sys.argv[1:]
Path(__file__).with_name("calls.log").open("a").write(args[0] + "\n")
PAIR = {"package": "1.0", "hud": "1.0"}


def say(report, code=0):
    print(json.dumps(report))
    sys.exit(code)


def manifest_of(text):
    m = json.loads(text)
    return {"package_format": "1.0", "hud": {"format": "1.0"}, "id": m["id"], "name": m["name"], "version": m["version"]}


if args[0] == "--hud-info":
    say({"protocol": 1, "supported_formats": [PAIR], "limits": {"package": {"1.0": {"artifact_bytes": 1000000}}}})
if args[0] == "--hud-lint":
    source = Path(args[1])
    if (source / "hud.json").read_text().find("BAD") >= 0:
        say({"protocol": 1, "findings": [{"code": "E0001", "severity": "error", "path": "/layout", "file": "hud.json",
             "rule": "stub", "message": "the stub does not like BAD", "hint": "remove BAD", "nearest": None}]}, 1)
    say({"protocol": 1, "findings": []})
if args[0] == "--hud-pack":
    source, out = Path(args[1]), Path(args[args.index("--out") + 1])
    manifest = (source / "manifest.json").read_text()
    content = b"STUB-PLXHUD\n" + manifest.encode() + b"\n---\n" + (source / "hud.json").read_bytes()
    out.write_bytes(content)
    say({"protocol": 1, "findings": [], "package_sha256": hashlib.sha256(content).hexdigest(), "bytes": len(content),
         "manifest": manifest_of(manifest)})
if args[0] == "--hud-inspect":
    content = Path(args[1]).read_bytes()
    manifest = content.split(b"\n", 1)[1].split(b"\n---\n", 1)[0].decode()
    say({"protocol": 1, "package_sha256": hashlib.sha256(content).hexdigest(), "manifest": manifest_of(manifest)})
sys.exit(2)
'''

MANIFEST = {"package_format": "1.0", "id": "org.example.hud", "name": "Test HUD", "version": "1.2.0",
            "author": "A", "license": "MIT", "hud": {"format": "1.0"}}
NOW = hud_dev.DEV_EPOCH + 1000


def write_json(path, document):
    path.write_text(json.dumps(document))


def served_version(content):
    """The version inside the stub archive the tool packed."""
    return json.loads(content.split(b"\n", 1)[1].split(b"\n---\n", 1)[0])["version"]


class Workdir(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="test-hud-dev-")
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        script = self.dir / "stub.py"
        script.write_text(STUB)
        self.sim = self.dir / "plxnative-sim"
        self.sim.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{script}" "$@"\n')
        self.sim.chmod(self.sim.stat().st_mode | stat.S_IXUSR)
        self.calls = self.dir / "calls.log"
        self.source = self.dir / "my-hud"
        self.source.mkdir()
        write_json(self.source / "manifest.json", MANIFEST)
        (self.source / "hud.json").write_text('{"layout": []}')
        (self.source / "README.md").write_text("not part of the package")
        self.now = [NOW]
        self.env = dict(os.environ)

    def builder(self, source=None):
        return hud_dev.Builder(self.sim, source or self.source, clock=lambda: self.now[0], env=self.env)

    def edit(self, text='{"layout": [1]}'):
        (self.source / "hud.json").write_text(text)

    def logged(self):
        return self.calls.read_text().split() if self.calls.exists() else []


class TheAddressTheTvMayFetchFrom(unittest.TestCase):
    def test_the_three_private_blocks_are_in_and_so_are_their_edges(self):
        for address in ("10.0.0.0", "10.1.2.3", "10.255.255.255", "172.16.0.0", "172.20.1.1", "172.31.255.255",
                        "192.168.0.0", "192.168.1.20", "192.168.255.255"):
            self.assertTrue(hud_dev.is_rfc1918(address), address)

    def test_everything_else_is_out_including_what_ipaddress_calls_private(self):
        for address in ("172.15.255.255", "172.32.0.0", "192.167.255.255", "192.169.0.0", "11.0.0.1", "9.255.255.255",
                        "127.0.0.1", "169.254.1.1", "100.64.0.1", "0.0.0.0", "255.255.255.255", "8.8.8.8",
                        "::1", "fe80::1", "fd00::1", "localhost", "printer.local", "", "192.168.1", "192.168.1.1.1",
                        "192.168.001.1", "192.168.1.256", " 192.168.1.1", "192.168.1.1 ", "192.168.1.1:80", None):
            self.assertFalse(hud_dev.is_rfc1918(address), repr(address))

    def test_an_address_is_named_or_found_and_a_machine_without_one_is_refused(self):
        self.assertEqual(hud_dev.addresses_for("10.0.0.5", ["192.168.1.2"]), ["10.0.0.5"])
        self.assertEqual(hud_dev.addresses_for(None, ["192.168.1.2", "10.0.0.9"]), ["192.168.1.2", "10.0.0.9"])
        with self.assertRaisesRegex(ValueError, "no RFC 1918 address"):
            hud_dev.addresses_for(None, [])
        with self.assertRaisesRegex(ValueError, "not an RFC 1918 address"):
            hud_dev.addresses_for("8.8.8.8", ["192.168.1.2"])

    def test_the_address_the_default_route_uses_is_listed_first_not_a_vpns(self):
        # A VPN that routes 10/8 answers the 10.x probe with the tunnel's address, which the TV cannot reach.
        routes = {"192.0.2.1": "192.168.1.20", "10.255.255.255": "10.8.0.2", "172.31.255.255": "127.0.0.1",
                  "192.168.255.255": "192.168.1.20"}
        with mock.patch.object(hud_dev, "route_address", side_effect=lambda destination: routes[destination]), \
                mock.patch.object(hud_dev.socket, "getaddrinfo", side_effect=OSError), \
                mock.patch.object(hud_dev.subprocess, "run", side_effect=OSError):
            self.assertEqual(hud_dev.private_addresses(), ["192.168.1.20", "10.8.0.2"])

    def test_the_banner_prints_the_exact_address_and_the_recipe_on_one_line(self):
        lines = hud_dev.banner(["192.168.1.20", "10.0.0.4"], 8731, "my-hud")
        self.assertEqual(lines[0], "TV address: http://192.168.1.20:8731/my-hud.plxhud")
        self.assertIn("http://10.0.0.4:8731/my-hud.plxhud", lines[1])
        recipe = [line for line in lines if "Install from address" in line]
        self.assertEqual(len(recipe), 1)
        self.assertIn("Play", recipe[0])
        self.assertNotIn("\n", recipe[0])

    def test_the_banner_says_to_remove_the_dev_install_before_installing_the_published_version(self):
        # A dev build's patch (seconds since 2026-01-01) is above a later published release, so the TV would call that
        # release "not newer" and never offer it; the one way out is the page's existing Remove row.
        lines = hud_dev.banner(["192.168.1.20"], 8731, "my-hud")
        removal = [line for line in lines if "Remove" in line]
        self.assertEqual(len(removal), 1, lines)
        self.assertIn("published", removal[0])
        self.assertIn("Player HUD", removal[0])
        self.assertNotIn("\n", removal[0])

    def test_the_file_name_in_the_address_is_the_sources_own_and_url_safe(self):
        self.assertEqual(hud_dev.package_name("/x/my-hud"), "my-hud")
        self.assertEqual(hud_dev.package_name("/x/My Hud v2.plxhud"), "My-Hud-v2")
        self.assertEqual(hud_dev.package_name("/x/..."), "hud")


class TheVersionIsAlwaysHigher(unittest.TestCase):
    def test_it_is_above_the_authors_and_above_the_last_one_even_in_the_same_second(self):
        first = hud_dev.dev_version("1.2.0", None, NOW)
        self.assertEqual(first, (1, 2, 1000))
        second = hud_dev.dev_version("1.2.0", first, NOW)
        third = hud_dev.dev_version("1.2.0", second, NOW)
        self.assertEqual((second, third), ((1, 2, 1001), (1, 2, 1002)))

    def test_a_restart_lands_above_what_the_set_already_holds(self):
        held = hud_dev.dev_version("1.2.0", None, NOW + 600)
        for _ in range(5):
            held = hud_dev.dev_version("1.2.0", held, NOW + 600)
        restarted = hud_dev.dev_version("1.2.0", None, NOW + 7200)
        self.assertGreater(restarted, held)

    def test_it_is_never_below_the_authors_own_version(self):
        for author in ("0.0.0", "1.2.0", "1.2.41", "9.0.0", "1.0.4294967295"):
            for now in (hud_dev.DEV_EPOCH - 99, hud_dev.DEV_EPOCH, NOW):
                self.assertGreater(hud_dev.dev_version(author, None, now), hud_dev.parse_version(author))
        # The author raising the minor wins over a counter that ran under the old one.
        self.assertEqual(hud_dev.dev_version("1.3.0", (1, 2, 5000), NOW), (1, 3, 1000))

    def test_the_text_is_the_three_plain_numbers_the_comparator_reads(self):
        self.assertEqual(hud_dev.format_version((1, 2, 1000)), "1.2.1000")
        for bad in ("1.0", "1.0.0-dev.1", "1.0.0+7", "01.0.0", "1.2.x", "", None):
            with self.assertRaises(ValueError, msg=repr(bad)):
                hud_dev.parse_version(bad)
        with self.assertRaises(ValueError):
            hud_dev.dev_version(f"1.0.{2**63}", None, NOW)


class OneBuild(Workdir):
    def test_a_clean_source_is_bundled_under_a_higher_version_and_the_authors_files_are_not_touched(self):
        before = (self.source / "manifest.json").read_bytes()
        bundle = self.builder().build()
        self.assertEqual(bundle.version, "1.2.1000")
        self.assertEqual(served_version(bundle.content), "1.2.1000")
        self.assertEqual((bundle.name, bundle.sha256), ("Test HUD", hashlib.sha256(bundle.content).hexdigest()))
        self.assertEqual((self.source / "manifest.json").read_bytes(), before)
        self.assertEqual(json.loads(before)["version"], "1.2.0")

    def test_it_lints_before_it_packs_and_the_simulator_is_the_only_packer(self):
        self.builder().build()
        calls = self.logged()
        self.assertEqual(calls[:2], ["--hud-lint", "--hud-info"])
        self.assertEqual(sorted(set(calls)), ["--hud-info", "--hud-inspect", "--hud-lint", "--hud-pack"])

    def test_every_change_gets_a_strictly_higher_version(self):
        builder = self.builder()
        seen = [builder.build().version]
        for step in range(4):
            self.edit('{"layout": [%d]}' % step)
            if step == 2:
                self.now[0] += 90
            seen.append(builder.build().version)
        parsed = [hud_dev.parse_version(version) for version in seen]
        self.assertEqual(parsed, sorted(set(parsed)))
        self.assertEqual(seen[:3], ["1.2.1000", "1.2.1001", "1.2.1002"])
        self.assertGreaterEqual(parsed[-1][2], 1090)

    def test_a_new_run_starts_above_the_last_run(self):
        first = self.builder()
        first.build()
        self.edit()
        last = first.build()
        self.now[0] += 30
        again = self.builder().build()
        self.assertGreater(hud_dev.parse_version(again.version), hud_dev.parse_version(last.version))

    def test_an_unchanged_source_is_not_rebuilt_and_does_not_burn_a_version(self):
        builder = self.builder()
        builder.build()
        count = len(self.logged())
        (self.source / "README.md").write_text("not part of the package")
        os.utime(self.source / "hud.json", (1, 1))
        self.assertIsNone(builder.build())
        self.assertEqual(len(self.logged()), count)

    def test_findings_stop_the_build_before_anything_is_packed(self):
        builder = self.builder()
        kept = builder.build()
        self.edit('{"layout": "BAD"}')
        before = self.logged().count("--hud-pack")
        with self.assertRaises(hud_dev.Findings) as raised:
            builder.build()
        self.assertEqual(raised.exception.findings[0]["code"], "E0001")
        self.assertEqual(self.logged().count("--hud-pack"), before, "a source with findings is never packed")
        self.edit('{"layout": [2]}')
        fixed = builder.build()
        self.assertGreater(hud_dev.parse_version(fixed.version), hud_dev.parse_version(kept.version))

    def test_the_findings_print_where_what_and_how_to_fix_it(self):
        lines = list(hud_dev.finding_lines([{"file": "hud.json", "path": "/layout", "code": "E0001", "severity": "error",
                                             "message": "the stub does not like BAD", "hint": "remove BAD"}]))
        self.assertEqual(lines, ["  hud.json/layout  E0001 error: the stub does not like BAD", "      hint: remove BAD"])

    def test_a_finding_without_a_file_or_a_pointer_prints_as_the_package(self):
        # The lint report writes null, not an absent key, for a finding that belongs to no file.
        lines = list(hud_dev.finding_lines([{"file": None, "path": None, "code": "E0002", "severity": "error",
                                             "message": "the archive is too large", "hint": None}]))
        self.assertEqual(lines, ["  (package)  E0002 error: the archive is too large"])

    def test_a_version_the_set_cannot_order_is_refused_before_packing(self):
        write_json(self.source / "manifest.json", dict(MANIFEST, version="1.2.0-dev.1"))
        with self.assertRaisesRegex(ValueError, "not MAJOR.MINOR.PATCH"):
            self.builder().build()
        self.assertNotIn("--hud-pack", self.logged())

    def test_a_symlink_in_the_source_is_refused(self):
        try:
            os.symlink("/etc/hosts", self.source / "link.svg")
        except (OSError, NotImplementedError):
            self.skipTest("no symlinks here")
        with self.assertRaisesRegex(ValueError, "symlink"):
            self.builder().build()


class AnArchiveAsTheSource(Workdir):
    def archive(self, *extra):
        path = self.dir / "given.plxhud"
        with zipfile.ZipFile(path, "w") as out:
            out.writestr("manifest.json", json.dumps(MANIFEST))
            out.writestr("hud.json", '{"layout": []}')
            for name, data in extra:
                out.writestr(name, data)
        return path

    def test_it_is_unpacked_to_a_source_and_bundled_by_the_simulator_under_a_higher_version(self):
        bundle = self.builder(self.archive()).build()
        self.assertEqual(served_version(bundle.content), "1.2.1000")
        self.assertIn("--hud-pack", self.logged())

    def test_a_member_that_leaves_the_folder_is_refused(self):
        for name in ("../evil.json", "/abs.json", "a\\b.json"):
            with self.assertRaisesRegex(ValueError, "not a plain relative file", msg=name):
                self.builder(self.archive((name, "x"))).build()
        self.assertEqual(self.logged(), [])

    def test_a_symlink_member_is_refused(self):
        path = self.dir / "link.plxhud"
        with zipfile.ZipFile(path, "w") as out:
            link = zipfile.ZipInfo("logo.svg")
            link.external_attr = (stat.S_IFLNK | 0o777) << 16
            out.writestr(link, "/etc/hosts")
        with self.assertRaisesRegex(ValueError, "not a plain relative file"):
            self.builder(path).build()

    def test_a_file_that_is_no_archive_is_a_failed_build_not_a_crash(self):
        path = self.dir / "broken.plxhud"
        path.write_bytes(b"not a zip")
        out = []
        dev = hud_dev.Dev(self.builder(path), hud_dev.Shelf(), out.append)
        self.assertFalse(dev.poll())
        self.assertTrue(out[0].startswith("not built:"), out)


def get(url, method="GET"):
    """A real HTTP request to the local server, with no proxy in between."""
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    return opener.open(urllib.request.Request(url, method=method), timeout=10)


class Served(Workdir):
    def start(self):
        self.shelf = hud_dev.Shelf()
        self.lines = []
        self.server = hud_dev.make_server(self.shelf, "my-hud", "127.0.0.1", 0, self.lines.append)
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()

        def stop():
            self.server.shutdown()
            self.server.server_close()
            thread.join(10)
        self.addCleanup(stop)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/my-hud.plxhud"


class TheServer(Served):
    def test_nothing_is_served_until_a_bundle_is_clean(self):
        self.start()
        with self.assertRaises(urllib.error.HTTPError) as raised:
            get(self.url)
        self.assertEqual(raised.exception.code, 503)
        raised.exception.close()

    def test_a_get_returns_the_bundle_bytes_and_a_put_replaces_them(self):
        self.start()
        self.shelf.put(hud_dev.Bundle("1.2.1000", b"first bundle", "a" * 64, "Test HUD"))
        with get(self.url) as response:
            self.assertEqual((response.status, response.read()), (200, b"first bundle"))
            self.assertEqual(response.headers["Content-Length"], "12")
            self.assertEqual(response.headers["Cache-Control"], "no-store")
        self.shelf.put(hud_dev.Bundle("1.2.1001", b"the second", "b" * 64, "Test HUD"))
        with get(self.url + "?again=1") as response:
            self.assertEqual(response.read(), b"the second")

    def test_starting_does_no_reverse_dns_lookup(self):
        # `HTTPServer.server_bind` calls `socket.getfqdn(host)`; on a resolver that answers slowly that was a 35 s start.
        with mock.patch.object(hud_dev.socket, "getfqdn", side_effect=AssertionError("reverse lookup")):
            server = hud_dev.make_server(hud_dev.Shelf(), "my-hud", "127.0.0.1", 0, lambda line: None)
        server.server_close()

    def test_a_head_has_the_length_and_no_body_and_anything_else_is_404(self):
        self.start()
        self.shelf.put(hud_dev.Bundle("1.2.1000", b"0123456789", "a" * 64, "Test HUD"))
        with get(self.url, "HEAD") as response:
            self.assertEqual((response.status, response.headers["Content-Length"], response.read()), (200, "10", b""))
        for path in ("/", "/other.plxhud", "/my-hud.plxhud/x", "/my-hud"):
            with self.assertRaises(urllib.error.HTTPError, msg=path) as raised:
                get(self.url.rsplit("/", 1)[0] + path)
            self.assertEqual(raised.exception.code, 404)
            raised.exception.close()

    def test_one_line_per_request_says_who_asked_for_what_and_which_version_it_got(self):
        self.start()
        self.shelf.put(hud_dev.Bundle("1.2.1000", b"x", "a" * 64, "Test HUD"))
        get(self.url).close()
        with self.assertRaises(urllib.error.HTTPError) as raised:
            get(self.url.replace("my-hud", "nope"))
        raised.exception.close()
        self.assertEqual(self.lines, ["127.0.0.1  GET /my-hud.plxhud  200  version 1.2.1000",
                                      "127.0.0.1  GET /nope.plxhud  404  version -"])


    def test_a_path_with_terminal_escapes_is_logged_written_out(self):
        # The server listens on every interface; what a stranger puts in the request line must not drive the terminal.
        self.start()
        with socket.create_connection(("127.0.0.1", self.server.server_address[1]), timeout=10) as raw:
            raw.sendall(b"GET /\x1b[31mred\x07 HTTP/1.0\r\n\r\n")
            raw.recv(4096)
        self.assertEqual(len(self.lines), 1, self.lines)
        self.assertNotIn("\x1b", self.lines[0])
        self.assertNotIn("\x07", self.lines[0])
        self.assertIn("\\x1b[31mred\\x07", self.lines[0])


class TheLoopKeepsTheLastCleanBundle(Served):
    def test_findings_are_printed_the_last_clean_bundle_stays_up_and_a_fix_replaces_it(self):
        self.start()
        said = []
        dev = hud_dev.Dev(self.builder(), self.shelf, said.append)
        self.assertTrue(dev.poll())
        with get(self.url) as response:
            first = response.read()
        self.assertEqual(served_version(first), "1.2.1000")
        self.assertTrue(said[-1].startswith("serving Test HUD 1.2.1000"), said)

        self.edit('{"layout": "BAD"}')
        os.utime(self.source / "hud.json", (2, 2))
        self.assertFalse(dev.poll())
        self.assertIn("lint: 1 finding(s); still serving 1.2.1000", said)
        self.assertTrue(any("the stub does not like BAD" in line for line in said), said)
        self.assertTrue(any("hint: remove BAD" in line for line in said), said)
        with get(self.url) as response:
            self.assertEqual(response.read(), first, "the bundle on offer is still the last clean one")

        said.clear()
        self.assertFalse(dev.poll(), "nothing changed since the failing build: it is not linted again")
        self.assertEqual(said, [])

        self.edit('{"layout": [9]}')
        os.utime(self.source / "hud.json", (3, 3))
        self.assertTrue(dev.poll())
        with get(self.url) as response:
            fixed = response.read()
        self.assertEqual(served_version(fixed), "1.2.1001")
        self.assertGreater(hud_dev.parse_version(served_version(fixed)), hud_dev.parse_version(served_version(first)))

    def test_going_back_to_what_is_being_served_builds_nothing_new(self):
        self.start()
        said = []
        dev = hud_dev.Dev(self.builder(), self.shelf, said.append)
        self.assertTrue(dev.poll())
        with get(self.url) as response:
            first = response.read()
        self.edit('{"layout": "BAD"}')
        os.utime(self.source / "hud.json", (2, 2))
        self.assertFalse(dev.poll())
        self.edit('{"layout": []}')
        os.utime(self.source / "hud.json", (3, 3))
        self.assertFalse(dev.poll())
        self.assertEqual(said[-1], "no content change; nothing rebuilt")
        with get(self.url) as response:
            self.assertEqual(response.read(), first)

    def test_a_source_with_findings_from_the_start_serves_nothing_and_keeps_watching(self):
        self.start()
        said = []
        self.edit('{"layout": "BAD"}')
        dev = hud_dev.Dev(self.builder(), self.shelf, said.append)
        self.assertFalse(dev.poll())
        self.assertIn("lint: 1 finding(s); nothing to serve yet", said)
        with self.assertRaises(urllib.error.HTTPError) as raised:
            get(self.url)
        self.assertEqual(raised.exception.code, 503)
        raised.exception.close()
        self.edit('{"layout": []}')
        os.utime(self.source / "hud.json", (5, 5))
        self.assertTrue(dev.poll())
        with get(self.url) as response:
            self.assertEqual(served_version(response.read()), "1.2.1000")

    def test_a_source_that_goes_missing_is_said_once_and_does_not_stop_the_loop(self):
        said = []
        dev = hud_dev.Dev(self.builder(), hud_dev.Shelf(), said.append)
        self.assertTrue(dev.poll())
        gone = self.source.with_name("gone")
        self.source.rename(gone)
        self.assertFalse(dev.poll())
        self.assertFalse(dev.poll())
        self.assertEqual(sum(line.startswith("cannot read the source") for line in said), 1)
        gone.rename(self.source)
        self.edit('{"layout": [4]}')
        os.utime(self.source / "hud.json", (7, 7))
        self.assertTrue(dev.poll())


class TheCommand(Workdir):
    def run_main(self, *argv):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = hud_dev.main(list(argv))
        return code, out.getvalue(), err.getvalue()

    def test_it_refuses_to_start_on_a_machine_with_no_rfc1918_address(self):
        with mock.patch.object(hud_dev, "private_addresses", return_value=[]):
            code, out, err = self.run_main(str(self.source), "--sim", str(self.sim), "--port", "0")
        self.assertEqual((code, out), (2, ""))
        self.assertIn("no RFC 1918 address", err)

    def test_it_refuses_a_named_host_the_tv_would_refuse(self):
        # The interrupt keeps a regression (the tool accepting loopback and serving) a failed assert, not a hang. It
        # replaces the loop's own `pause`, never `time.sleep`: that is the module every subprocess wait shares, and a
        # patched one stopped a build on a slow runner before it had printed `serving`.
        with mock.patch.object(hud_dev, "private_addresses", return_value=[]), \
                mock.patch.object(hud_dev, "pause", side_effect=KeyboardInterrupt):
            code, _, err = self.run_main(str(self.source), "--sim", str(self.sim), "--host", "127.0.0.1", "--port", "0")
        self.assertEqual(code, 2)
        self.assertIn("not an RFC 1918 address", err)

    def test_it_needs_a_simulator_and_a_source(self):
        with mock.patch.object(hud_dev, "private_addresses", return_value=["192.168.1.20"]):
            self.assertEqual(self.run_main(str(self.source), "--sim", str(self.dir / "absent"))[0], 2)
            self.assertEqual(self.run_main(str(self.dir / "absent"), "--sim", str(self.sim))[0], 2)

    def test_it_prints_the_address_and_the_recipe_builds_and_stops_on_interrupt(self):
        with mock.patch.object(hud_dev, "private_addresses", return_value=["192.168.1.20"]), \
                mock.patch.object(hud_dev, "pause", side_effect=KeyboardInterrupt):
            code, out, _ = self.run_main(str(self.source), "--sim", str(self.sim), "--port", "0")
        self.assertEqual(code, 0)
        lines = out.splitlines()
        self.assertRegex(lines[0], r"^TV address: http://192\.168\.1\.20:[0-9]+/my-hud\.plxhud$")
        self.assertIn("Install from address", lines[1])
        self.assertTrue(any(line.startswith("serving Test HUD 1.2.") for line in lines), lines)
        self.assertEqual(lines[-1], "stopped")


if __name__ == "__main__":
    unittest.main()
