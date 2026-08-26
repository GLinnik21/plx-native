#!/usr/bin/env python3
"""Probe the configured PMS's real HLS universal-transcoder contract.

This is intentionally a developer tool, not part of playback.  It reads the same
gitignored overlay/token as tests/run.py, registers one short-lived transcode session,
captures redacted playlists plus derived ffprobe facts, and stops the session in a
finally block.  Tokens, server addresses, titles, media bytes and rating keys never
reach the report or stdout.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import Path
from typing import Optional


ROOT = Path(__file__).resolve().parents[1]
OVERLAY = ROOT / "tests" / "manifest.local.json"
CONFIG = ROOT / "src" / "config.local.h"
CID = "plxnative-hls-probe"
PROFILE = (
    "add-transcode-target(type=videoProfile&context=streaming&protocol=hls"
    "&container=mpegts&videoCodec=h264&audioCodec=aac)"
)
MAX_MANIFEST = 1024 * 1024
MAX_SEGMENT = 32 * 1024 * 1024
MIN_FIXED_SESSION_SAMPLES = 60
SESSION_RE = re.compile(r"^[A-Za-z0-9._-]{1,160}$")


@dataclass(frozen=True)
class SessionPlan:
    """The independently observable PMS session-id wires for one probe leg.

    `query_x` preserves the legacy baseline this probe first measured: both
    `session` and `X-Plex-Session-Identifier` travelled as query parameters.
    The other modes put the X-Plex identifier in its documented HTTP header so
    a mismatch run can distinguish encoder-path and playback-correlation IDs.
    """

    mode: str
    legacy: Optional[str] = None
    canonical: Optional[str] = None
    query_x: Optional[str] = None
    header: Optional[str] = None

    def query_fields(self):
        fields = {}
        if self.legacy:
            fields["session"] = self.legacy
        if self.canonical:
            fields["transcodeSessionId"] = self.canonical
        if self.query_x:
            fields["X-Plex-Session-Identifier"] = self.query_x
        return fields

    def candidates(self):
        ordered = []
        for value in (self.legacy, self.canonical, self.query_x, self.header):
            if value and value not in ordered:
                ordered.append(value)
        return tuple(ordered)

    def aliases(self):
        by_value = {value: f"sid-{index + 1}" for index, value in enumerate(self.candidates())}
        return {
            key: by_value[value]
            for key, value in (
                ("legacy_query", self.legacy),
                ("canonical_query", self.canonical),
                ("x_plex_query", self.query_x),
                ("x_plex_header", self.header),
            )
            if value
        }


class CleanupLedger:
    """Deduplicated session IDs which must be stopped before the tool exits."""

    def __init__(self):
        self._pending = []

    def arm(self, session: str):
        _validate_session(session)
        if session not in self._pending:
            self._pending.append(session)

    def retire(self, session: str):
        if session in self._pending:
            self._pending.remove(session)

    def pending(self):
        return tuple(self._pending)


def _validate_session(value: str) -> str:
    if not SESSION_RE.fullmatch(value or ""):
        raise ValueError("session IDs must be 1-160 ASCII letters, digits, dot, underscore, or dash")
    return value


def _fresh_session(label: str) -> str:
    return f"plxnative-probe-{label}-{uuid.uuid4().hex}"


def _session_plan(mode: str, legacy=None, canonical=None, header=None, factory=_fresh_session):
    supplied = [value for value in (legacy, canonical, header) if value is not None]
    for value in supplied:
        _validate_session(value)

    if mode == "baseline":
        if canonical is not None or header is not None:
            raise ValueError("baseline accepts only --legacy-session-id; use explicit for separate wires")
        shared = legacy or factory("baseline")
        return SessionPlan(mode, legacy=shared, query_x=shared)
    if mode == "legacy":
        if canonical is not None:
            raise ValueError("legacy mode cannot set a canonical session ID")
        shared = legacy or header or factory("legacy")
        if legacy and header and legacy != header:
            raise ValueError("legacy mode requires matching legacy and header IDs")
        return SessionPlan(mode, legacy=shared, header=shared)
    if mode == "canonical":
        if legacy is not None:
            raise ValueError("canonical mode cannot set a legacy session ID")
        shared = canonical or header or factory("canonical")
        if canonical and header and canonical != header:
            raise ValueError("canonical mode requires matching canonical and header IDs")
        return SessionPlan(mode, canonical=shared, header=shared)
    if mode == "matched":
        shared = next(iter(supplied), None) or factory("matched")
        if any(value != shared for value in supplied):
            raise ValueError("matched mode requires all supplied session IDs to be equal")
        return SessionPlan(mode, legacy=shared, canonical=shared, header=shared)
    if mode == "mismatch":
        plan = SessionPlan(
            mode,
            legacy=legacy or factory("legacy"),
            canonical=canonical or factory("canonical"),
            header=header or factory("header"),
        )
        if len(plan.candidates()) != 3:
            raise ValueError("mismatch mode requires three distinct session IDs")
        return plan
    if mode == "explicit":
        if not supplied:
            raise ValueError("explicit mode requires at least one session-ID option")
        return SessionPlan(mode, legacy=legacy, canonical=canonical, header=header)
    raise ValueError("unknown session mode")


def _token() -> str:
    match = re.search(r'#define\s+PMS_TOKEN\s+"([^"]+)"', CONFIG.read_text())
    if not match:
        raise SystemExit(f"no PMS_TOKEN in {CONFIG}")
    return match.group(1)


def _overlay(item: str):
    doc = json.loads(OVERLAY.read_text())
    pms = doc.get("pms") or {}
    rk = (doc.get("items") or {}).get(item)
    if not pms.get("host") or not rk:
        raise SystemExit(f"{OVERLAY} needs pms.host and items.{item}")
    test_user = doc.get("test_user") or {}
    return str(pms["host"]), int(pms.get("port") or 32400), str(rk), test_user.get("id")


def _managed_token(owner_token: str, host: str, port: int, user_id, client_id: str = CID) -> str:
    if user_id is None:
        raise SystemExit(f"{OVERLAY} has no test_user.id; pass --owner only if mutating owner history is intended")
    root_url = f"{_origin(host, port)}/"
    _, body, _ = _request(
        root_url, owner_token, "application/json", MAX_MANIFEST, client_id=client_id
    )
    try:
        machine_id = json.loads(body)["MediaContainer"]["machineIdentifier"]
    except (KeyError, TypeError, json.JSONDecodeError):
        raise SystemExit("configured PMS root did not expose a machineIdentifier") from None
    request = urllib.request.Request(
        f"https://plex.tv/api/servers/{machine_id}/shared_servers",
        headers={"X-Plex-Token": owner_token, "X-Plex-Client-Identifier": client_id},
    )
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), _SameOriginRedirect(request.full_url)
    )
    with opener.open(request, timeout=20) as response:
        root = ET.fromstring(response.read())
    for server in root.findall("SharedServer"):
        if server.get("userID") == str(user_id) and server.get("accessToken"):
            return server.get("accessToken")
    raise SystemExit("configured test user has no per-server access token")


def _origin(host: str, port: int) -> str:
    authority = f"[{host}]" if ":" in host and not host.startswith("[") else host
    return f"http://{authority}:{port}"


def _origin_key(url: str):
    if any(char in url for char in "\r\n\0"):
        return None
    try:
        parsed = urllib.parse.urlsplit(url)
        port = parsed.port
    except ValueError:
        return None
    if parsed.scheme.lower() not in ("http", "https") or not parsed.hostname:
        return None
    if parsed.username is not None or parsed.password is not None:
        return None
    if port is None:
        port = 443 if parsed.scheme.lower() == "https" else 80
    return parsed.scheme.lower(), parsed.hostname.lower(), port


def _same_origin(base: str, target: str) -> bool:
    a, b = _origin_key(base), _origin_key(target)
    return a is not None and a == b


def _secret_spellings(value: str):
    return tuple(dict.fromkeys((value, urllib.parse.quote(value, safe=""), urllib.parse.quote_plus(value))))


def _redact(
    text: str,
    origin: str,
    sessions,
    tokens,
    item: Optional[str] = None,
    client_id: Optional[str] = None,
) -> str:
    if isinstance(sessions, dict):
        session_aliases = sessions
    else:
        if isinstance(sessions, str):
            sessions = (sessions,)
        session_aliases = {value: f"sid-{index + 1}" for index, value in enumerate(sessions)}
    if isinstance(tokens, str):
        tokens = (tokens,)
    for token in tokens:
        for spelling in _secret_spellings(token):
            text = text.replace(spelling, "<token>")
    for session, alias in session_aliases.items():
        for spelling in _secret_spellings(session):
            text = text.replace(spelling, f"<{alias}>")
    if client_id:
        for spelling in _secret_spellings(client_id):
            text = text.replace(spelling, "<client>")
    for spelling in _secret_spellings(origin):
        text = text.replace(spelling, "<origin>")
    if item:
        media_path = f"/library/metadata/{item}"
        for spelling in _secret_spellings(media_path):
            text = text.replace(spelling, "/library/metadata/<item>")
        text = re.sub(rf"(?i)(ratingKey(?:=|%3D)){re.escape(item)}(?=\b|&)", r"\1<item>", text)
    # Fail closed even when a future PMS puts a token into a URI which did not
    # originate in this process (and therefore is not one of `tokens`).
    text = re.sub(
        r"(?i)(X-Plex-Token)(?:=|%3D|:\s*)[^&\s\"'<>]+",
        r"\1=<token>",
        text,
    )
    text = re.sub(
        r"(?i)(/session/)(?!<sid-[0-9]+>)[^/?#\s]+",
        r"\1<session>",
        text,
    )
    text = re.sub(
        r"(?i)((?:session|transcodeSessionId|X-Plex-Session-Identifier)(?:=|%3D))"
        r"(?!<sid-[0-9]+>)[^&\s\"'<>]+",
        r"\1<session>",
        text,
    )
    # Artifacts never need an authority. Removing every absolute HTTP(S)
    # authority also covers harmless PMS spelling differences (host case,
    # explicit default port) which exact replacement cannot anticipate.
    text = re.sub(r"(?i)https?://[^/\s?#]+", "<origin>", text)
    return text


def _assert_artifact_safe(
    text: str,
    origin: str,
    sessions,
    tokens,
    item: Optional[str] = None,
    client_id: Optional[str] = None,
):
    secrets = [origin, *sessions, *tokens, client_id]
    for secret in secrets:
        if secret and any(spelling in text for spelling in _secret_spellings(secret)):
            raise RuntimeError("refusing to write an artifact containing a configured secret or identifier")
    if item:
        media_path = f"/library/metadata/{item}"
        if any(spelling in text for spelling in _secret_spellings(media_path)):
            raise RuntimeError("refusing to write an artifact containing the item ID")
    if re.search(r"(?i)X-Plex-Token(?:=|%3D|:\s*)(?!<token>)", text):
        raise RuntimeError("refusing to write an artifact containing an unredacted token field")
    if re.search(r"(?i)/session/(?!<(?:sid-[0-9]+|session)>)[^/?#\s]+", text):
        raise RuntimeError("refusing to write an artifact containing an unredacted session path")
    if re.search(
        r"(?i)(?:session|transcodeSessionId|X-Plex-Session-Identifier)(?:=|%3D)"
        r"(?!<(?:sid-[0-9]+|session)>)[^&\s\"'<>]+",
        text,
    ):
        raise RuntimeError("refusing to write an artifact containing an unredacted session field")
    if re.search(r"(?i)https?://[^/\s?#]+", text):
        raise RuntimeError("refusing to write an artifact containing a server authority")


def _write_artifact(
    path: Path,
    text: str,
    origin: str,
    sessions,
    tokens,
    item: Optional[str] = None,
    client_id: Optional[str] = None,
):
    _assert_artifact_safe(text, origin, sessions, tokens, item, client_id)
    path.write_text(text)


def _prepare_output(path: Optional[Path]) -> Path:
    if path is None:
        resolved = Path(tempfile.mkdtemp(prefix="plxnative-hls-probe-"))
    else:
        resolved = path.expanduser().resolve()
        if resolved == ROOT or ROOT in resolved.parents:
            raise SystemExit("--out must stay outside the repository")
        resolved.mkdir(parents=True, exist_ok=True)
    os.chmod(resolved, 0o700)
    return resolved


class _SameOriginRedirect(urllib.request.HTTPRedirectHandler):
    """Reject a redirect before urllib can forward X-Plex headers to it."""

    def __init__(self, origin: str):
        super().__init__()
        self.origin = origin

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        target = urllib.parse.urljoin(req.full_url, newurl)
        if not _same_origin(self.origin, target):
            raise RuntimeError("PMS redirected the probe across origins")
        return super().redirect_request(req, fp, code, msg, headers, target)


def _request_headers(
    token: str, accept: str, session_header: Optional[str] = None, client_id: str = CID
):
    headers = {
        "Accept": accept,
        "X-Plex-Token": token,
        "X-Plex-Client-Identifier": client_id,
        "X-Plex-Product": "PlxNative ABR Probe",
        "X-Plex-Version": "1",
        "X-Plex-Platform": "webOS",
        "X-Plex-Device": "TV",
        "X-Plex-Model": "PlxNative",
        "X-Plex-Client-Profile-Name": "Generic",
        "X-Plex-Client-Profile-Extra": PROFILE,
    }
    if session_header:
        headers["X-Plex-Session-Identifier"] = session_header
    return headers


def _request(
    url: str,
    token: str,
    accept: str,
    limit: int,
    method: str = "GET",
    session_header: Optional[str] = None,
    client_id: str = CID,
):
    req = urllib.request.Request(
        url,
        method=method,
        headers=_request_headers(token, accept, session_header, client_id),
    )
    started = time.monotonic()
    # The configured PMS is often cleartext on a LAN. Never allow ambient
    # HTTP_PROXY settings to receive its credential-bearing request.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), _SameOriginRedirect(url))
    with opener.open(req, timeout=30) as response:
        first = time.monotonic()
        body = response.read(limit + 1)
        finished = time.monotonic()
        if len(body) > limit:
            raise RuntimeError(f"response exceeded the {limit}-byte probe bound")
        final_url = response.geturl()
        if not _same_origin(url, final_url):
            raise RuntimeError("PMS redirected the probe across origins")
        return response.status, body, {
            "ttfb_ms": round((first - started) * 1000, 1),
            "body_ms": round((finished - first) * 1000, 1),
            "total_ms": round((finished - started) * 1000, 1),
            "bytes": len(body),
        }


def _params(
    rk: str,
    sessions: SessionPlan,
    auto: bool,
    bitrate: int,
    resolution: str,
    offset: int = 0,
):
    params = {
        "path": f"/library/metadata/{rk}",
        "mediaIndex": "0",
        "partIndex": "0",
        "protocol": "hls",
        "directPlay": "0",
        "directStream": "0",
        "directStreamAudio": "0",
        "subtitles": "none",
        "autoAdjustQuality": "1" if auto else "0",
        "videoBitrate": str(bitrate),
        "maxVideoBitrate": "20000" if auto else str(bitrate),
        "peakBitrate": "20000" if auto else str(bitrate),
        "videoResolution": resolution,
        "videoQuality": "100",
        "secondsPerSegment": "2",
        "mediaBufferSize": "20971",
        "location": "lan",
        "fastSeek": "1",
        "offset": str(offset),
    }
    params.update(sessions.query_fields())
    return params


def _playlist_uris(text: str):
    lines = (line.strip() for line in text.splitlines())
    return [line for line in lines if line and not line.startswith("#")]


def _playlist_kind(text: str) -> str:
    if "#EXT-X-STREAM-INF" in text:
        return "master"
    if "#EXTINF" in text or "#EXT-X-MAP" in text:
        return "media"
    return "unknown"


def _master_variants(text: str):
    variants = []
    pending = None
    for line in (raw.strip() for raw in text.splitlines()):
        if line.startswith("#EXT-X-STREAM-INF:"):
            if pending is not None:
                variants.append({"attributes": pending, "uri": None})
            pending = {}
            raw = line.split(":", 1)[1]
            for match in re.finditer(r'([A-Za-z0-9-]+)=("[^"]*"|[^,]*)', raw):
                value = match.group(2)
                pending[match.group(1).upper()] = value[1:-1] if value.startswith('"') else value
            continue
        if pending is not None and line and not line.startswith("#"):
            variants.append({"attributes": pending, "uri": line})
            pending = None
    if pending is not None:
        variants.append({"attributes": pending, "uri": None})
    return variants


def _safe_child(base: str, uri: str) -> str:
    if not uri or any(char in uri for char in "\r\n\0"):
        raise RuntimeError("playlist named an invalid child URI")
    child = urllib.parse.urljoin(base, uri)
    if not _same_origin(base, child):
        raise RuntimeError("playlist named a cross-origin child")
    return child


def _ffprobe(segment: bytes):
    if not shutil.which("ffprobe"):
        return {"available": False}
    with tempfile.NamedTemporaryFile(suffix=".segment") as tmp:
        tmp.write(segment)
        tmp.flush()
        proc = subprocess.run(
            [
                "ffprobe",
                "-v",
                "error",
                "-show_entries",
                "format=format_name,duration:stream=index,codec_type,codec_name,width,height,time_base:packet=stream_index,pts,dts,duration",
                "-of",
                "json",
                tmp.name,
            ],
            capture_output=True,
            text=True,
            timeout=20,
        )
    if proc.returncode:
        return {"available": True, "ok": False, "error": proc.stderr.strip()[:300]}
    out = json.loads(proc.stdout)
    packet_timing = {}
    for packet in out.pop("packets", []):
        stream = str(packet.get("stream_index", "unknown"))
        summary = packet_timing.setdefault(
            stream,
            {"count": 0, "missing_pts": 0, "missing_dts": 0, "missing_both": 0, "first": []},
        )
        summary["count"] += 1
        missing_pts = packet.get("pts") in (None, "N/A")
        missing_dts = packet.get("dts") in (None, "N/A")
        summary["missing_pts"] += int(missing_pts)
        summary["missing_dts"] += int(missing_dts)
        summary["missing_both"] += int(missing_pts and missing_dts)
        if len(summary["first"]) < 8:
            summary["first"].append(
                {key: packet.get(key) for key in ("pts", "dts", "duration") if key in packet}
            )
    out["packet_timing"] = packet_timing
    out.update({"available": True, "ok": True})
    return out


def _bandwidth_changes(body: bytes):
    changes = []
    try:
        mc = json.loads(body).get("MediaContainer", {})
        raw = mc.get("Bandwidths") or []
        # PMS currently emits this as a bare JSON list while the published schema models an
        # XML-shaped {Bandwidth: [...]} wrapper. Accept both; the report normalizes them below.
        changes = raw.get("Bandwidth", []) if isinstance(raw, dict) else raw
    except (AttributeError, UnicodeDecodeError, json.JSONDecodeError):
        pass
    clean = []

    def visit(value):
        if isinstance(value, list):
            for child in value:
                visit(child)
            return
        if not isinstance(value, dict):
            return
        normalized = {}
        for key in ("time", "bandwidth", "resolution"):
            found = value.get(key, value.get(key[:1].upper() + key[1:]))
            if found is not None and not isinstance(found, (dict, list)):
                normalized[key] = found
        if normalized:
            clean.append(normalized)
        else:
            for child in value.values():
                visit(child)

    visit(changes)
    return clean


def _timeline(origin: str, token: str, rk: str, sessions: SessionPlan, time_ms: int,
              duration_ms: int, bandwidth: int, client_id: str = CID):
    params = {
        "ratingKey": rk,
        "key": f"/library/metadata/{rk}",
        "identifier": "com.plexapp.plugins.library",
        "state": "playing",
        "time": str(time_ms),
        "duration": str(duration_ms),
        "bandwidth": str(bandwidth),
        # The published prose says seconds, but official-client logs and PMS's own timeline
        # examples carry milliseconds (e.g. 2589 for ~2.6 s). Ten here would report a 10 ms
        # starvation and correctly prevent every upshift, so send ten seconds in wire units.
        "bufferedTime": "10000",
    }
    if sessions.query_x:
        params["X-Plex-Session-Identifier"] = sessions.query_x
    query = urllib.parse.urlencode(params)
    status, body, timing = _request(
        f"{origin}/:/timeline?{query}",
        token,
        "application/json",
        MAX_MANIFEST,
        method="POST",
        session_header=sessions.header,
        client_id=client_id,
    )
    return {"status": status, "timing": timing, "bandwidth_changes": _bandwidth_changes(body)}


def _segment_sample(url: str, token: str, index: int, client_id: str = CID):
    status, segment, timing = _request(
        url, token, "application/octet-stream", MAX_SEGMENT, client_id=client_id
    )
    return {
        "index": index,
        "status": status,
        "timing": timing,
        "sha256": hashlib.sha256(segment).hexdigest(),
        "probe": _ffprobe(segment),
    }


def _decision_summary(body: bytes):
    try:
        mc = json.loads(body).get("MediaContainer", {})
    except (AttributeError, UnicodeDecodeError, json.JSONDecodeError):
        return {"parse": "failed", "bytes": len(body)}

    def first_dict(value):
        return value[0] if isinstance(value, list) and value and isinstance(value[0], dict) else {}

    metadata = first_dict(mc.get("Metadata"))
    media = first_dict(metadata.get("Media"))
    part = first_dict(media.get("Part"))
    streams = []
    for stream in part.get("Stream") or []:
        if not isinstance(stream, dict):
            continue
        streams.append(
            {
                key: stream.get(key)
                for key in ("streamType", "codec", "decision", "bitrate", "width", "height", "location")
                if key in stream
            }
        )
    return {
        "generalDecisionCode": mc.get("generalDecisionCode"),
        "transcodeDecisionCode": mc.get("transcodeDecisionCode"),
        "protocol": media.get("protocol") or part.get("protocol"),
        "container": media.get("container") or part.get("container"),
        "videoCodec": media.get("videoCodec"),
        "audioCodec": media.get("audioCodec"),
        "width": media.get("width") or part.get("width"),
        "height": media.get("height") or part.get("height"),
        "bitrate": media.get("bitrate") or part.get("bitrate"),
        "streams": streams,
    }


def _status_summary(body: bytes, aliases, client_id: str = CID):
    """Return only this probe client's session IDs, represented by aliases."""
    try:
        mc = json.loads(body).get("MediaContainer", {})
    except (AttributeError, UnicodeDecodeError, json.JSONDecodeError):
        return {"parse": "failed"}, ()
    observed = []

    def remember(value):
        if not isinstance(value, str) or not SESSION_RE.fullmatch(value):
            return None
        if value not in aliases:
            aliases[value] = f"sid-{len(aliases) + 1}"
        if value not in observed:
            observed.append(value)
        return aliases[value]

    entries = []
    for metadata in mc.get("Metadata") or []:
        if not isinstance(metadata, dict):
            continue
        player = metadata.get("Player") or {}
        if not isinstance(player, dict) or player.get("machineIdentifier") != client_id:
            continue
        session = metadata.get("Session") or {}
        transcode = metadata.get("TranscodeSession") or {}
        raw_key = transcode.get("key") if isinstance(transcode, dict) else None
        transcode_id = None
        if isinstance(raw_key, str):
            transcode_id = raw_key.rstrip("/").rsplit("/", 1)[-1].split("?", 1)[0]
        clean = {
            "playback_id": remember(session.get("id") if isinstance(session, dict) else None),
            "transcode_id": remember(transcode_id),
        }
        if isinstance(transcode, dict):
            for key in ("protocol", "progress", "speed", "throttled"):
                if key in transcode and not isinstance(transcode[key], (dict, list)):
                    clean[key] = transcode[key]
        entries.append({key: value for key, value in clean.items() if value is not None})
    return {"entries": entries}, tuple(observed)


def _capture_status(origin: str, owner_token: str, aliases, client_id: str = CID):
    status, body, timing = _request(
        f"{origin}/status/sessions",
        owner_token,
        "application/json",
        MAX_MANIFEST,
        client_id=client_id,
    )
    summary, observed = _status_summary(body, aliases, client_id)
    summary.update({"status": status, "timing": timing})
    return summary, observed


def _video_shapes(report):
    shapes = set()
    for sample in report.get("segments") or []:
        probe = sample.get("probe") or {}
        if not probe.get("ok"):
            continue
        for stream in probe.get("streams") or []:
            if stream.get("codec_type") == "video":
                shapes.add((stream.get("codec_name"), stream.get("width"), stream.get("height")))
    return shapes


def _classification(report):
    variants = int((report.get("start") or {}).get("variant_count") or 0)
    shapes = _video_shapes(report)
    sampled = len(report.get("segments") or [])
    reported = {
        item.get("reported_bandwidth_kbps")
        for item in report.get("timeline") or []
        if item.get("reported_bandwidth_kbps") is not None
    }
    request = report.get("request") or {}
    segment_seconds = float(request.get("seconds_per_segment") or 0)
    paced_realtime = segment_seconds > 0 and float(request.get("pace_seconds") or 0) >= segment_seconds
    evidence = {
        "master_variants": variants,
        "sampled_segments": sampled,
        "actual_video_shapes": len(shapes),
        "reported_bandwidth_legs": len(reported),
        "paced_realtime": paced_realtime,
    }
    if variants > 1:
        return {"kind": "ClientVariants", "evidence": evidence}
    if variants == 1 and len(shapes) > 1:
        return {"kind": "ServerManaged", "evidence": evidence}
    if (
        report.get("mode") == "auto"
        and variants == 1
        and len(shapes) == 1
        and sampled >= MIN_FIXED_SESSION_SAMPLES
        and len(reported) >= 2
        and paced_realtime
    ):
        return {"kind": "FixedSession", "evidence": evidence}
    return {"kind": "Inconclusive", "evidence": evidence}


def _cleanup_sessions(
    origin: str, token: str, ledger: CleanupLedger, aliases, request_fn=None, client_id: str = CID
):
    if request_fn is None:
        request_fn = _request
    attempts = []
    for session in ledger.pending():
        query = urllib.parse.urlencode({"session": session, "X-Plex-Client-Identifier": client_id})
        url = f"{origin}/video/:/transcode/universal/stop?{query}"
        settled = False
        attempt = {"alias": aliases[session]}
        try:
            status, _, timing = request_fn(
                url, token, "*/*", MAX_MANIFEST, client_id=client_id
            )
            attempt.update({"status": status, "timing": timing})
            settled = 200 <= status < 300
        except urllib.error.HTTPError as error:
            # A mismatch leg deliberately arms IDs which PMS may not adopt. 404
            # proves that candidate is absent and is therefore a settled cleanup.
            attempt["status"] = error.code
            settled = error.code == 404
        except Exception as error:  # cleanup failure must be loud but never reveal its message
            attempt["error"] = type(error).__name__
        attempt["settled"] = settled
        attempts.append(attempt)
        if settled:
            ledger.retire(session)
    return {"complete": not ledger.pending(), "attempts": attempts}


def probe(args):
    host, port, rk, test_user_id = _overlay(args.item)
    owner_token = _token()
    client_id = f"{CID}-{uuid.uuid4().hex}"
    token = (
        owner_token
        if args.owner
        else _managed_token(owner_token, host, port, test_user_id, client_id)
    )
    origin = _origin(host, port)
    sessions = _session_plan(
        args.session_mode,
        legacy=args.legacy_session_id,
        canonical=args.canonical_session_id,
        header=args.header_session_id,
    )
    aliases = {session: f"sid-{index + 1}" for index, session in enumerate(sessions.candidates())}
    ledger = CleanupLedger()
    # Arm every possible owner before the decision which may create one. The
    # mismatch experiment cannot know in advance which wire PMS will adopt.
    for session in sessions.candidates():
        ledger.arm(session)
    out = _prepare_output(args.out)
    params = _params(rk, sessions, args.auto, args.bitrate, args.resolution, args.offset)
    query = urllib.parse.urlencode(params)
    report = {
        "schema": 2,
        "identity": "owner" if args.owner else "managed-test-user",
        "mode": "auto" if args.auto else "fixed",
        "request": {
            "bitrate_kbps": args.bitrate,
            "peak_kbps": 20000 if args.auto else args.bitrate,
            "resolution": args.resolution,
            "offset_seconds": args.offset,
            "seconds_per_segment": 2,
            "segments_per_bandwidth": args.segments_per_bandwidth,
            "pace_seconds": args.pace,
            "session_mode": sessions.mode,
            "session_wires": sessions.aliases(),
        },
    }
    stopped = False
    try:
        decision_url = f"{origin}/video/:/transcode/universal/decision?{query}"
        status, body, timing = _request(
            decision_url,
            token,
            "application/json",
            MAX_MANIFEST,
            session_header=sessions.header,
            client_id=client_id,
        )
        report["decision"] = {
            "status": status,
            "timing": timing,
            "summary": _decision_summary(body),
        }

        start_url = f"{origin}/video/:/transcode/universal/start.m3u8?{query}"
        status, body, timing = _request(
            start_url,
            token,
            "application/vnd.apple.mpegurl",
            MAX_MANIFEST,
            session_header=sessions.header,
            client_id=client_id,
        )
        playlist = body.decode("utf-8", "replace")
        report["start"] = {"status": status, "timing": timing, "kind": _playlist_kind(playlist)}
        playlists = [("start.m3u8", start_url, playlist)]
        status_snapshot, observed = _capture_status(origin, owner_token, aliases, client_id)
        status_snapshot["stage"] = "after_start"
        report.setdefault("session_status", []).append(status_snapshot)
        for observed_session in observed:
            ledger.arm(observed_session)

        if _playlist_kind(playlist) == "master":
            children = _playlist_uris(playlist)
            report["start"]["variant_count"] = len(children)
            report["start"]["variant_attributes"] = [
                {
                    key: variant["attributes"][key]
                    for key in ("BANDWIDTH", "RESOLUTION", "FRAME-RATE", "CODECS")
                    if key in variant["attributes"]
                }
                for variant in _master_variants(playlist)
            ]
            for index, uri in enumerate(children):
                child_url = _safe_child(start_url, uri)
                child_status, child_body, child_timing = _request(
                    child_url,
                    token,
                    "application/vnd.apple.mpegurl",
                    MAX_MANIFEST,
                    client_id=client_id,
                )
                child_text = child_body.decode("utf-8", "replace")
                playlists.append((f"variant-{index}.m3u8", child_url, child_text))
                report.setdefault("variants", []).append(
                    {
                        "status": child_status,
                        "timing": child_timing,
                        "kind": _playlist_kind(child_text),
                    }
                )

        media = next(((name, url, text) for name, url, text in playlists if _playlist_kind(text) == "media"), None)
        if media:
            _, media_url, media_text = media
            uris = _playlist_uris(media_text)
            report["media"] = {"child_count": len(uris)}
            if uris:
                sample_limit = 1 if args.auto else args.fixed_segments
                report["segments"] = [
                    _segment_sample(
                        _safe_child(media_url, uris[index]), token, index, client_id
                    )
                    for index in range(min(sample_limit, len(uris)))
                ]
                if args.auto:
                    duration_ms = len(uris) * 2000
                    index = 0
                    for bandwidth in args.bandwidth_sequence:
                        for leg_index in range(1, args.segments_per_bandwidth + 1):
                            index += 1
                            if index >= len(uris):
                                break
                            if leg_index == 1 or leg_index % 5 == 0:
                                signal = _timeline(
                                    origin,
                                    token,
                                    rk,
                                    sessions,
                                    index * 2000,
                                    duration_ms,
                                    bandwidth,
                                    client_id,
                                )
                                signal["reported_bandwidth_kbps"] = bandwidth
                                signal["before_segment"] = index
                                report.setdefault("timeline", []).append(signal)
                            sample = _segment_sample(
                                _safe_child(media_url, uris[index]), token, index, client_id
                            )
                            report["segments"].append(sample)
                            if args.pace > 0:
                                elapsed = sample["timing"]["total_ms"] / 1000.0
                                time.sleep(max(0.0, args.pace - elapsed))

        status_snapshot, observed = _capture_status(origin, owner_token, aliases, client_id)
        status_snapshot["stage"] = "after_samples"
        report.setdefault("session_status", []).append(status_snapshot)
        for observed_session in observed:
            ledger.arm(observed_session)

        report["classification"] = _classification(report)
        for name, _, text in playlists:
            clean = _redact(text, origin, aliases, (owner_token, token), rk, client_id)
            _write_artifact(
                out / name, clean, origin, aliases, (owner_token, token), rk, client_id
            )
    finally:
        report["cleanup"] = _cleanup_sessions(
            origin, token, ledger, aliases, client_id=client_id
        )
        stopped = report["cleanup"]["complete"]
        report_text = json.dumps(report, indent=2, sort_keys=True) + "\n"
        _write_artifact(
            out / "report.json",
            report_text,
            origin,
            aliases,
            (owner_token, token),
            rk,
            client_id,
        )

    print(f"PMS HLS probe: {'auto' if args.auto else 'fixed'}; artifacts={out}")
    print(
        "  decision={} playlist={} variants={} segment={} class={} cleanup={}".format(
            report.get("decision", {}).get("status", "-"),
            report.get("start", {}).get("kind", "-"),
            report.get("start", {}).get("variant_count", 0),
            (report.get("segments") or [{}])[0].get("status", "-"),
            report.get("classification", {}).get("kind", "-"),
            "ok" if stopped else "FAILED",
        )
    )
    if not stopped:
        raise SystemExit("probe completed but cleanup was not confirmed for every candidate session")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--item", default="movie_av1_no_dp_audio", help="overlay item key")
    parser.add_argument("--owner", action="store_true", help="use the owner token instead of the configured test user")
    parser.add_argument("--auto", action="store_true", help="set autoAdjustQuality=1 and peakBitrate=20000")
    parser.add_argument("--bitrate", type=int, default=720, help="initial/target video bitrate in kbps")
    parser.add_argument("--resolution", default="854x480", help="initial/target WxH")
    parser.add_argument(
        "--offset",
        type=int,
        default=0,
        help="non-negative playback offset in seconds (default: 0)",
    )
    parser.add_argument(
        "--bandwidth-sequence",
        default="20000,512",
        help="comma-separated client bandwidth reports in kbps (auto mode only)",
    )
    parser.add_argument(
        "--segments-per-bandwidth",
        type=int,
        default=12,
        help="sequential two-second segments to request for each bandwidth leg",
    )
    parser.add_argument(
        "--fixed-segments",
        type=int,
        default=4,
        help="segments to sample in fixed mode",
    )
    parser.add_argument(
        "--pace",
        type=float,
        default=0.0,
        help="minimum wall seconds per segment request (use 2.0 to simulate realtime playback)",
    )
    parser.add_argument(
        "--out",
        type=Path,
        help="artifact directory outside the repository (default: a private unique temp directory)",
    )
    parser.add_argument(
        "--session-mode",
        choices=("baseline", "legacy", "canonical", "matched", "mismatch", "explicit"),
        default="baseline",
        help=(
            "session-ID wire layout: baseline preserves legacy session + query X-Plex; "
            "other named modes use the X-Plex header"
        ),
    )
    parser.add_argument("--legacy-session-id", help="explicit legacy session= value (never reported raw)")
    parser.add_argument(
        "--canonical-session-id",
        help="explicit transcodeSessionId= value (never reported raw)",
    )
    parser.add_argument(
        "--header-session-id",
        help="explicit X-Plex-Session-Identifier header value (never reported raw)",
    )
    args = parser.parse_args()
    try:
        args.bandwidth_sequence = [int(value) for value in args.bandwidth_sequence.split(",") if value]
    except ValueError:
        parser.error("--bandwidth-sequence must contain only comma-separated integers")
    if not args.bandwidth_sequence:
        parser.error("--bandwidth-sequence must not be empty")
    if (
        args.bitrate <= 0
        or args.segments_per_bandwidth <= 0
        or args.fixed_segments <= 0
        or args.pace < 0
        or args.offset < 0
    ):
        parser.error(
            "bitrate and segment count must be positive; pace and offset must be non-negative"
        )
    if not re.fullmatch(r"[1-9][0-9]*x[1-9][0-9]*", args.resolution):
        parser.error("--resolution must be positive WxH, for example 854x480")
    try:
        _session_plan(
            args.session_mode,
            legacy=args.legacy_session_id,
            canonical=args.canonical_session_id,
            header=args.header_session_id,
            factory=lambda label: f"plxnative-probe-{label}-validation",
        )
    except ValueError as error:
        parser.error(str(error))
    try:
        probe(args)
    except urllib.error.HTTPError as error:
        raise SystemExit(f"PMS HLS probe failed with HTTP {error.code}") from None
    except urllib.error.URLError as error:
        raise SystemExit(f"PMS HLS probe could not reach the configured server ({error.reason})") from None


if __name__ == "__main__":
    main()
