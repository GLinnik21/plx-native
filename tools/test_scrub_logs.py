#!/usr/bin/env python3
"""Tests for `tools/scrub-logs.py`.

These exist because the scrubber was, for its whole life until 2026-08-27, **inverted relative to
the threat it was written for**. It redacted RFC1918 addresses -- which identify nobody outside the
LAN they are on -- and passed routable ones straight through. A routable address and port identify
a real machine belonging to a real person, and this application reaches other people's servers by
construction: `auth: reached "<handle>" <addr>:<port> (shared)` is logged for every server the
signed-in account can see, so a friend's shared server lands in EVERY captured device log.

Neither existing pass could see it. The declared-secret pass only knows values read out of
`PRIVATE_FILES`, and a shared server's address is in none of them -- it arrives at runtime from
plex.tv. The address pass only matched RFC1918. So 42 occurrences across 21 committed logs survived
a commit whose message was "scrub third-party and device identifiers from the captured logs", and
`tools/scrub-gate.py` -- written in response to that very incident -- inherits `load_secrets` and
cannot catch it either.

The version-string case below is not padding. PMS reports `version=1.43.3.10896-cb3ebc72d`, whose
leading run is four dot-separated numbers, and a public-address regex written without a trailing
guard turns every server version in every log into `<peer-ip-1>`.
"""

from __future__ import annotations

import importlib.util
import pathlib
import unittest

_spec = importlib.util.spec_from_file_location(
    "scrub_logs", pathlib.Path(__file__).with_name("scrub-logs.py")
)
assert _spec and _spec.loader
sl = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sl)

ROOT = pathlib.Path(__file__).resolve().parents[1]
GUARD = sl.load_guard(str(ROOT))


def scrub(text, secrets=()):
    return sl.scrub(text, GUARD, list(secrets))


class PublicAddresses(unittest.TestCase):
    """The pass that did not exist. Every assertion here fails against the previous scrubber."""

    def test_a_routable_address_is_redacted(self):
        out, _ = scrub("auth: reached \"peer\" 203.0.113.7:32400 (shared)")
        self.assertNotIn("203.0.113.7", out)
        self.assertIn("<peer-ip-1>", out)

    def test_the_port_beside_it_survives(self):
        # The port is not identifying on its own and the line has to stay readable as a result.
        out, _ = scrub("auth: reached \"peer\" 203.0.113.7:32400 (shared)")
        self.assertIn(":32400", out)

    def test_two_distinct_peers_stay_distinguishable(self):
        out, _ = scrub("a 203.0.113.7 b 198.51.100.9 c 203.0.113.7")
        self.assertIn("<peer-ip-1>", out)
        self.assertIn("<peer-ip-2>", out)
        self.assertEqual(out.count("<peer-ip-1>"), 2, "one host, one label")

    def test_the_server_handle_is_redacted_too(self):
        # The handle names the machine and usually its owner just as precisely as the address.
        out, _ = scrub('auth: reached "somebodys-server" 203.0.113.7:32400 (shared)')
        self.assertNotIn("somebodys-server", out)
        self.assertIn("<peer-name-1>", out)
        self.assertIn("auth: reached", out, "the line must still read as a reachability result")

    def test_loopback_and_link_local_are_left_alone(self):
        text = "bound 127.0.0.1:8910 and 169.254.1.1 and 0.0.0.0"
        out, _ = scrub(text)
        self.assertEqual(out, text)

    def test_private_addresses_still_get_the_lan_label_not_the_peer_one(self):
        out, _ = scrub("serving http://192.168.0.3:55124")
        self.assertIn("<lan-ip-1>", out)
        self.assertNotIn("<peer-ip-1>", out)


class VersionStringsAreNotAddresses(unittest.TestCase):
    """A dotted quad followed by more digits is a version, and every log is full of them."""

    def test_pms_version_survives_intact(self):
        text = "pms: server 0 version=1.43.3.10896-cb3ebc72d plexPass=true"
        out, _ = scrub(text)
        self.assertEqual(out, text)

    def test_a_four_part_version_that_looks_exactly_like_an_address_survives(self):
        text = "version=1.43.4.109-abc"
        out, _ = scrub(text)
        self.assertEqual(out, text)

    def test_a_cidr_block_is_not_a_host(self):
        text = "route 203.0.113.0/24 via gateway"
        out, _ = scrub(text)
        self.assertIn("203.0.113.0/24", out)


class DeclaredSecrets(unittest.TestCase):
    def test_a_declared_value_is_replaced_by_its_label(self):
        out, n = scrub("token=abcdefgh12345678", [("plex token", "abcdefgh12345678")])
        self.assertNotIn("abcdefgh12345678", out)
        self.assertIn("<plex-token>", out)
        self.assertEqual(n, 1)

    def test_longest_value_first_so_a_substring_cannot_mask_it(self):
        # If the short value were replaced first it would leave a recognisable fragment of the long
        # one behind, which is the whole reason the sort exists.
        out, _ = scrub("AAAABBBB", [("short", "AAAA"), ("long", "AAAABBBB")])
        self.assertEqual(out, "<long>")


class Counting(unittest.TestCase):
    def test_the_count_includes_every_class(self):
        out, n = scrub(
            'auth: reached "peer" 203.0.113.7:32400 tv 192.168.0.4 tok=zzzzzzzzzzzzzzzz',
            [("plex token", "zzzzzzzzzzzzzzzz")],
        )
        # one declared secret + one LAN host + one peer address + one peer handle
        self.assertEqual(n, 4)
        for expected in ("<plex-token>", "<lan-ip-1>", "<peer-ip-1>", "<peer-name-1>"):
            self.assertIn(expected, out)


class TheCommittedLogsAreClean(unittest.TestCase):
    """A regression gate on the artefacts themselves, since nothing else reads them.

    Scoped to the log directories rather than the whole tree: these are captured device output,
    the one file class that carries whatever the television happened to see.
    """

    def test_no_routable_address_survives_in_any_captured_log(self):
        offenders = []
        for path in sorted(ROOT.glob("docs/measurements/*-logs/*.log")):
            text = path.read_text(errors="replace")
            found = sl.PUBLIC_IP.findall(text)
            if found:
                # Never print the value -- that is the same disclosure by a shorter route.
                offenders.append(f"{path.relative_to(ROOT)} ({len(found)})")
        self.assertFalse(
            offenders,
            "captured logs carry routable addresses; re-run tools/scrub-logs.py over: "
            + ", ".join(offenders),
        )

    def test_no_peer_handle_survives_in_any_captured_log(self):
        offenders = []
        for path in sorted(ROOT.glob("docs/measurements/*-logs/*.log")):
            for match in sl.PEER_HANDLE.finditer(path.read_text(errors="replace")):
                if not match.group(2).startswith("<peer-name-"):
                    offenders.append(str(path.relative_to(ROOT)))
                    break
        self.assertFalse(offenders, "captured logs name reachable servers: " + ", ".join(offenders))


if __name__ == "__main__":
    unittest.main(verbosity=2)
