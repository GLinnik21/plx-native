"""The mock's Live TV fixture (`--livetv-fixture`; `docs/pms-api.md` §10): the wire shapes the Rust client reads
(`plex::livetv`), the tune and its tuner budget, the decision each channel gets, a live playlist that grows, and the
segment file `--livetv-segment` serves. With the fixture off the mock answers as it always did. Direct `handle()`
calls, no sockets.

-I drops the script's directory from sys.path, so the module adds it below.
Run: python3 -I tests/test_mock_pms_livetv.py
"""
import json
import pathlib
import re
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from mock_pms import Library, LiveTvFixture, MockPms  # noqa: E402

DVR = "/livetv/dvrs/7"
EPG = "/tv.plex.providers.epg.xmltv:7"
NOW = 1792523168  # 368 s into its half hour, so an airing's begin is not `now`
UNSORTED = ["C102.243.mock.tv", "C101.242.mock.tv", "C105.246.mock.tv", "C103.244.mock.tv", "C104.245.mock.tv"]
SESSION_KEY = re.compile(r"^/livetv/sessions/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
NO_SEGMENT = (404, "text/plain", b"mock_pms: no live segments")
GONE = "00000000-0000-4000-8000-000000000000"  # a uuid no tune minted
BUSY = {"error": "All tuners are busy"}
SEGMENT = bytes(range(256)) * 2  # what --livetv-segment serves, byte for byte


def make():
    return MockPms(Library(seed=3, movies=24, shows=6, seasons=1, episodes=2))


def make_live():
    pms = make()
    pms.livetv = LiveTvFixture()
    return pms


def call(pms, method, path, headers=None):
    return pms.handle(method, path, b"", headers or {})


def jcall(pms, method, path, want=200):
    status, ctype, data = call(pms, method, path)
    assert status == want, (path, status, data[:200])
    assert ctype == "application/json"
    return json.loads(data)["MediaContainer"]


def tune_path(cid, sid="pb1"):
    return f"{DVR}/channels/{cid}/tune?X-Plex-Session-Identifier={sid}"


def grab_uuid(mc):
    """The live session's uuid in a tune answer: its grab's key is `/livetv/sessions/<uuid>`."""
    return mc["MediaSubscription"][0]["MediaGrabOperation"][0]["Metadata"]["key"].rsplit("/", 1)[-1]


def tuned(pms, cid, sid="pb1"):
    return grab_uuid(jcall(pms, "POST", tune_path(cid, sid)))


def decision(pms, uuid, direct_play, session="pb1"):
    return jcall(pms, "GET", f"/video/:/transcode/universal/decision?path=%2Flivetv%2Fsessions%2F{uuid}"
                             f"&directPlay={direct_play}&session={session}")


def guide(pms, encoded=True):
    keys = f"beginsAt%3C={NOW}&endsAt%3E={NOW}" if encoded else f"beginsAt<={NOW}&endsAt>={NOW}"
    return jcall(pms, "GET", f"{EPG}/grid?{keys}")["Metadata"]


def segments(playlist):
    return [line.decode() for line in playlist.splitlines() if line.endswith(b".ts")]


def media_sequence(playlist):
    return int(re.search(rb"#EXT-X-MEDIA-SEQUENCE:(\d+)", playlist).group(1))


class LiveTvOff(unittest.TestCase):
    """With no fixture the Live TV paths are today's catch-all, and a library decision does not move."""

    def test_dvrs_no_dvr_row(self):
        pms = make()
        mc = json.loads(call(pms, "GET", "/livetv/dvrs")[2])["MediaContainer"]
        self.assertNotIn("Dvr", mc)
        self.assertIn("/livetv/dvrs", pms.unknown)

    def test_library_decision_same(self):
        off, on = make(), make_live()
        rk = next(k for k, it in off.lib.items.items() if it.get("type") == "movie")
        for query in ("directPlay=1", "directPlay=0&directStream=1"):
            path = f"/video/:/transcode/universal/decision?path=%2Flibrary%2Fmetadata%2F{rk}&{query}"
            self.assertEqual(call(on, "GET", path), call(off, "GET", path), query)


class LiveTvMock(unittest.TestCase):
    """The fixture's answers, one behaviour to a method."""

    def test_dvrs_shape(self):
        mc = jcall(make_live(), "GET", "/livetv/dvrs")
        self.assertEqual(mc["size"], 1)
        dvr = mc["Dvr"][0]
        self.assertEqual((dvr["key"], dvr["epgIdentifier"]), ("7", "tv.plex.providers.epg.xmltv:7"))
        device = dvr["Device"][0]
        self.assertEqual(device["status"], "dead", "discovery status is history, not health (§10)")
        self.assertEqual(device["tuners"], "2")
        self.assertEqual([m["enabled"] for m in device["ChannelMapping"]], ["1"] * 5)

    def test_lineup_order(self):
        mc = jcall(make_live(), "GET", f"{EPG}/lineups/dvr/channels")
        self.assertEqual(mc["size"], 5)
        self.assertEqual([c["id"] for c in mc["Channel"]], UNSORTED)
        self.assertTrue(all(c["gridKey"] == c["id"] for c in mc["Channel"]))

    def test_grid_four_airings(self):
        rows = guide(make_live())
        self.assertEqual(sorted(r["Media"][0]["channelVcn"] for r in rows), ["101", "102", "104", "105"])

    def test_grid_covers_now(self):
        for row in guide(make_live()):
            for m in row["Media"]:
                self.assertLessEqual(m["beginsAt"], NOW)
                self.assertLessEqual(NOW, m["endsAt"])

    def test_grid_episode(self):
        ep = next(r for r in guide(make_live()) if r["Media"][0]["channelVcn"] == "101")
        self.assertEqual((ep["type"], ep["grandparentTitle"]), ("episode", "Family Show"))
        self.assertTrue(ep["thumb"] and ep["grandparentThumb"])

    def test_grid_encoded_and_literal_keys(self):
        pms = make_live()
        self.assertEqual(len(guide(pms)), 4)
        self.assertEqual(guide(pms, encoded=False), guide(pms))

    def test_tune_needs_session_id(self):
        pms = make_live()
        status, ctype, data = call(pms, "POST", f"{DVR}/channels/C104.245.mock.tv/tune")
        self.assertEqual((status, ctype), (400, "application/json"))
        self.assertEqual(json.loads(data), {"error": "A session identifier is required for this request"})
        self.assertEqual(pms.livetv.sessions, {}, "a refused tune holds no tuner")

    def test_tune_session_key(self):
        pms = make_live()
        mc = jcall(pms, "POST", tune_path("C101.242.mock.tv"))
        grab = mc["MediaSubscription"][0]["MediaGrabOperation"][0]
        self.assertRegex(grab["Metadata"]["key"], SESSION_KEY)
        session = pms.livetv.sessions[grab_uuid(mc)]
        self.assertEqual((session["channel"], session["playback"]), ("C101.242.mock.tv", "pb1"))
        self.assertEqual([w[0] for w in pms.writes], ["POST"], "a tune is a write")

    def test_tune_session_header(self):
        pms = make_live()
        status, _, data = call(pms, "POST", f"{DVR}/channels/C104.245.mock.tv/tune",
                               {"X-Plex-Session-Identifier": "pbh"})
        self.assertEqual(status, 200)
        self.assertEqual(pms.livetv.sessions[grab_uuid(json.loads(data)["MediaContainer"])]["playback"], "pbh")

    def test_tune_unknown_dvr_or_channel(self):
        pms = make_live()
        self.assertEqual(call(pms, "POST", "/livetv/dvrs/8/channels/C104.245.mock.tv/tune"
                                           "?X-Plex-Session-Identifier=pb1")[0], 404)
        self.assertEqual(call(pms, "POST", tune_path("C999.999.mock.tv"))[0], 404)
        self.assertEqual(pms.livetv.sessions, {}, "a refused tune holds no tuner")

    def test_direct_101_is_h264_aac(self):
        pms = make_live()
        uuid = tuned(pms, "C101.242.mock.tv")
        mc = decision(pms, uuid, 1)
        self.assertEqual(mc["generalDecisionCode"], 1000)
        media = mc["Metadata"][0]["Media"][0]
        self.assertEqual((media["videoCodec"], media["audioCodec"]), ("h264", "aac"))
        part = media["Part"][0]
        self.assertEqual(part["decision"], "directplay")
        self.assertTrue(part["key"].startswith(f"/livetv/sessions/{uuid}/pb1/"), part["key"])
        self.assertTrue(part["key"].endswith("index.m3u8?offset=-1.000000"), part["key"])

    def test_direct_104_is_mpeg2_ac3(self):
        pms = make_live()
        media = decision(pms, tuned(pms, "C104.245.mock.tv"), 1)["Metadata"][0]["Media"][0]
        self.assertEqual((media["videoCodec"], media["audioCodec"]), ("mpeg2video", "ac3"))

    def test_direct_105_is_ac4(self):
        pms = make_live()
        media = decision(pms, tuned(pms, "C105.246.mock.tv"), 1)["Metadata"][0]["Media"][0]
        self.assertEqual((media["videoCodec"], media["audioCodec"]), ("h264", "ac4"))

    def test_transcode_104_is_h264_aac(self):
        pms = make_live()
        mc = decision(pms, tuned(pms, "C104.245.mock.tv"), 0)
        self.assertEqual((mc["generalDecisionCode"], mc["transcodeDecisionCode"]), (1000, 1000))
        media = mc["Metadata"][0]["Media"][0]
        self.assertEqual((media["videoCodec"], media["audioCodec"]), ("h264", "aac"))
        self.assertEqual(media["Part"][0]["decision"], "transcode")
        self.assertEqual(media["Part"][0]["Stream"], [
            {"streamType": 1, "codec": "h264", "decision": "transcode"},
            {"streamType": 2, "codec": "aac", "channels": 2, "decision": "transcode"}])

    def test_refuse_conversions_answers_2000(self):
        pms = make_live()
        uuid = tuned(pms, "C104.245.mock.tv")
        pms.refuse_conversions = True
        self.assertEqual(decision(pms, uuid, 0)["generalDecisionCode"], 2000)
        self.assertEqual(decision(pms, uuid, 1)["generalDecisionCode"], 1000, "the direct-play probe is not refused")

    def test_live_playlist_grows(self):
        pms = make_live()
        uuid = tuned(pms, "C101.242.mock.tv")
        path = f"/livetv/sessions/{uuid}/pb1/index.m3u8"
        status, ctype, first = call(pms, "GET", path)
        self.assertEqual((status, ctype), (200, "application/vnd.apple.mpegurl"))
        self.assertNotIn(b"#EXT-X-ENDLIST", first)
        seq = media_sequence(first)
        self.assertEqual(segments(first), [f"{seq + i}.ts" for i in range(6)])
        pms.livetv.sessions[uuid]["at"] -= 10  # ten seconds of air later, with no sleep
        later = call(pms, "GET", path)[2]
        self.assertNotIn(b"#EXT-X-ENDLIST", later)
        self.assertGreaterEqual(media_sequence(later) - seq, 5)

    def test_start_master_base_playlist(self):
        pms = make_live()
        uuid = tuned(pms, "C101.242.mock.tv")
        status, ctype, master = call(pms, "GET",
                                     f"/video/:/transcode/universal/start.m3u8?path=%2Flivetv%2Fsessions%2F{uuid}"
                                     "&session=tx1")
        self.assertEqual((status, ctype), (200, "application/vnd.apple.mpegurl"))
        variants = [line for line in master.decode().splitlines() if line and not line.startswith("#")]
        self.assertEqual(variants, ["session/tx1/base/index.m3u8"])
        status, ctype, base = call(pms, "GET", "/video/:/transcode/universal/" + variants[0])
        self.assertEqual((status, ctype), (200, "application/vnd.apple.mpegurl"))
        self.assertNotIn(b"#EXT-X-ENDLIST", base)
        self.assertEqual(segments(base), [f"{media_sequence(base) + i}.ts" for i in range(6)])

    def test_segments_404(self):
        pms = make_live()
        uuid = tuned(pms, "C101.242.mock.tv")
        self.assertEqual(call(pms, "GET", f"/livetv/sessions/{uuid}/pb1/0.ts"), NO_SEGMENT)
        self.assertEqual(call(pms, "GET", "/video/:/transcode/universal/session/tx1/base/0.ts"), NO_SEGMENT)

    def test_busy_503(self):
        pms = make_live()
        tuned(pms, "C104.245.mock.tv", "pb1")
        tuned(pms, "C105.246.mock.tv", "pb2")
        status, _, data = call(pms, "POST", tune_path("C101.242.mock.tv", "pb3"))
        self.assertEqual(status, 503)
        self.assertEqual(json.loads(data), BUSY)
        self.assertEqual(len(pms.livetv.sessions), 2)

    def test_stop_frees_tuner(self):
        pms = make_live()
        first = tuned(pms, "C104.245.mock.tv", "pb1")
        tuned(pms, "C105.246.mock.tv", "pb2")
        self.assertEqual(call(pms, "POST", tune_path("C101.242.mock.tv", "pb3"))[0], 503)
        status, _, _ = call(pms, "POST", f"/:/timeline?key=%2Flivetv%2Fsessions%2F{first}&state=stopped")
        self.assertEqual(status, 200)
        self.assertNotIn(first, pms.livetv.sessions)
        tuned(pms, "C101.242.mock.tv", "pb3")  # the freed tuner takes the next tune

    def test_other_states_keep_tuner(self):
        pms = make_live()
        uuid = tuned(pms, "C104.245.mock.tv", "pb1")
        tuned(pms, "C105.246.mock.tv", "pb2")
        for state in ("playing", "paused"):
            status, _, _ = call(pms, "POST", f"/:/timeline?key=%2Flivetv%2Fsessions%2F{uuid}&state={state}")
            self.assertEqual(status, 200)
        self.assertIn(uuid, pms.livetv.sessions)
        self.assertEqual(call(pms, "POST", tune_path("C101.242.mock.tv", "pb3"))[0], 503)

    def test_zero_tuners_503(self):
        pms = make_live()
        pms.livetv.tuners = 0  # what --livetv-busy sets
        status, _, data = call(pms, "POST", tune_path("C104.245.mock.tv"))
        self.assertEqual(status, 503)
        self.assertEqual(json.loads(data), BUSY)
        self.assertEqual(jcall(pms, "GET", "/livetv/dvrs")["Dvr"][0]["Device"][0]["tuners"], "0")

    def test_unknown_session_404(self):
        pms = make_live()
        self.assertEqual(call(pms, "GET", f"/video/:/transcode/universal/decision?path=%2Flivetv%2Fsessions%2F{GONE}"
                                          "&directPlay=1&session=pb1")[0], 404)
        self.assertEqual(call(pms, "GET", f"/livetv/sessions/{GONE}/pb1/index.m3u8")[0], 404)
        self.assertEqual(call(pms, "GET", "/video/:/transcode/universal/start.m3u8?path=%2Flivetv%2Fsessions%2F"
                                          f"{GONE}&session=pb1")[0], 404)
        self.assertEqual(call(pms, "GET", "/video/:/transcode/universal/session/pb1/base/index.m3u8")[0], 404)


class LiveTvSegment(unittest.TestCase):
    """`--livetv-segment` (module doc): with a segment file set, a segment of a live session, or of a live transcode,
    answers that file's bytes. Without one, or under a session that is not live, a segment answers 404."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.segment = pathlib.Path(tmp.name) / "seg.ts"
        self.segment.write_bytes(SEGMENT)

    def live_pms(self, segment=True):
        pms = make_live()
        if segment:
            pms.livetv.segment = self.segment
        return pms

    def start(self, pms, uuid):
        """The transcode start of a live session under the session `tx1`, whose segments are `tx1`'s."""
        return call(pms, "GET", f"/video/:/transcode/universal/start.m3u8?path=%2Flivetv%2Fsessions%2F{uuid}"
                                "&session=tx1")

    def test_session_segment_is_the_file(self):
        pms = self.live_pms()
        uuid = tuned(pms, "C101.242.mock.tv")
        self.assertEqual(call(pms, "GET", f"/livetv/sessions/{uuid}/pb1/0.ts"), (200, "video/mp2t", SEGMENT))

    def test_transcode_segment_is_the_file(self):
        pms = self.live_pms()
        uuid = tuned(pms, "C101.242.mock.tv")
        self.assertEqual(self.start(pms, uuid)[0], 200)
        self.assertEqual(call(pms, "GET", "/video/:/transcode/universal/session/tx1/base/0.ts"),
                         (200, "video/mp2t", SEGMENT))

    def test_unknown_session_segment_404(self):
        pms = self.live_pms()
        self.assertEqual(call(pms, "GET", f"/livetv/sessions/{GONE}/pb1/0.ts"), NO_SEGMENT)
        self.assertEqual(call(pms, "GET", "/video/:/transcode/universal/session/tx9/base/0.ts"), NO_SEGMENT)

    def test_stopped_session_segment_404(self):
        pms = self.live_pms()
        uuid = tuned(pms, "C101.242.mock.tv")
        self.assertEqual(self.start(pms, uuid)[0], 200)
        self.assertEqual(call(pms, "GET", f"/livetv/sessions/{uuid}/pb1/0.ts")[0], 200)
        self.assertEqual(call(pms, "POST", f"/:/timeline?key=%2Flivetv%2Fsessions%2F{uuid}&state=stopped")[0], 200)
        self.assertEqual(call(pms, "GET", f"/livetv/sessions/{uuid}/pb1/0.ts"), NO_SEGMENT)
        self.assertEqual(call(pms, "GET", "/video/:/transcode/universal/session/tx1/base/0.ts"), NO_SEGMENT)

    def test_live_without_segment_file_404(self):
        pms = self.live_pms(segment=False)
        uuid = tuned(pms, "C101.242.mock.tv")
        self.assertEqual(self.start(pms, uuid)[0], 200)
        self.assertEqual(call(pms, "GET", f"/livetv/sessions/{uuid}/pb1/0.ts"), NO_SEGMENT)
        self.assertEqual(call(pms, "GET", "/video/:/transcode/universal/session/tx1/base/0.ts"), NO_SEGMENT)


if __name__ == "__main__":
    unittest.main()
