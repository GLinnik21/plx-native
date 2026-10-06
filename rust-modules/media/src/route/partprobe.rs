//! **Dev probe `plxnative-partprobe`** — while a server-converted (remux / transcode) stream plays,
//! does PMS let this same client read byte ranges of the film's ORIGINAL Part file over a second
//! connection? A client-side subtitle renderer over a remux would have to demux that file a second
//! time, and its whole feasibility is this one server behaviour, which only the real device and a
//! real PMS can answer.
//!
//! Armed by the trigger file (`devtrig::flag`, so compiled out of every shipping build: this module
//! is `#[cfg(feature = "devtriggers")]` at its declaration). Fired once per playback start from
//! `apply_plan`, only when the settled route is a server transcode session. The work is a
//! short-lived background thread of five `Range` GETs through the app's own client
//! (`Client::part_range_probe`: token, resolve pin, TLS all as for a real request); it never
//! touches the player or feed thread, and writes nothing to the server.
//!
//! One event-log line per request, `partprobe: variant=… http=… bytes=… ms=…`. The URL, host,
//! token and session identifier values are never logged; [`Variant::session`] is the only place
//! that decides which identifier a request carries and the log line is built from labels and
//! numbers alone.

use super::plan::RouteFamily;
use std::time::{Duration, Instant};

/// First and last byte of every range but the mid-file one: the first KiB.
const HEAD_LEN: i64 = 1024;
/// The 10 s gap before the last variant.
const LATE_DELAY: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Variant {
    /// The live transcode session's own identifier.
    Same,
    /// A fresh identifier PMS has never seen.
    Fresh,
    /// No `X-Plex-Session-Identifier` at all.
    None,
    /// [`Variant::Fresh`], reading the middle of the file.
    FreshMid,
    /// [`Variant::Fresh`] again, [`LATE_DELAY`] later.
    FreshLate,
}

/// The order they are issued in.
const ORDER: [Variant; 5] = [
    Variant::Same,
    Variant::Fresh,
    Variant::None,
    Variant::FreshMid,
    Variant::FreshLate,
];

impl Variant {
    fn label(self) -> &'static str {
        match self {
            Variant::Same => "same",
            Variant::Fresh => "fresh",
            Variant::None => "none",
            Variant::FreshMid => "fresh-mid",
            Variant::FreshLate => "fresh-late",
        }
    }

    /// The session identifier this variant's request carries, from the live one and the fresh one.
    fn session<'a>(self, live: &'a str, fresh: &'a str) -> Option<&'a str> {
        match self {
            Variant::Same => Some(live),
            Variant::Fresh | Variant::FreshMid | Variant::FreshLate => Some(fresh),
            Variant::None => None,
        }
    }

    /// The inclusive byte range, or `None` when the variant cannot run (the mid-file read of a Part
    /// whose size is unknown).
    fn range(self, size: Option<i64>) -> Option<(i64, i64)> {
        match self {
            Variant::FreshMid => mid_range(size?),
            _ => Some((0, HEAD_LEN - 1)),
        }
    }
}

/// `size/2 ..= size/2 + 1023`, clamped to the file. `None` for a size with no middle to read.
fn mid_range(size: i64) -> Option<(i64, i64)> {
    if size <= 0 {
        return None;
    }
    let first = size / 2;
    Some((first, (first + HEAD_LEN - 1).min(size - 1)))
}

/// What one request came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// An HTTP answer: status and body bytes.
    Http { status: i32, bytes: usize },
    /// A transfer error: libcurl's code when it gave one.
    Failed(Option<i32>),
}

fn armed_line(family: RouteFamily, size: Option<i64>) -> String {
    let family = match family {
        RouteFamily::Direct => "direct",
        RouteFamily::Remux => "remux",
        RouteFamily::Other => "transcode",
    };
    let size = size.map_or_else(|| "?".to_owned(), |n| n.to_string());
    format!("partprobe: armed route={family} part_size={size}")
}

fn result_line(variant: Variant, outcome: Outcome, ms: u128) -> String {
    let (http, bytes) = match outcome {
        Outcome::Http { status, bytes } => (status.to_string(), bytes.to_string()),
        Outcome::Failed(rc) => (rc.map_or_else(|| "rc=?".to_owned(), |rc| format!("rc={rc}")), "0".to_owned()),
    };
    format!("partprobe: variant={} http={http} bytes={bytes} ms={ms}", variant.label())
}

/// Fire the probe for a playback that has just settled on `family`. Everything is passed by value:
/// the thread owns it, and reads the main-thread session not at all.
///
/// `rk` and `part_key` name the film; `live_session` is the transcode session's identifier.
/// A no-op unless the trigger is armed and the route is a server conversion.
pub(super) fn arm(
    family: RouteFamily,
    sid: plx_plex::plex::ServerId,
    rk: String,
    part_key: String,
    live_session: String,
) {
    if family == RouteFamily::Direct
        || part_key.is_empty()
        || live_session.is_empty()
        || !plx_base::devtrig::flag("partprobe")
    {
        return;
    }
    plx_base::task::spawn_small("partprobe", move || run(family, sid, &rk, &part_key, &live_session));
}

fn run(family: RouteFamily, sid: plx_plex::plex::ServerId, rk: &str, part_key: &str, live: &str) {
    let Some(client) = plx_plex::plex::client_for(sid) else { return };
    let size = client
        .metadata(rk)
        .and_then(|m| m.first_part().map(|p| p.size))
        .filter(|n| *n > 0);
    plx_base::eventlog::log(&armed_line(family, size));
    let fresh = format!("{live}-partprobe");
    for variant in ORDER {
        let Some((first, last)) = variant.range(size) else { continue };
        if variant == Variant::FreshLate {
            std::thread::sleep(LATE_DELAY);
        }
        let started = Instant::now();
        let outcome = match client.part_range_probe(part_key, variant.session(live, &fresh), first, last) {
            Ok((status, bytes)) => Outcome::Http { status, bytes },
            Err(rc) => Outcome::Failed(rc),
        };
        plx_base::eventlog::log(&result_line(variant, outcome, started.elapsed().as_millis()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partprobe_variants_carry_the_session_the_spec_names() {
        let (live, fresh) = ("LIVE-123", "FRESH-456");
        assert_eq!(Variant::Same.session(live, fresh), Some(live));
        assert_eq!(Variant::Fresh.session(live, fresh), Some(fresh));
        assert_eq!(Variant::None.session(live, fresh), None);
        assert_eq!(Variant::FreshMid.session(live, fresh), Some(fresh));
        assert_eq!(Variant::FreshLate.session(live, fresh), Some(fresh));
    }

    #[test]
    fn partprobe_ranges_head_mid_and_unknown_size() {
        assert_eq!(Variant::Same.range(None), Some((0, 1023)));
        assert_eq!(Variant::None.range(Some(10)), Some((0, 1023)));
        assert_eq!(Variant::FreshLate.range(None), Some((0, 1023)));
        assert_eq!(Variant::FreshMid.range(None), None, "no size, no mid-file read");
        assert_eq!(Variant::FreshMid.range(Some(0)), None);
        assert_eq!(Variant::FreshMid.range(Some(10_000_000)), Some((5_000_000, 5_001_023)));
        assert_eq!(Variant::FreshMid.range(Some(100_001)), Some((50_000, 51_023)));
        assert_eq!(mid_range(1_000), Some((500, 999)), "clamped to the last byte of a small file");
        assert_eq!(mid_range(1), Some((0, 0)));
    }

    #[test]
    fn partprobe_order_is_the_five_variants_once() {
        let labels: Vec<_> = ORDER.iter().map(|v| v.label()).collect();
        assert_eq!(labels, ["same", "fresh", "none", "fresh-mid", "fresh-late"]);
    }

    #[test]
    fn partprobe_lines_carry_no_url_token_or_session() {
        let armed = armed_line(RouteFamily::Remux, Some(34_659_545_780));
        assert_eq!(armed, "partprobe: armed route=remux part_size=34659545780");
        assert_eq!(armed_line(RouteFamily::Other, None), "partprobe: armed route=transcode part_size=?");
        let ok = result_line(Variant::FreshMid, Outcome::Http { status: 206, bytes: 1024 }, 87);
        assert_eq!(ok, "partprobe: variant=fresh-mid http=206 bytes=1024 ms=87");
        let bad = result_line(Variant::None, Outcome::Failed(Some(28)), 8001);
        assert_eq!(bad, "partprobe: variant=none http=rc=28 bytes=0 ms=8001");
        assert_eq!(
            result_line(Variant::Same, Outcome::Failed(None), 5),
            "partprobe: variant=same http=rc=? bytes=0 ms=5"
        );
        for line in [armed, ok, bad] {
            for banned in ["http://", "https://", "X-Plex", "token", "Token", "/library", "LIVE", "FRESH"] {
                assert!(!line.contains(banned), "{line:?} names {banned:?}");
            }
        }
    }
}
