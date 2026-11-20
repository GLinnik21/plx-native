//! The on-screen diagnostics panel's TEXT MODEL: plain data, no SDL, no GL, testable anywhere.
//!
//! A retail Samsung TV cannot be read back (no sdb shell, no dlog), so the tester installs the package
//! and sends a PHOTO of the screen. This module is what the photo shows. Writers are the main thread
//! (Ecore key and focus callbacks, `plx_tizen_create`, `pause`/`resume`) and the render thread (GL
//! strings, the H5 result); each calls one setter here. The render thread reads [`Panel::snapshot`]
//! and re-rasterises the panel texture ONLY when [`Panel::version`] moved, which is why no setter
//! bumps the version when the content is unchanged, and why nothing time-varying (a clock, a frame
//! count) may ever be put on the panel.
//!
//! The sections come out top to bottom as: title, instruction, key events, keys seen, lifecycle, step
//! results, device facts. `panel_render.rs` turns them into pixels; `Tone` only names a colour role.
//!
//! The "keys seen" list exists because a tester presses about 25 buttons and photographs ONCE, while
//! the event ring holds only the last 8 events (4 presses: every press is a DOWN and an UP).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

/// Latin, a check mark and Cyrillic: the H5 probe's UTF-8 sample, shown on the panel as proof that
/// UTF-8 text reaches the screen through SDL_ttf and a GLES2 texture.
pub const SAMPLE_TEXT: &str = "PlxNative H5 \u{2713} \u{041F}\u{0440}\u{0438}\u{0432}\u{0435}\u{0442}";

/// The header: the crate's version and the git short sha `build.rs` embedded (`unknown` when the
/// build was not made from a git checkout; `+dirty` when tracked files under tizen/hello differed).
pub const HEADER: &str = concat!("PlxNative Samsung probe ", env!("CARGO_PKG_VERSION"), " \u{2014} ", env!("PLX_GIT_SHA"));

/// The instruction line. The two media-key glyphs and the minus sign may be missing from the font; the
/// renderer substitutes ASCII for exactly the ones it lacks (`panel_render::displayable`).
pub const HINT: &str = "Press: arrows, OK, Back, Play/Pause, Stop, \u{23EA} \u{23E9}, red/green/yellow/blue, 0-9, CH+/CH\u{2212}, then photograph this screen";

/// Key events kept (oldest dropped). Eight is what the brief asks for; four presses.
pub const EVENT_RING: usize = 8;
/// Events per printed row.
const EVENTS_PER_ROW: usize = 4;
/// Distinct (name, code) pairs remembered, and the characters of them shown (the rest is a count).
const SEEN_MAX: usize = 80;
const SEEN_SHOWN_CHARS: usize = 420;
/// Lifecycle entries kept.
pub const LIFECYCLE_RING: usize = 4;
/// Longest value taken from a fact, so one runaway string cannot push everything else off screen.
const VALUE_MAX: usize = 110;
/// Longest line of a multi-line step result (steps 8 and 9). Those lines are EVIDENCE (a driver's error, the
/// list of missing GL extensions): they are wrapped by the renderer, not cut at a value's 110 characters,
/// so the tester's photo carries the whole message. The cap only stops a runaway string; the log has the
/// text in full whatever this is.
pub const STEP_LINE_MAX: usize = 400;

/// A colour role. The renderer owns the actual colours.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Title,
    Hint,
    Fact,
    Ok,
    Warn,
    Key,
}

/// One block of panel text. `text` may hold `\n`; the renderer wraps long lines.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Section {
    pub tone: Tone,
    pub text: String,
}

/// The steps a tester's photo reports, in display order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Step {
    /// The .so loaded and `plx_tizen_create` ran.
    So,
    /// The `dlopen`s of the Tizen libraries.
    Libs,
    /// The Ecore_Wl2 window and key handlers.
    Window,
    /// EGL: version, vendor, context and window surface.
    Egl,
    /// GLES: vendor, renderer, version (render thread).
    Gl,
    /// The H5 probe's result: static SDL2 + SDL2_ttf.
    Sdl,
    /// The UTF-8 sample line.
    Sample,
    /// H7: the application's own GLES2 renderer (`plx_gfx`) on this set's GL. Two lines.
    Gfx,
    /// H8: libcurl loaded by the application's name list, and a GET of the PC's mock server. Two lines.
    Net,
}

/// What the library itself learns about the device (`sysinfo.rs`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeFacts {
    /// `std::env::consts::ARCH`: what this .so was compiled for.
    pub arch: String,
    pub ptr_bits: usize,
    /// `uname` machine field.
    pub machine: String,
    pub glibc: String,
    /// `/proc/self/exe`.
    pub exe: String,
}

struct State {
    host: Vec<(String, String)>,
    native: Option<NativeFacts>,
    steps: Vec<(Step, bool, String)>,
    events: VecDeque<String>,
    /// `name=code`, first-seen order, distinct.
    seen: Vec<String>,
    lifecycle: VecDeque<String>,
}

/// The panel model. One process-global instance ([`PANEL`]); tests build their own.
pub struct Panel {
    state: Mutex<State>,
    version: AtomicU64,
}

/// The process-global panel every writer in the library uses.
pub static PANEL: Panel = Panel::new();

impl Panel {
    pub const fn new() -> Panel {
        Panel {
            state: Mutex::new(State {
                host: Vec::new(),
                native: None,
                steps: Vec::new(),
                events: VecDeque::new(),
                seen: Vec::new(),
                lifecycle: VecDeque::new(),
            }),
            // Version 0 is "nothing rendered yet": the renderer starts at 0 and the first snapshot is
            // always newer, even when nothing was set.
            version: AtomicU64::new(1),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The content version. It only ever grows, and only when the content changed.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Run `f` on the state; `f` returns whether it changed anything, and the version moves if so.
    fn edit(&self, f: impl FnOnce(&mut State) -> bool) {
        let mut st = self.lock();
        if f(&mut st) {
            // Under the lock, so `snapshot` always pairs a version with the content that made it.
            self.version.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Replace (by key) the host's `key=value` facts.
    pub fn set_host_facts(&self, facts: Vec<(String, String)>) {
        self.edit(|st| {
            let mut changed = false;
            for (k, v) in facts {
                match st.host.iter_mut().find(|(ek, _)| *ek == k) {
                    Some((_, ev)) if *ev == v => {}
                    Some((_, ev)) => {
                        *ev = v;
                        changed = true;
                    }
                    None => {
                        st.host.push((k, v));
                        changed = true;
                    }
                }
            }
            changed
        });
    }

    pub fn set_native(&self, facts: NativeFacts) {
        let facts = NativeFacts {
            arch: clean(&facts.arch),
            ptr_bits: facts.ptr_bits,
            machine: clean(&facts.machine),
            glibc: clean(&facts.glibc),
            exe: clean(&facts.exe),
        };
        self.edit(|st| {
            let changed = st.native.as_ref() != Some(&facts);
            st.native = Some(facts);
            changed
        });
    }

    /// Record (or replace) one step's result that takes more than one line (steps 8 and 9: the verdict
    /// first, then the evidence). Each line is [`clean`]ed on its own (to [`STEP_LINE_MAX`], not a value's
    /// [`VALUE_MAX`]: the renderer wraps a long line) and the lines are joined with `\n`, which `compose`
    /// and the renderer keep as line breaks; blank lines are dropped.
    pub fn step_lines(&self, step: Step, ok: bool, lines: &[String]) {
        let text =
            lines.iter().map(|l| clean_to(l, STEP_LINE_MAX)).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
        self.set_step(step, ok, text);
    }

    /// Record (or replace) one step's result. `ok` picks the colour role.
    pub fn step(&self, step: Step, ok: bool, text: &str) {
        self.set_step(step, ok, clean(text));
    }

    fn set_step(&self, step: Step, ok: bool, text: String) {
        self.edit(|st| {
            match st.steps.iter_mut().find(|(s, _, _)| *s == step) {
                Some((_, eok, etext)) => {
                    if *eok == ok && *etext == text {
                        return false;
                    }
                    *eok = ok;
                    *etext = text;
                }
                None => {
                    st.steps.push((step, ok, text));
                    st.steps.sort_by_key(|(s, _, _)| *s);
                }
            }
            true
        });
    }

    /// One key event. `name` is the Ecore keyname (or the key string when that is missing).
    pub fn key_event(&self, down: bool, name: &str, code: u32) {
        let name = clean_token(name);
        let pair = format!("{name}={code}");
        let event = format!("{} {name} {code}", if down { "DOWN" } else { "UP" });
        self.edit(|st| {
            if st.events.len() >= EVENT_RING {
                st.events.pop_front();
            }
            st.events.push_back(event);
            if st.seen.len() < SEEN_MAX && !st.seen.contains(&pair) {
                st.seen.push(pair);
            }
            true
        });
    }

    /// A lifecycle line (pause, resume, focus, locale), stamped with seconds since the app started.
    pub fn lifecycle(&self, at_secs: f64, what: &str) {
        let line = format!("{} +{:.1}s", clean(what), at_secs);
        self.edit(|st| {
            if st.lifecycle.len() >= LIFECYCLE_RING {
                st.lifecycle.pop_front();
            }
            st.lifecycle.push_back(line);
            true
        });
    }

    /// The version and the sections it made, read together.
    pub fn snapshot(&self) -> (u64, Vec<Section>) {
        let st = self.lock();
        let v = self.version.load(Ordering::Acquire);
        (v, compose(&st))
    }
}

/// What the host writes for a Tizen key it could not read (`Program.cs` `AddFact`).
const HOST_UNAVAILABLE: &str = "(unavailable)";

/// A host fact as the panel shows it: `?` when the host sent nothing, `n/a` when it sent
/// [`HOST_UNAVAILABLE`] (the long form would not fit the short caps, e.g. the CPU flags' 8 characters).
fn host<'a>(st: &'a State, key: &str) -> &'a str {
    match st.host.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty()) {
        Some(HOST_UNAVAILABLE) => "n/a",
        Some(v) => v,
        None => "?",
    }
}

fn compose(st: &State) -> Vec<Section> {
    let mut out = vec![
        Section { tone: Tone::Title, text: HEADER.to_string() },
        Section { tone: Tone::Hint, text: HINT.to_string() },
    ];

    // --- keys -------------------------------------------------------------------------------
    if st.events.is_empty() {
        out.push(Section { tone: Tone::Key, text: "Key events: none yet (press a remote button)".to_string() });
    } else {
        let rows: Vec<String> =
            st.events.iter().map(String::as_str).collect::<Vec<_>>().chunks(EVENTS_PER_ROW).map(|r| r.join("   |   ")).collect();
        out.push(Section {
            tone: Tone::Key,
            text: format!("Last {} key events, oldest first (DOWN/UP, Ecore keyname, keycode):\n{}", st.events.len(), rows.join("\n")),
        });
    }
    if !st.seen.is_empty() {
        let mut shown = String::new();
        let mut n = 0;
        for pair in &st.seen {
            if !shown.is_empty() && shown.len() + pair.len() + 2 > SEEN_SHOWN_CHARS {
                break;
            }
            if !shown.is_empty() {
                shown.push_str("  ");
            }
            shown.push_str(pair);
            n += 1;
        }
        let more = st.seen.len() - n;
        let tail = if more > 0 { format!("  ... +{more} more") } else { String::new() };
        out.push(Section {
            tone: Tone::Key,
            text: format!("Keys seen, {} distinct (name=keycode): {shown}{tail}", st.seen.len()),
        });
    }

    // --- lifecycle --------------------------------------------------------------------------
    if !st.lifecycle.is_empty() {
        let items: Vec<&str> = st.lifecycle.iter().map(String::as_str).collect();
        out.push(Section { tone: Tone::Fact, text: format!("Lifecycle since start: {}", items.join("   |   ")) });
    }

    // --- steps ------------------------------------------------------------------------------
    for (step, ok, text) in &st.steps {
        let label = match step {
            Step::So => "1 .so",
            Step::Libs => "2 libs",
            Step::Window => "3 window",
            Step::Egl => "4 EGL",
            Step::Gl => "5 GL",
            Step::Sdl => "6 SDL/TTF",
            Step::Sample => "7 UTF-8",
            Step::Gfx => "8 gfx",
            Step::Net => "9 net",
        };
        out.push(Section { tone: if *ok { Tone::Ok } else { Tone::Warn }, text: format!("{label}: {text}") });
    }

    // --- device facts -----------------------------------------------------------------------
    // Each value is capped to what a real one needs, so one odd string cannot push the lines below it
    // off the plate (at 28 px the plate has room for about two wrapped lines more than a typical panel
    // uses, steps 8 and 9 included; `panel_render`'s fit tests are what keep that true).
    let mut facts = Vec::new();
    facts.push(format!(
        "Tizen {} (native API {}) | model {} | maker {} | build {}",
        cap(host(st, "platform.version"), 24),
        cap(host(st, "native.api"), 24),
        cap(host(st, "model_name"), 48),
        cap(host(st, "manufacturer"), 24),
        cap(host(st, "build.string"), 48),
    ));
    let (arch, bits, machine) = match &st.native {
        Some(n) => (n.arch.as_str(), n.ptr_bits.to_string(), cap(&n.machine, 24)),
        None => ("?", "?".to_string(), "?".to_string()),
    };
    facts.push(format!(
        "CPU flags: armv7 {} | aarch64 {} | x86 {} | x86_64 {} | uname machine {machine} | libplx built for {arch} ({bits}-bit)",
        cap(host(st, "cpu.armv7"), 8),
        cap(host(st, "cpu.aarch64"), 8),
        cap(host(st, "cpu.x86"), 8),
        cap(host(st, "cpu.x86_64"), 8),
    ));
    facts.push(format!(
        "runtime {} | process {} | OS arch {} | IntPtr {} bytes",
        cap(host(st, "runtime"), 48),
        cap(host(st, "procarch"), 16),
        cap(host(st, "osarch"), 16),
        cap(host(st, "intptr"), 8),
    ));
    facts.push(format!("OS {}", host(st, "os")));
    let (exe, glibc) = match &st.native {
        Some(n) => (cap(&n.exe, 70), cap(&n.glibc, 16)),
        None => ("?".to_string(), "?".to_string()),
    };
    facts.push(format!("exe {exe} | glibc {glibc}"));
    out.push(Section { tone: Tone::Fact, text: facts.join("\n") });
    out
}

/// `s` cut to `max` characters, with ASCII `...` marking a cut (the font need not have an ellipsis).
fn cap(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let mut cut: String = s.chars().take(max.saturating_sub(3)).collect();
        cut.push_str("...");
        cut
    } else {
        s.to_string()
    }
}

/// A text value as the panel may show it: control characters become spaces, the ends are trimmed and
/// a runaway length is cut ([`VALUE_MAX`]).
pub fn clean(s: &str) -> String {
    clean_to(s, VALUE_MAX)
}

/// [`clean`] with the cut at `max` characters.
fn clean_to(s: &str, max: usize) -> String {
    let flat: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    cap(flat.trim(), max)
}

/// A key name: printable, no spaces (a space would blur the `name=code` pairs), short.
fn clean_token(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_control() || c.is_whitespace() { '_' } else { c }).take(24).collect();
    if t.is_empty() {
        "(none)".to_string()
    } else {
        t
    }
}

/// Parse the host's facts: one `name=value` per line (`\n` or `\r\n`). A line without `=`, with an
/// empty name or a name with spaces is dropped. Values are [`clean`]ed.
pub fn parse_host_facts(raw: &str) -> Vec<(String, String)> {
    raw.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            let k = k.trim();
            if k.is_empty() || k.contains(char::is_whitespace) {
                return None;
            }
            Some((k.to_string(), clean(v)))
        })
        .collect()
}

// --- the process-global panel, as plain functions ----------------------------------------------

fn start() -> &'static Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now)
}

/// Seconds since the first call (the library calls it at the start of `plx_tizen_create`).
pub fn uptime() -> f64 {
    start().elapsed().as_secs_f64()
}

pub fn version() -> u64 {
    PANEL.version()
}

pub fn snapshot() -> (u64, Vec<Section>) {
    PANEL.snapshot()
}

pub fn step(step: Step, ok: bool, text: &str) {
    PANEL.step(step, ok, text);
}

/// A step result of several lines (steps 8 and 9); see [`Panel::step_lines`].
pub fn step_lines(step: Step, ok: bool, lines: &[String]) {
    PANEL.step_lines(step, ok, lines);
}

pub fn key_event(down: bool, name: &str, code: u32) {
    PANEL.key_event(down, name, code);
}

pub fn lifecycle(what: &str) {
    PANEL.lifecycle(uptime(), what);
}

/// "Has the content moved since I last rendered?", for a renderer that must not redraw an unchanged
/// panel. Starts at 0, which no real version equals.
#[derive(Default)]
pub struct Tracker {
    rendered: u64,
}

impl Tracker {
    pub fn stale(&self, current: u64) -> bool {
        current != self.rendered
    }

    pub fn rendered(&mut self, version: u64) {
        self.rendered = version;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(p: &Panel) -> String {
        p.snapshot().1.iter().map(|s| s.text.clone()).collect::<Vec<_>>().join("\n")
    }

    fn typical() -> Panel {
        let p = Panel::new();
        p.set_host_facts(parse_host_facts(
            "platform.version=6.0\nnative.api=6.0\nplatform.name=Tizen\nmodel_name=SmartTV-Test\nmanufacturer=Samsung\n\
             build.string=T-TEST-1234.5\ncpu.armv7=True\ncpu.aarch64=False\ncpu.x86=False\ncpu.x86_64=False\n\
             runtime=.NET 8.0.1\nos=Linux 4.4.35 #1 SMP\nosarch=Arm\nprocarch=Arm\nintptr=4\n",
        ));
        p.set_native(NativeFacts {
            arch: "arm".into(),
            ptr_bits: 32,
            machine: "armv7l".into(),
            glibc: "2.30".into(),
            exe: "/usr/bin/dotnet-launcher".into(),
        });
        p
    }

    #[test]
    fn header_carries_version_and_the_embedded_sha() {
        assert!(HEADER.starts_with("PlxNative Samsung probe "), "{HEADER}");
        assert!(HEADER.contains(env!("CARGO_PKG_VERSION")));
        assert!(HEADER.contains(" \u{2014} "), "the em dash of the brief");
        let sha = HEADER.rsplit(" \u{2014} ").next().unwrap();
        assert!(!sha.is_empty() && !sha.contains(' '), "sha part {sha:?}");
        assert_eq!(sha, env!("PLX_GIT_SHA"));
    }

    #[test]
    fn the_instruction_names_every_button_of_the_brief() {
        for needle in ["arrows", "OK", "Back", "Play/Pause", "Stop", "red/green/yellow/blue", "0-9", "CH+/CH", "photograph this screen"] {
            assert!(HINT.contains(needle), "{needle:?} missing from {HINT:?}");
        }
    }

    #[test]
    fn sections_come_in_the_documented_order_and_carry_the_facts() {
        let p = typical();
        p.step(Step::So, true, "loaded");
        p.step(Step::Gl, true, "vendor X");
        p.key_event(true, "Return", 36);
        p.lifecycle(1.25, "pause");
        let (_, s) = p.snapshot();
        let tones: Vec<Tone> = s.iter().map(|x| x.tone).collect();
        assert_eq!(tones, [Tone::Title, Tone::Hint, Tone::Key, Tone::Key, Tone::Fact, Tone::Ok, Tone::Ok, Tone::Fact]);
        let all = text_of(&p);
        for needle in [
            "Tizen 6.0", "model SmartTV-Test", "build T-TEST-1234.5", "armv7 True", "uname machine armv7l", "built for arm (32-bit)",
            "runtime .NET 8.0.1", "process Arm", "IntPtr 4 bytes", "OS Linux 4.4.35", "exe /usr/bin/dotnet-launcher", "glibc 2.30",
            "1 .so: loaded", "5 GL: vendor X", "pause +1.2s",
        ] {
            assert!(all.contains(needle), "{needle:?} missing from:\n{all}");
        }
    }

    #[test]
    fn steps_stay_in_display_order_whatever_order_they_arrive_in() {
        let p = Panel::new();
        p.step(Step::Sdl, true, "later");
        p.step(Step::So, true, "first");
        p.step(Step::Gl, false, "middle");
        let t = text_of(&p);
        let (a, b, c) = (t.find("1 .so").unwrap(), t.find("5 GL").unwrap(), t.find("6 SDL/TTF").unwrap());
        assert!(a < b && b < c, "{t}");
        let (_, s) = p.snapshot();
        assert!(s.iter().any(|x| x.tone == Tone::Warn && x.text.starts_with("5 GL")), "a failed step is Warn");
    }

    #[test]
    fn the_version_moves_only_when_the_content_does() {
        let p = Panel::new();
        let v0 = p.version();
        p.step(Step::So, true, "loaded");
        let v1 = p.version();
        assert!(v1 > v0);
        p.step(Step::So, true, "loaded");
        assert_eq!(p.version(), v1, "the same step text again is not a change");
        p.step(Step::So, false, "loaded");
        assert!(p.version() > v1, "the same text with another outcome is");
        let v2 = p.version();
        p.set_host_facts(parse_host_facts("model_name=A\n"));
        let v3 = p.version();
        assert!(v3 > v2);
        p.set_host_facts(parse_host_facts("model_name=A\n"));
        assert_eq!(p.version(), v3);
        p.set_host_facts(parse_host_facts("model_name=B\n"));
        assert!(p.version() > v3);
        let n = NativeFacts { arch: "arm".into(), ptr_bits: 32, ..Default::default() };
        p.set_native(n.clone());
        let v4 = p.version();
        p.set_native(n);
        assert_eq!(p.version(), v4);
        // A key event always changes the panel (it is a new DOWN or UP), so it always bumps.
        p.key_event(true, "Up", 111);
        assert!(p.version() > v4);
    }

    #[test]
    fn a_tracker_asks_for_a_render_only_when_the_version_moved() {
        let p = Panel::new();
        let mut t = Tracker::default();
        assert!(t.stale(p.version()), "nothing rendered yet");
        let (v, _) = p.snapshot();
        t.rendered(v);
        assert!(!t.stale(p.version()));
        assert!(!t.stale(p.version()), "asking again changes nothing");
        p.key_event(true, "Up", 111);
        assert!(t.stale(p.version()));
        let (v, _) = p.snapshot();
        t.rendered(v);
        assert!(!t.stale(p.version()));
    }

    #[test]
    fn the_event_ring_keeps_the_last_eight_in_order() {
        let p = Panel::new();
        for i in 0..12u32 {
            p.key_event(i % 2 == 0, "Right", 100 + i);
        }
        let t = text_of(&p);
        assert!(t.contains("Last 8 key events"), "{t}");
        assert!(!t.contains("DOWN Right 100"), "the oldest are gone: {t}");
        assert!(t.contains("DOWN Right 104") && t.contains("UP Right 111"), "{t}");
        let (a, b) = (t.find("Right 104").unwrap(), t.find("Right 111").unwrap());
        assert!(a < b, "oldest first");
        let rows = t.lines().filter(|l| l.contains("Right 10") || l.contains("Right 11")).count();
        assert!(rows >= 2, "eight events print as two rows of four: {t}");
    }

    #[test]
    fn keys_seen_accumulates_distinct_pairs_past_the_ring() {
        let p = Panel::new();
        for (n, c) in [("Up", 111u32), ("Down", 116), ("Return", 36), ("XF86Back", 166), ("XF86AudioPlay", 207), ("1", 10), ("2", 11)] {
            p.key_event(true, n, c);
            p.key_event(false, n, c);
        }
        p.key_event(true, "Up", 111); // a repeat adds no pair
        let t = text_of(&p);
        assert!(t.contains("Keys seen, 7 distinct"), "{t}");
        for pair in ["Up=111", "Down=116", "Return=36", "XF86Back=166", "XF86AudioPlay=207", "1=10", "2=11"] {
            assert!(t.contains(pair), "{pair} missing: {t}");
        }
        // The same key name with another code is another pair: that is a finding, not noise.
        p.key_event(true, "Up", 999);
        assert!(text_of(&p).contains("Up=999"));
    }

    #[test]
    fn keys_seen_is_bounded_and_says_how_many_it_hid() {
        let p = Panel::new();
        for i in 0..70u32 {
            p.key_event(true, &format!("XF86Key{i}"), 1000 + i);
        }
        let t = text_of(&p);
        let line = t.lines().find(|l| l.starts_with("Keys seen")).unwrap();
        assert!(line.len() < SEEN_SHOWN_CHARS + 80, "{} chars", line.len());
        assert!(line.contains("70 distinct") && line.contains("more"), "{line}");
    }

    #[test]
    fn no_key_is_special_the_panel_records_back_like_any_other() {
        // The brief: Back must not exit. The panel (and `keys.rs`) have no notion of a quit key; this
        // pins that a Back event is only ever recorded.
        let p = Panel::new();
        p.key_event(true, "XF86Back", 166);
        p.key_event(false, "XF86Back", 166);
        let t = text_of(&p);
        assert!(t.contains("DOWN XF86Back 166") && t.contains("UP XF86Back 166"), "{t}");
    }

    #[test]
    fn lifecycle_keeps_the_last_four_and_names_a_pause_resume_round_trip() {
        let p = Panel::new();
        p.lifecycle(0.4, "focus IN");
        p.lifecycle(12.34, "pause");
        p.lifecycle(15.06, "resume");
        let t = text_of(&p);
        assert!(t.contains("focus IN +0.4s   |   pause +12.3s   |   resume +15.1s"), "{t}");
        for i in 0..5 {
            p.lifecycle(20.0 + i as f64, &format!("e{i}"));
        }
        let t = text_of(&p);
        assert!(!t.contains("pause +") && t.contains("e1 +21.0s") && t.contains("e4 +24.0s"), "{t}");
    }

    #[test]
    fn hostile_values_are_flattened_and_cut() {
        let long = "x".repeat(500);
        let facts = parse_host_facts(&format!("os=a\nb\tc\r\nbad line\n=novalue\nmodel_name={long}\nname with space=1\nok=\n"));
        let get = |k: &str| facts.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("os").as_deref(), Some("a"));
        assert_eq!(get("model_name").map(|v| v.chars().count()), Some(VALUE_MAX));
        assert!(get("model_name").unwrap().ends_with("..."));
        assert_eq!(get("ok").as_deref(), Some(""), "an empty value is kept and shows as ?");
        assert!(get("bad line").is_none() && get("").is_none() && get("name with space").is_none());
        assert_eq!(clean("a\u{7}b\nc"), "a b c");
        assert_eq!(clean_token("Page Up\n"), "Page_Up_");
        assert_eq!(clean_token(""), "(none)");
        assert_eq!(clean_token(&"k".repeat(60)).chars().count(), 24);
    }

    #[test]
    fn a_value_the_host_could_not_read_prints_n_a_and_no_device_value_is_cut() {
        // The host writes "(unavailable)" for a Tizen key that is missing (Program.cs `AddFact`). At 13
        // characters it was cut to "(unav..." by the 8-character cap of the CPU flags; "n/a" fits every cap.
        let all = [
            "platform.version", "native.api", "model_name", "manufacturer", "build.string", "cpu.armv7", "cpu.aarch64",
            "cpu.x86", "cpu.x86_64", "runtime", "procarch", "osarch", "intptr", "os",
        ];
        let raw: String = all.iter().map(|k| format!("{k}=(unavailable)\n")).collect();
        let p = Panel::new();
        p.set_host_facts(parse_host_facts(&raw));
        let t = text_of(&p);
        let cpu = t.lines().find(|l| l.starts_with("CPU flags")).unwrap();
        assert!(cpu.starts_with("CPU flags: armv7 n/a | aarch64 n/a | x86 n/a | x86_64 n/a |"), "{cpu}");
        assert!(!t.contains("unav") && !t.contains("(un"), "{t}");
        assert!(t.contains("Tizen n/a (native API n/a) | model n/a | maker n/a | build n/a"), "{t}");
        // A real set's facts are shown whole: no value hits its cap (a cut ends in "...").
        let facts = typical().snapshot().1.pop().unwrap().text;
        assert!(!facts.contains("..."), "{facts}");
        assert!(!t.lines().skip_while(|l| !l.starts_with("Tizen")).any(|l| l.contains("...")), "{t}");
    }

    #[test]
    fn steps_8_and_9_follow_step_7_and_a_multi_line_result_keeps_its_line_breaks() {
        let p = Panel::new();
        p.step_lines(Step::Net, true, &["OK: GET /identity -> 200".to_string(), "libcurl.so.4, curl 8.5.0".to_string()]);
        p.step_lines(Step::Gfx, false, &["FAILED: plx_gfx not entered".to_string(), "shaders 12/14 compile+link".to_string()]);
        p.step(Step::Sample, true, "sample");
        let t = text_of(&p);
        let (s7, s8, s9) = (t.find("7 UTF-8").unwrap(), t.find("8 gfx").unwrap(), t.find("9 net").unwrap());
        assert!(s7 < s8 && s8 < s9, "{t}");
        assert!(t.contains("8 gfx: FAILED: plx_gfx not entered\nshaders 12/14 compile+link"), "{t}");
        assert!(t.contains("9 net: OK: GET /identity -> 200\nlibcurl.so.4, curl 8.5.0"), "{t}");
        let (_, s) = p.snapshot();
        assert!(s.iter().any(|x| x.tone == Tone::Warn && x.text.starts_with("8 gfx")), "a failed step is Warn");
        assert!(s.iter().any(|x| x.tone == Tone::Ok && x.text.starts_with("9 net")));
    }

    #[test]
    fn step_lines_cleans_each_line_drops_blank_ones_and_moves_the_version_only_on_a_change() {
        let p = Panel::new();
        let v0 = p.version();
        let lines = ["a\u{7}b".to_string(), "   ".to_string(), "x".repeat(600)];
        p.step_lines(Step::Gfx, true, &lines);
        let t = text_of(&p);
        let mut it = t.lines().filter(|l| l.starts_with("8 gfx") || l.starts_with('x'));
        assert_eq!(it.next(), Some("8 gfx: a b"), "{t}");
        let second = it.next().unwrap();
        assert_eq!(second.chars().count(), STEP_LINE_MAX, "a runaway line is cut at the step cap");
        assert!(second.ends_with("..."));
        let v1 = p.version();
        assert!(v1 > v0);
        p.step_lines(Step::Gfx, true, &lines);
        assert_eq!(p.version(), v1, "the same two lines again is not a change");
        p.step_lines(Step::Gfx, false, &lines);
        assert!(p.version() > v1);
    }

    /// The owner's photo of the first emulator run showed "...syntax error,...; no G...": a step line was cut
    /// at a value's 110 characters while most of the row was empty. A step line is evidence and is wrapped,
    /// not cut, up to the step cap.
    #[test]
    fn a_step_line_of_evidence_is_shown_whole_not_cut_at_a_values_cap() {
        let first = "first failure init_gl ambient, vertex: 0:37(9): error: syntax error, unexpected NEW_IDENTIFIER";
        let ext = "shaders 13/14 compile+link; no GL ext: ARM|EXT_shader_framebuffer_fetch EXT_disjoint_timer_query";
        assert!(first.chars().count() < STEP_LINE_MAX && ext.chars().count() < STEP_LINE_MAX);
        assert!(first.chars().count() + ext.chars().count() > 2 * VALUE_MAX - 40);
        let p = Panel::new();
        p.step_lines(Step::Gfx, false, &["FAILED".to_string(), first.repeat(2), ext.to_string()]);
        let t = text_of(&p);
        assert!(t.contains(&first.repeat(2)) && t.contains(ext), "{t}");
        assert!(!t.contains("..."), "nothing under the step cap is cut: {t}");
        // A single-line step still obeys a value's cap.
        p.step(Step::Sample, true, &"v".repeat(300));
        let t = text_of(&p);
        assert!(t.contains(&format!("{}...", "v".repeat(VALUE_MAX - 3))) && !t.contains(&"v".repeat(VALUE_MAX - 2)), "{t}");
    }

    #[test]
    fn missing_facts_print_a_question_mark_not_nothing() {
        let t = text_of(&Panel::new());
        assert!(t.contains("Tizen ? (native API ?)") && t.contains("uname machine ?") && t.contains("exe ? | glibc ?"), "{t}");
    }

    #[test]
    fn the_sample_text_has_latin_a_check_mark_and_cyrillic() {
        assert!(SAMPLE_TEXT.contains('\u{2713}') && SAMPLE_TEXT.contains('\u{041F}') && SAMPLE_TEXT.starts_with("PlxNative"));
    }
}
