#!/usr/bin/env python3
"""Offline tests for pms-hls-probe.py. No PMS, plex.tv, or device I/O."""

import importlib.util
import json
import sys
import tempfile
import unittest
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from types import SimpleNamespace
from unittest import mock


TOOL = Path(__file__).with_name("pms-hls-probe.py")
SPEC = importlib.util.spec_from_file_location("pms_hls_probe", TOOL)
probe = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = probe
SPEC.loader.exec_module(probe)


class PlaylistParsing(unittest.TestCase):
    def test_master_attributes_keep_quoted_codec_commas(self):
        text = """#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=425000,RESOLUTION=480x270,CODECS="avc1.4d401f,mp4a.40.2"
session/example/base/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=11356000,RESOLUTION=1920x1080
  second.m3u8
"""
        self.assertEqual(probe._playlist_kind(text), "master")
        variants = probe._master_variants(text)
        self.assertEqual(probe._playlist_uris(text), ["session/example/base/index.m3u8", "second.m3u8"])
        self.assertEqual(variants[0]["attributes"]["CODECS"], "avc1.4d401f,mp4a.40.2")
        self.assertEqual(variants[1]["attributes"]["RESOLUTION"], "1920x1080")

    def test_media_and_unknown_are_distinct(self):
        self.assertEqual(probe._playlist_kind("#EXTM3U\n#EXTINF:2,\na.ts\n"), "media")
        self.assertEqual(probe._playlist_kind("#EXTM3U\n#EXT-X-VERSION:3\n"), "unknown")

    def test_timeline_bandwidths_accept_bare_and_wrapped_shapes(self):
        expected = [{"time": 0, "bandwidth": 575, "resolution": "SD"}]
        bare = {"MediaContainer": {"Bandwidths": expected}}
        wrapped = {"MediaContainer": {"Bandwidths": {"Bandwidth": expected}}}
        self.assertEqual(probe._bandwidth_changes(json.dumps(bare).encode()), expected)
        self.assertEqual(probe._bandwidth_changes(json.dumps(wrapped).encode()), expected)
        self.assertEqual(probe._bandwidth_changes(b"not json"), [])

    def test_decision_summary_is_allowlisted_and_malformed_input_is_soft(self):
        body = json.dumps(
            {
                "MediaContainer": {
                    "resourceSession": "must-not-leak",
                    "Metadata": [{"title": "must-not-leak", "Media": [{"protocol": "hls", "Part": []}]}],
                }
            }
        ).encode()
        summary = probe._decision_summary(body)
        self.assertEqual(summary["protocol"], "hls")
        self.assertNotIn("resourceSession", summary)
        self.assertNotIn("must-not-leak", json.dumps(summary))
        self.assertEqual(probe._decision_summary(b"[]")["parse"], "failed")

    def test_status_keeps_only_probe_client_and_aliases_both_id_families(self):
        body = json.dumps(
            {
                "MediaContainer": {
                    "Metadata": [
                        {
                            "title": "must-not-leak",
                            "Player": {"machineIdentifier": probe.CID, "address": "must-not-leak"},
                            "Session": {"id": "playback-private"},
                            "TranscodeSession": {
                                "key": "/transcode/sessions/encoder-private",
                                "protocol": "hls",
                                "speed": 1.25,
                            },
                        },
                        {
                            "Player": {"machineIdentifier": "somebody-else"},
                            "Session": {"id": "other-private"},
                        },
                    ]
                }
            }
        ).encode()
        aliases = {"legacy-private": "sid-1"}
        summary, observed = probe._status_summary(body, aliases)
        self.assertEqual(observed, ("playback-private", "encoder-private"))
        self.assertEqual(summary["entries"][0]["playback_id"], "sid-2")
        self.assertEqual(summary["entries"][0]["transcode_id"], "sid-3")
        self.assertNotIn("private", json.dumps(summary))
        self.assertEqual(len(summary["entries"]), 1)


class Redaction(unittest.TestCase):
    def test_raw_and_percent_encoded_values_are_removed(self):
        origin = "http://private.example:32400"
        token = "tok+secret/value"
        session = "plxnative-probe-private"
        client_id = "plxnative-client-private"
        item = "987654"
        text = (
            f"{origin}/library/metadata/{item}?X-Plex-Token={urllib.parse.quote(token, safe='')}"
            f"&session={session}\nHTTP://PRIVATE.EXAMPLE:32400/session/{session}/index.m3u8\n"
            f"X-Plex-Token: unknown-token\nX-Plex-Client-Identifier: {client_id}\n"
        )
        clean = probe._redact(text, origin, {session: "sid-1"}, (token,), item, client_id)
        self.assertIn("<origin>", clean)
        self.assertIn("<sid-1>", clean)
        self.assertIn("/library/metadata/<item>", clean)
        self.assertNotIn("private.example", clean)
        self.assertNotIn(session, clean)
        self.assertNotIn(token, clean)
        self.assertNotIn(client_id, clean)
        self.assertNotIn("unknown-token", clean)
        probe._assert_artifact_safe(clean, origin, (session,), (token,), item, client_id)

    def test_fail_closed_scan_rejects_unknown_token_field(self):
        with self.assertRaises(RuntimeError):
            probe._assert_artifact_safe(
                "child.ts?X-Plex-Token=surprise", "http://pms:32400", (), ("known",)
            )
        with self.assertRaises(RuntimeError):
            probe._assert_artifact_safe(
                "child.ts?transcodeSessionId=surprise", "http://pms:32400", (), ("known",)
            )


class UriSafety(unittest.TestCase):
    def test_same_origin_normalizes_default_ports(self):
        self.assertTrue(probe._same_origin("http://pms/a", "http://PMS:80/b"))
        self.assertTrue(probe._same_origin("https://pms/a", "https://pms:443/b"))
        self.assertFalse(probe._same_origin("http://pms/a", "https://pms/a"))
        self.assertEqual(probe._origin("2001:db8::1", 32400), "http://[2001:db8::1]:32400")

    def test_child_rejects_cross_origin_credentials_and_control_chars(self):
        base = "http://pms:32400/video/start.m3u8"
        self.assertEqual(probe._safe_child(base, "segment/0.ts"), "http://pms:32400/video/segment/0.ts")
        for uri in ("//other:32400/x", "http://user@pms:32400/x", "segment.ts\nX:bad"):
            with self.subTest(uri=uri), self.assertRaises(RuntimeError):
                probe._safe_child(base, uri)

    def test_redirect_is_rejected_before_headers_can_cross_origin(self):
        request = urllib.request.Request(
            "http://pms:32400/start", headers={"X-Plex-Token": "secret"}
        )
        handler = probe._SameOriginRedirect("http://pms:32400/start")
        with self.assertRaises(RuntimeError):
            handler.redirect_request(request, None, 302, "Found", {}, "https://elsewhere/start")
        redirected = handler.redirect_request(request, None, 302, "Found", {}, "/same-origin")
        self.assertEqual(redirected.full_url, "http://pms:32400/same-origin")
        self.assertEqual(redirected.get_header("X-plex-token"), "secret")


class SessionWires(unittest.TestCase):
    @staticmethod
    def factory(label):
        return f"generated-{label}"

    def test_baseline_preserves_legacy_query_contract(self):
        plan = probe._session_plan("baseline", factory=self.factory)
        self.assertEqual(
            plan.query_fields(),
            {"session": "generated-baseline", "X-Plex-Session-Identifier": "generated-baseline"},
        )
        self.assertIsNone(plan.header)
        self.assertEqual(plan.candidates(), ("generated-baseline",))

    def test_named_wire_modes_and_mismatch_are_exact(self):
        legacy = probe._session_plan("legacy", factory=self.factory)
        self.assertEqual(legacy.query_fields(), {"session": "generated-legacy"})
        self.assertEqual(legacy.header, "generated-legacy")
        self.assertEqual(
            probe._request_headers("secret", "application/json", legacy.header)[
                "X-Plex-Session-Identifier"
            ],
            "generated-legacy",
        )
        canonical = probe._session_plan("canonical", factory=self.factory)
        self.assertEqual(canonical.query_fields(), {"transcodeSessionId": "generated-canonical"})
        matched = probe._session_plan("matched", factory=self.factory)
        self.assertEqual(len(matched.candidates()), 1)
        mismatch = probe._session_plan("mismatch", factory=self.factory)
        self.assertEqual(len(mismatch.candidates()), 3)
        self.assertEqual(set(mismatch.aliases().values()), {"sid-1", "sid-2", "sid-3"})
        self.assertNotIn("generated", json.dumps(mismatch.aliases()))

    def test_explicit_mode_validates_without_echoing_values(self):
        plan = probe._session_plan("explicit", legacy="left", canonical="centre", header="right")
        self.assertEqual(plan.candidates(), ("left", "centre", "right"))
        with self.assertRaisesRegex(ValueError, "ASCII") as caught:
            probe._session_plan("explicit", legacy="private value")
        self.assertNotIn("private value", str(caught.exception))

    def test_transcode_offset_is_explicit_and_defaults_to_zero(self):
        plan = probe._session_plan("baseline", factory=self.factory)
        self.assertEqual(probe._params("1", plan, False, 720, "854x480")["offset"], "0")
        self.assertEqual(probe._params("1", plan, False, 720, "854x480", 39)["offset"], "39")


def _sample(codec="h264", width=480, height=200):
    return {
        "probe": {
            "ok": True,
            "streams": [{"codec_type": "video", "codec_name": codec, "width": width, "height": height}],
        }
    }


class Classification(unittest.TestCase):
    def test_client_variants(self):
        result = probe._classification({"start": {"variant_count": 2}})
        self.assertEqual(result["kind"], "ClientVariants")

    def test_server_managed_requires_actual_segment_change(self):
        report = {
            "mode": "auto",
            "request": {"seconds_per_segment": 2, "pace_seconds": 2},
            "start": {"variant_count": 1},
            "segments": [_sample(width=480, height=200), _sample(width=854, height=358)],
        }
        self.assertEqual(probe._classification(report)["kind"], "ServerManaged")

    def test_fixed_session_requires_two_long_signalled_legs(self):
        report = {
            "mode": "auto",
            "request": {"seconds_per_segment": 2, "pace_seconds": 2},
            "start": {"variant_count": 1},
            "segments": [_sample()] * 60,
            "timeline": [
                {"reported_bandwidth_kbps": 20000},
                {"reported_bandwidth_kbps": 512},
            ],
        }
        self.assertEqual(probe._classification(report)["kind"], "FixedSession")
        report["segments"] = report["segments"][:59]
        self.assertEqual(probe._classification(report)["kind"], "Inconclusive")

    def test_status_only_change_does_not_claim_server_managed(self):
        report = {
            "mode": "auto",
            "request": {"seconds_per_segment": 2, "pace_seconds": 2},
            "start": {"variant_count": 1},
            "segments": [_sample()] * 60,
            "timeline": [
                {"reported_bandwidth_kbps": 20000, "bandwidth_changes": [{"resolution": "HD"}]},
                {"reported_bandwidth_kbps": 512, "bandwidth_changes": [{"resolution": "SD"}]},
            ],
        }
        self.assertEqual(probe._classification(report)["kind"], "FixedSession")
        report["request"]["pace_seconds"] = 0
        self.assertEqual(probe._classification(report)["kind"], "Inconclusive")


class Cleanup(unittest.TestCase):
    def test_ledger_deduplicates_and_cleanup_uses_every_candidate(self):
        ledger = probe.CleanupLedger()
        for session in ("left", "centre", "right", "left"):
            ledger.arm(session)
        seen = []

        def fake_request(url, token, accept, limit, client_id=probe.CID):
            session = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)["session"][0]
            seen.append(session)
            if session == "centre":
                raise urllib.error.HTTPError(url, 404, "absent", {}, None)
            return 200, b"", {"bytes": 0}

        aliases = {"left": "sid-1", "centre": "sid-2", "right": "sid-3"}
        result = probe._cleanup_sessions("http://pms:32400", "token", ledger, aliases, fake_request)
        self.assertEqual(seen, ["left", "centre", "right"])
        self.assertTrue(result["complete"])
        self.assertEqual(ledger.pending(), ())
        self.assertNotIn("left", json.dumps(result))

    def test_unconfirmed_cleanup_remains_armed(self):
        ledger = probe.CleanupLedger()
        ledger.arm("still-live")

        def refused(url, token, accept, limit, client_id=probe.CID):
            raise OSError("private detail must not enter the report")

        result = probe._cleanup_sessions(
            "http://pms:32400", "token", ledger, {"still-live": "sid-1"}, refused
        )
        self.assertFalse(result["complete"])
        self.assertEqual(ledger.pending(), ("still-live",))
        self.assertNotIn("private detail", json.dumps(result))

    def test_probe_arms_every_mismatch_candidate_before_decision(self):
        args = SimpleNamespace(
            item="offline-item",
            owner=True,
            auto=False,
            bitrate=720,
            resolution="854x480",
            offset=0,
            bandwidth_sequence=[20000, 512],
            segments_per_bandwidth=30,
            pace=2.0,
            out=None,
            session_mode="mismatch",
            legacy_session_id="left-private",
            canonical_session_id="centre-private",
            header_session_id="right-private",
        )
        stops = []
        client_ids = []

        def fake_request(
            url, token, accept, limit, method="GET", session_header=None, client_id=probe.CID
        ):
            client_ids.append(client_id)
            if "/decision?" in url:
                raise urllib.error.URLError("offline decision failure")
            if "/stop?" in url:
                stops.append(urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)["session"][0])
                return 200, b"", {"bytes": 0}
            raise AssertionError("unexpected offline request")

        with tempfile.TemporaryDirectory() as directory:
            args.out = Path(directory) / "artifacts"
            with mock.patch.object(
                probe, "_overlay", return_value=("pms.invalid", 32400, "item-private", None)
            ):
                with mock.patch.object(probe, "_token", return_value="token-private"):
                    with mock.patch.object(probe, "_request", side_effect=fake_request):
                        with self.assertRaises(urllib.error.URLError):
                            probe.probe(args)
            report = (args.out / "report.json").read_text()
        self.assertEqual(stops, ["left-private", "centre-private", "right-private"])
        self.assertEqual(len(set(client_ids)), 1)
        self.assertNotEqual(client_ids[0], probe.CID)
        self.assertNotIn("private", report)
        self.assertTrue(json.loads(report)["cleanup"]["complete"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
