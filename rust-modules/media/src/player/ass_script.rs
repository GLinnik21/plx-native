//! A sidecar `.ass` script fed to the renderer a window at a time, the way an embedded track is
//! (`ass_source`): libass is handed the script's header plus the dialogue lines around the playhead,
//! never the whole script, so the native library's own event guard (20 000 per track) is never
//! approached however long the script is, and what libass holds stays one window of lines.
//!
//! The script is still read by libass itself (`plx_ass_load` with `script = 1`), so its grammar,
//! Format orders and timestamp rules are libass's; this module only decides WHICH `Dialogue:` lines
//! go in with the header. The lines are kept in file order, which libass uses to stack overlapping
//! events of one layer.
use std::sync::Arc;

/// Dialogue lines in one window: the embedded tracks' forward window (`ass_source`), far under the
/// native guard. A script with no more events than this is loaded whole, as it always was.
pub const WINDOW_EVENTS: usize = 8192;
/// How far behind the playhead a window reaches, so a short step back stays inside it.
const BACK_MS: i64 = 10_000;
/// "Alive for ever": a line whose times cannot be read is kept in every window (libass decides what
/// it is worth) rather than dropped here.
const ALWAYS: (i64, i64) = (0, i64::MAX);

struct Line {
    start_ms: i64,
    end_ms: i64,
    off: u32,
    len: u32,
}

pub struct ScriptIndex {
    raw: Arc<[u8]>,
    /// Everything that is not a `Dialogue:` line, in file order.
    header: Vec<u8>,
    lines: Vec<Line>,
    /// `lines` by start time (ties in file order).
    by_start: Vec<u32>,
}

/// One window's script text and the playheads it answers for: `lo_ms <= now < hi_ms`.
pub struct Window {
    pub lo_ms: i64,
    pub hi_ms: i64,
    pub text: Vec<u8>,
}

impl Window {
    pub fn covers(&self, now_ms: i64) -> bool {
        (self.lo_ms..self.hi_ms).contains(&now_ms)
    }
}

impl ScriptIndex {
    pub fn parse(raw: &Arc<[u8]>) -> Option<Self> {
        if u32::try_from(raw.len()).is_err() {
            return None;
        }
        let (mut start_at, mut end_at, mut fields) = (1usize, 2usize, 10usize);
        let (mut header, mut lines) = (Vec::new(), Vec::new());
        let mut off = 0usize;
        for line in raw.split_inclusive(|&b| b == b'\n') {
            let text = String::from_utf8_lossy(line);
            let trimmed = text.trim_start_matches('\u{feff}').trim_start();
            if trimmed.get(..9).is_some_and(|p| p.eq_ignore_ascii_case("Dialogue:")) {
                let (start_ms, end_ms) = times(&trimmed[9..], start_at, end_at, fields).unwrap_or(ALWAYS);
                lines.push(Line { start_ms, end_ms, off: off as u32, len: line.len() as u32 });
            } else {
                if trimmed.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("Format:")) {
                    let names: Vec<String> =
                        trimmed[7..].split(',').map(|f| f.trim().to_ascii_lowercase()).collect();
                    // the styles section has a Format line too: only the events' one names a
                    // Start and an End
                    if let (Some(s), Some(e)) = (
                        names.iter().position(|f| f == "start"),
                        names.iter().position(|f| f == "end"),
                    ) {
                        (start_at, end_at, fields) = (s, e, names.len());
                    }
                }
                header.extend_from_slice(line);
            }
            off += line.len();
        }
        let mut by_start: Vec<u32> = (0..lines.len() as u32).collect();
        by_start.sort_by_key(|&i| lines[i as usize].start_ms);
        Some(Self { raw: raw.clone(), header, lines, by_start })
    }

    /// More lines than one window holds: the script must be fed in windows.
    pub fn windowed(&self) -> bool {
        self.lines.len() > WINDOW_EVENTS
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// The window for a playhead at `now_ms`: the header and, in file order, the first
    /// [`WINDOW_EVENTS`] lines (by start) that had not ended `BACK_MS` before it. Every line alive
    /// at a playhead the window answers for is in it: lines are taken by start, so a line that
    /// starts before the cut is taken before any that starts after it.
    pub fn window(&self, now_ms: i64) -> Window {
        let lo_ms = now_ms.saturating_sub(BACK_MS);
        let mut taken: Vec<u32> = Vec::new();
        let mut hi_ms = i64::MAX;
        for &i in &self.by_start {
            let line = &self.lines[i as usize];
            if line.end_ms <= lo_ms {
                continue;
            }
            if taken.len() == WINDOW_EVENTS {
                hi_ms = line.start_ms;
                break;
            }
            taken.push(i);
        }
        // More than a window of lines starting together cannot all be held; the window still has to
        // answer for the playhead it was cut for, or it would be cut again every frame.
        let hi_ms = hi_ms.max(now_ms.saturating_add(1));
        taken.sort_unstable();
        let mut text = Vec::with_capacity(self.header.len() + taken.len() * 96);
        text.extend_from_slice(&self.header);
        for i in taken {
            let line = &self.lines[i as usize];
            text.extend_from_slice(&self.raw[line.off as usize..(line.off + line.len) as usize]);
        }
        Window { lo_ms, hi_ms, text }
    }
}

/// The start and end of a `Dialogue:` body whose Format names `fields` columns, in milliseconds.
fn times(body: &str, start_at: usize, end_at: usize, fields: usize) -> Option<(i64, i64)> {
    let cols: Vec<&str> = body.splitn(fields.max(1), ',').collect();
    Some((clock_ms(cols.get(start_at)?)?, clock_ms(cols.get(end_at)?)?))
}

/// `H:MM:SS.cc` (libass accepts fewer fraction digits and none).
fn clock_ms(s: &str) -> Option<i64> {
    let mut parts = s.trim().split(':');
    let h: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let sec = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let (whole, frac) = sec.split_once('.').unwrap_or((sec, ""));
    let whole: i64 = whole.parse().ok()?;
    let frac_ms: i64 = if frac.is_empty() {
        0
    } else {
        let digits: String = frac.chars().take(3).collect();
        digits.parse::<i64>().ok()? * 10i64.pow(3 - digits.len() as u32)
    };
    Some(((h * 60 + m) * 60 + whole) * 1000 + frac_ms)
}

#[cfg(test)]
mod tests;
