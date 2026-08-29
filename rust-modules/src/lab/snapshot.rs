//! The uploaded document: **one envelope line, then one JSON record per ring line** (JSONL).
//!
//! The consumer is a coding agent, not a person, so the format is machine-readable end to end and
//! the envelope carries the structured state that no log line states in one place. It is built in
//! this one file on purpose — that is what makes "what may appear in an upload" a rule with a
//! single edit point rather than a habit spread over the modules that produce the data.
//!
//! # What may appear, and what may not
//!
//! The envelope is assembled from [`crate::player::Diag`], [`crate::webos`] and
//! [`crate::devcaps`], whose fields are numbers, bools, enums and short platform strings.
//! `ui::stats`'s module doc states the rule those types already live under and the reasoning
//! behind each clause; it applies here unchanged and for a stronger reason, since an upload
//! crosses the public internet rather than a room:
//!
//! * **no URL and no path** — the PMS token rides in the query string of every playback and image
//!   URL, so a URL-shaped field is a guaranteed credential leak rather than a possible one;
//! * **no credential at any length** — omitted, never masked, because a PMS token is short and
//!   shape-indistinguishable from an ordinary opaque id;
//! * **no stable identity** — not the server's friendly name (commonly the owner's first name),
//!   not its `machineIdentifier` (a permanent household fingerprint), not its address;
//! * **no viewing identity** — what is playing appears only as its technical shape.
//!
//! # Defence in depth: [`scrub`]
//!
//! Ring records are ordinary log lines, and the log's own policy — *no call site formats a URL into
//! a line* — has been violated before (`crate::redact_tokens`'s doc carries that history: one
//! `-> {url}` in `route::retranscode`, reached by an ordinary audio-track switch, live for months).
//! So every record passes a second, broader pass on the way out. It is deliberately not the same
//! function as the log's: that one is a hot-path backstop for one parameter name, this one is a
//! wider sweep that runs once per upload on a worker thread and can afford to be thorough.
use crate::diag::ring::Rec;
use crate::diag::scrub::{scrub, Scrubbed};
use serde::Serialize;

// ---- the envelope -----------------------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct Envelope {
    /// always `"envelope"`, so a reader can dispatch on line kind without counting lines
    pub kind: &'static str,
    /// which upload this is within the session, from 1
    pub seq: u32,
    pub session: String,
    /// what triggered it: `"key"` or `"menu"` — the field that settles the colour-button question
    pub reason: String,
    /// the ring clock at the moment of the snapshot (= app uptime in ms)
    pub sent_at_ms: u32,
    pub app: App,
    pub device: Device,
    pub caps: Caps,
    pub player: Player,
    /// route the app was on when the button was pressed
    pub route: &'static str,
    /// ring records evicted since the last snapshot — a non-zero value means the window was too
    /// small and the interesting part may be missing
    pub dropped: u64,
    /// records refused by [`scrub`] outright (see [`Scrubbed::Refuse`])
    pub refused: u64,
    pub records: usize,
}

#[derive(Serialize)]
pub(crate) struct App {
    pub version: &'static str,
    pub id: &'static str,
    pub flavour: &'static str,
    pub features: Vec<&'static str>,
    pub uptime_ms: u32,
}

#[derive(Serialize)]
pub(crate) struct Device {
    pub webos_release: String,
    pub webos_codename: String,
    pub webos_api: String,
    pub webos_name: String,
    pub model: String,
    pub board: String,
    pub hw_revision: String,
}

/// What the SoC's own table says it decodes ([`crate::devcaps`]) — the field that separates "this
/// firmware refuses the stream" from "this set was never going to decode it".
#[derive(Serialize)]
pub(crate) struct Caps {
    pub hevc: bool,
    pub hevc_max_w: u32,
    pub hevc_max_h: u32,
    pub vp9: bool,
    /// the direct-playable audio subset, in `plex::DP_AUDIO_CODECS`'s comma form
    pub audio: String,
}

/// The playback state, out of one consistent [`crate::player::Diag`] read.
///
/// Enums are sent as the STRINGS the diagnostics panel prints rather than as their raw
/// discriminants: the receiving agent should not have to hold this crate's numbering in its head,
/// and the raw number is meaningless without it.
#[derive(Serialize)]
pub(crate) struct Player {
    pub vp_mode: &'static str,
    pub window_id: String,
    pub acb_ok: bool,
    pub stage: u8,
    pub load_completed: bool,
    pub load_failed: bool,
    pub load_video_codec: &'static str,
    pub load_audio_codec: &'static str,
    pub feed_state: &'static str,
    pub feed_is_fault: bool,
    pub video_w: i32,
    pub video_h: i32,
    pub pos_ns: i64,
    pub dur_ns: i64,
    pub frames: i32,
    pub seen_frame: bool,
    pub fed_v: i64,
    pub fed_a: i64,
    pub aq_video: i64,
    pub aq_audio: i64,
    pub cb_count: u32,
    pub cb_err: i32,
    pub cb_err_at: u32,
    pub http_status: i32,
    pub net_rx: i64,
    pub abr_mode: u8,
    pub abr_kbps: i64,
    pub abr_net_kbps: i64,
    pub abr_buffer_ms: i64,
    pub abr_action: u8,
    pub abr_why: u8,
}

#[derive(Serialize)]
struct Line<'a> {
    t_ms: u32,
    m: &'a str,
}

impl From<&crate::player::Diag> for Player {
    fn from(d: &crate::player::Diag) -> Self {
        Player {
            vp_mode: d.vp_mode_str(),
            window_id: d.window_id.clone(),
            acb_ok: d.acb_ok,
            stage: d.stage,
            load_completed: d.load_completed,
            load_failed: d.load_failed,
            load_video_codec: d.load_v_str(),
            load_audio_codec: d.load_a_str(),
            feed_state: d.feed_state_str(),
            feed_is_fault: d.feed_is_fault(),
            video_w: d.video_w,
            video_h: d.video_h,
            pos_ns: d.pos_ns,
            dur_ns: d.dur_ns,
            frames: d.frames,
            seen_frame: d.seen_frame,
            fed_v: d.fed_v,
            fed_a: d.fed_a,
            aq_video: d.aq_video,
            aq_audio: d.aq_audio,
            cb_count: d.cb_count,
            cb_err: d.cb_err,
            cb_err_at: d.cb_err_at,
            http_status: d.http_status,
            net_rx: d.net_rx,
            abr_mode: d.abr_mode,
            abr_kbps: d.abr_kbps,
            abr_net_kbps: d.abr_net_kbps,
            abr_buffer_ms: d.abr_buffer_ms,
            abr_action: d.abr_action,
            abr_why: d.abr_why,
        }
    }
}

/// Which cargo features this binary was built with — the first question asked of any log whose
/// behaviour looks wrong for the code, and one an uploaded document can answer for itself.
fn features() -> Vec<&'static str> {
    let mut v = vec!["lab-diagnostics"];
    if cfg!(feature = "devtools") {
        v.push("devtools");
    }
    if cfg!(feature = "devtriggers") {
        v.push("devtriggers");
    }
    v
}

/// Build the whole body. **Main thread**: `player::diag()` is main-thread by contract, and the
/// ring clone is a memcpy of at most [`crate::diag::ring::MAX_BYTES`].
pub(crate) fn build(seq: u32, reason: &str, session: &str, route: &'static str) -> String {
    let d = crate::player::diag();
    let (recs, dropped) = crate::diag::ring::take();
    body(seq, reason, session, route, &d, recs, dropped)
}

/// The serialisation half, split out so `make check` can grade the document without a television:
/// `Diag::default()` is the never-started session and needs no Starfish symbols.
pub(crate) fn body(
    seq: u32,
    reason: &str,
    session: &str,
    route: &'static str,
    d: &crate::player::Diag,
    recs: Vec<Rec>,
    dropped: u64,
) -> String {
    let now = crate::diag::ring::t_ms();
    let mut lines: Vec<String> = Vec::with_capacity(recs.len() + 1);
    let mut refused = 0u64;
    let mut kept: Vec<Line> = Vec::with_capacity(recs.len());
    let scrubbed: Vec<(u32, String)> = recs
        .iter()
        .filter_map(|r| match scrub(&r.msg) {
            Scrubbed::Keep(s) => Some((r.t_ms, s)),
            Scrubbed::Refuse => {
                refused += 1;
                None
            }
        })
        .collect();
    for (t_ms, m) in &scrubbed {
        kept.push(Line { t_ms: *t_ms, m });
    }
    let env = Envelope {
        kind: "envelope",
        seq,
        session: session.to_string(),
        reason: reason.to_string(),
        sent_at_ms: now,
        app: App {
            version: env!("CARGO_PKG_VERSION"),
            id: crate::paths::app_id(),
            flavour: crate::paths::flavour().unwrap_or("stable"),
            features: features(),
            uptime_ms: now,
        },
        device: device(),
        caps: caps(),
        player: Player::from(d),
        route,
        dropped,
        refused,
        records: kept.len(),
    };
    // `to_string` on a struct of numbers and short strings cannot fail; if it somehow did, an
    // empty envelope line would make the whole upload unreadable, so the fallback still says what
    // happened in the same shape.
    lines.push(
        serde_json::to_string(&env)
            .unwrap_or_else(|_| r#"{"kind":"envelope","error":"envelope did not serialise"}"#.into()),
    );
    for l in &kept {
        if let Ok(s) = serde_json::to_string(l) {
            lines.push(s);
        }
    }
    lines.join("\n")
}

fn device() -> Device {
    let i = crate::webos::info();
    let d = crate::webos::device();
    Device {
        webos_release: i.release.clone(),
        webos_codename: i.codename.clone(),
        webos_api: i.api.clone(),
        webos_name: i.name.clone(),
        model: d.model.clone(),
        board: d.board.clone(),
        hw_revision: d.hw_revision.clone(),
    }
}

fn caps() -> Caps {
    let c = crate::devcaps::caps();
    Caps {
        hevc: c.hevc,
        hevc_max_w: c.hevc_max.0,
        hevc_max_h: c.hevc_max.1,
        vp9: c.vp9,
        audio: c.audio.clone(),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// A credential shape none of the four rewrites anticipated is DROPPED, not shipped — and the
    /// drop is counted in the envelope so the document says a line is missing.
    ///
    /// Lives here rather than beside `scrub` because the assertion that matters is about the
    /// DOCUMENT — `refused` and `records` in the envelope — not about the predicate. `diag::scrub`
    /// carries the twin proving the LOCAL exit keeps the same line.
    ///
    /// **The REMOTE exit only.** Gated with its function: the local exit is contractually forbidden
    /// from dropping, and the test directly below this one is the assertion that it does not.
    #[test]
    fn an_unanticipated_credential_shape_is_refused_outright() {
        assert!(matches!(scrub("auth ok, Bearer eyJhbGciOi.abc"), Scrubbed::Refuse));
        let recs = vec![
            Rec { t_ms: 1, msg: "keep me".into() },
            Rec { t_ms: 2, msg: "Bearer eyJ.abc".into() },
        ];
        let doc = body(1, "key", "s", "home", &crate::player::Diag::default(), recs, 0);
        assert!(!doc.contains("eyJ.abc"), "{doc}");
        let env: serde_json::Value = serde_json::from_str(doc.split('\n').next().unwrap()).unwrap();
        assert_eq!(env["refused"], 1);
        assert_eq!(env["records"], 1);
    }

    /// The exact shape the log's own backstop was written for still cannot survive this one.
    #[test]

    /// The document's SHAPE: line 1 is the envelope, one line per kept record, and the counts in
    /// the envelope describe the lines that follow it.
    #[test]
    fn the_document_is_one_envelope_line_then_one_line_per_record() {
        let recs = vec![
            Rec { t_ms: 10, msg: "first".into() },
            Rec { t_ms: 20, msg: "second".into() },
        ];
        let doc = body(3, "key", "a1b2c3d4", "player", &crate::player::Diag::default(), recs, 7);
        let lines: Vec<&str> = doc.split('\n').collect();
        assert_eq!(lines.len(), 3);
        let env: serde_json::Value = serde_json::from_str(lines[0]).expect("envelope is JSON");
        assert_eq!(env["kind"], "envelope");
        assert_eq!(env["seq"], 3);
        assert_eq!(env["reason"], "key");
        assert_eq!(env["dropped"], 7);
        assert_eq!(env["records"], 2);
        assert_eq!(env["route"], "player");
        assert_eq!(env["app"]["version"], env!("CARGO_PKG_VERSION"));
        // the never-started session reads honestly rather than as a healthy one
        assert_eq!(env["player"]["vp_mode"], "NONE — no video path");
        assert_eq!(env["player"]["feed_state"], "— nothing fed yet");
        let rec: serde_json::Value = serde_json::from_str(lines[2]).expect("record is JSON");
        assert_eq!(rec["t_ms"], 20);
        assert_eq!(rec["m"], "second");
    }

    /// Every line is independently parseable — that is the whole point of JSONL, and a record
    /// containing a newline, a quote or a control character must not break the frame.
    #[test]
    fn a_record_with_hostile_characters_stays_one_json_line() {
        let recs = vec![Rec { t_ms: 1, msg: "a\nb\t\"c\"\\d".into() }];
        let doc = body(1, "menu", "s", "home", &crate::player::Diag::default(), recs, 0);
        let lines: Vec<&str> = doc.split('\n').collect();
        assert_eq!(lines.len(), 2, "the embedded newline did not split the record");
        let rec: serde_json::Value = serde_json::from_str(lines[1]).expect("still JSON");
        assert_eq!(rec["m"], "a\nb\t\"c\"\\d");
    }
}
