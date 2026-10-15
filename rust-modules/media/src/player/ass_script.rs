//! A sidecar script (an authored `.ass`, or a converted SubRip/WebVTT file) fed to the renderer a
//! window at a time, the way an embedded track is (`ass_source`): libass is handed the script's
//! header plus the dialogue lines around the playhead, never the whole script, so the native
//! library's own event guard (20 000 per track) is never approached however long the script is, and
//! what libass holds stays one window of lines, within [`WINDOW_BYTES`] as well.
//!
//! The script is still read by libass itself (`plx_ass_load` with `script = 1`), so its grammar,
//! Format orders and timestamp rules are libass's; this module only decides WHICH `Dialogue:` lines
//! go in with the header. The lines are kept in file order, which libass uses to stack overlapping
//! events of one layer.
use super::ass_timing::{effect_animated, libass_clock_ms, text_animated, Motion, Span};
use std::collections::VecDeque;
use std::sync::Arc;

/// Dialogue lines in one window: the embedded tracks' forward window (`ass_source`), far under the
/// native guard. A script with no more events than this (and no more bytes than [`WINDOW_BYTES`]) is loaded whole.
pub const WINDOW_EVENTS: usize = 8192;
/// Bytes in one window, header included: the bound a script loaded whole answers to, so a
/// window is never bigger than the largest script the renderer takes in one piece. A script past
/// it is fed in windows whatever its line count.
pub const WINDOW_BYTES: usize = 8 * 1024 * 1024;
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
    /// Its picture depends on the clock (an animation tag or an Effect), so a frame showing it
    /// is valid for one time only.
    animated: bool,
}

pub struct ScriptIndex {
    raw: Arc<[u8]>,
    /// Everything that is not a `Dialogue:` line, in file order.
    header: Vec<u8>,
    lines: Vec<Line>,
    /// `lines` by start time (ties in file order).
    by_start: Vec<u32>,
    /// The longest `end - start` of any line: no line starting earlier than `now - max_span` is
    /// alive at `now`, which bounds [`ScriptIndex::alive_at`].
    max_span_ms: i64,
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
    /// Whether a script of `len` bytes can be indexed at all (line offsets are 32-bit): the only
    /// size past which a script is refused rather than windowed.
    pub fn indexable(len: usize) -> bool {
        u32::try_from(len).is_ok()
    }

    pub fn parse(raw: &Arc<[u8]>) -> Option<Self> {
        if !Self::indexable(raw.len()) {
            return None;
        }
        let (mut start_at, mut end_at, mut fields) = (1usize, 2usize, 10usize);
        let mut effect_at: Option<usize> = Some(8);
        let (mut header, mut lines) = (Vec::new(), Vec::new());
        let mut off = 0usize;
        for line in raw.split_inclusive(|&b| b == b'\n') {
            let text = String::from_utf8_lossy(line);
            let trimmed = text.trim_start_matches('\u{feff}').trim_start();
            if trimmed.get(..9).is_some_and(|p| p.eq_ignore_ascii_case("Dialogue:")) {
                let body = &trimmed[9..];
                let (start_ms, end_ms) = times(body, start_at, end_at, fields).unwrap_or(ALWAYS);
                let effect = effect_at.and_then(|at| body.splitn(fields.max(1), ',').nth(at));
                let animated = effect.is_some_and(|e| effect_animated(e.as_bytes())) || text_animated(body.as_bytes());
                lines.push(Line { start_ms, end_ms, off: off as u32, len: line.len() as u32, animated });
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
                        effect_at = names.iter().position(|f| f == "effect");
                    }
                }
                header.extend_from_slice(line);
            }
            off += line.len();
        }
        let mut by_start: Vec<u32> = (0..lines.len() as u32).collect();
        by_start.sort_by_key(|&i| lines[i as usize].start_ms);
        let max_span_ms = lines.iter().map(|l| l.end_ms.saturating_sub(l.start_ms)).max().unwrap_or(0);
        Some(Self { raw: raw.clone(), header, lines, by_start, max_span_ms })
    }

    /// Every line's timing, kept whether or not the script is windowed: what a rendered frame is
    /// valid for (`ass_timing::validity`) depends on all of them, not only the held window.
    pub fn spans(&self) -> impl Iterator<Item = Span<'_>> {
        self.lines.iter().map(|l| Span {
            start_ms: l.start_ms,
            end_ms: l.end_ms,
            motion: Motion::Known(l.animated),
        })
    }

    /// More lines, or more bytes, than one window holds: the script must be fed in windows.
    pub fn windowed(&self) -> bool {
        self.lines.len() > WINDOW_EVENTS || self.raw.len() > WINDOW_BYTES
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// The `Dialogue:` lines alive at `now_ms` (`start <= now < end`), as they stand in the script.
    pub fn alive_at(&self, now_ms: i64) -> impl Iterator<Item = &[u8]> + '_ {
        let started = self.by_start.partition_point(|&i| self.lines[i as usize].start_ms <= now_ms);
        self.by_start[..started]
            .iter()
            .rev()
            .map(|&i| &self.lines[i as usize])
            .take_while(move |line| line.start_ms.saturating_add(self.max_span_ms) > now_ms)
            .filter(move |line| now_ms < line.end_ms)
            .map(|line| &self.raw[line.off as usize..(line.off + line.len) as usize])
    }

    /// The window for a playhead at `now_ms`: the header and, in file order, the first
    /// [`WINDOW_EVENTS`] lines (by start, and within [`WINDOW_BYTES`] with the header, but never
    /// fewer than one) that had not ended `BACK_MS` before it. Every line alive at a playhead the
    /// window answers for is in it: lines are taken by start, so a line that starts before the cut
    /// is taken before any that starts after it, and when the bound is reached the lines already
    /// over before the playhead are let go of first, and only to make room for a line that starts by
    /// the playhead (a later line that does not fit is cut off instead; a count that is reached
    /// cuts too). The window then answers from the end of the last one it let go, not from
    /// `BACK_MS` back. Every window is within [`WINDOW_BYTES`] except a single line larger than the
    /// bound, which the renderer refuses.
    pub fn window(&self, now_ms: i64) -> Window {
        let mut lo_ms = now_ms.saturating_sub(BACK_MS);
        let mut taken: VecDeque<u32> = VecDeque::new();
        let mut bytes = self.header.len();
        let mut hi_ms = i64::MAX;
        for &i in &self.by_start {
            let line = &self.lines[i as usize];
            if line.end_ms <= lo_ms {
                continue;
            }
            let over_bytes = |taken: &VecDeque<u32>, bytes: usize| {
                !taken.is_empty() && bytes.saturating_add(line.len as usize) > WINDOW_BYTES
            };
            // Look-back is let go of only to make room for a line that starts by the playhead; a
            // later line that does not fit is cut off (`hi_ms`) and the window keeps its `BACK_MS`.
            while line.start_ms <= now_ms && over_bytes(&taken, bytes) {
                match taken.front().map(|&f| &self.lines[f as usize]) {
                    Some(old) if old.end_ms <= now_ms => {
                        bytes -= old.len as usize;
                        lo_ms = lo_ms.max(old.end_ms);
                        taken.pop_front();
                    }
                    _ => break,
                }
            }
            if taken.len() == WINDOW_EVENTS || over_bytes(&taken, bytes) {
                hi_ms = line.start_ms;
                break;
            }
            if line.end_ms <= lo_ms {
                continue; // let go of with the lines before it
            }
            bytes += line.len as usize;
            taken.push_back(i);
        }
        // More than a window of lines starting together cannot all be held; the window still has to
        // answer for the playhead it was cut for, or it would be cut again every frame.
        let hi_ms = hi_ms.max(now_ms.saturating_add(1));
        let mut taken: Vec<u32> = taken.into();
        taken.sort_unstable();
        let mut text = Vec::with_capacity(bytes);
        text.extend_from_slice(&self.header);
        for i in taken {
            let line = &self.lines[i as usize];
            text.extend_from_slice(&self.raw[line.off as usize..(line.off + line.len) as usize]);
        }
        Window { lo_ms, hi_ms, text }
    }
}

/// The start and end of a `Dialogue:` body whose Format names `fields` columns, in milliseconds as
/// libass reads them (`libass_clock_ms`: a malformed stamp is 0, as it is for libass).
fn times(body: &str, start_at: usize, end_at: usize, fields: usize) -> Option<(i64, i64)> {
    let cols: Vec<&str> = body.splitn(fields.max(1), ',').collect();
    Some((libass_clock_ms(cols.get(start_at)?), libass_clock_ms(cols.get(end_at)?)))
}

#[cfg(test)]
mod tests;
