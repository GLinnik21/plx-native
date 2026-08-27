//! The `/tmp` developer-trigger surface, behind one door.
//!
//! This app is driven headlessly by ~44 files under `/tmp/plxnative-*`: which screen to boot to,
//! which item to play, which URL to stream, whether to auto-press OK, which PMS token to use.
//! That is how `tests/run.py` and every capture scene work, and it is not going away.
//!
//! It must not exist in a public build. `/tmp` is the SHARED system `/tmp` in the production jail
//! too (mode 1777, both jail profiles), so on an ordinary user's TV every one of those files is a
//! behaviour switch any co-resident process can throw. Three are outright takeovers:
//! `plxnative-token` beats the signed-in session (`app.rs`'s boot gate), `plxnative-servers` hands
//! the app a whole additional server — an address AND the token to trust it with (see [`servers`])
//! — and `plxnative-url` replaces the stream the player feeds.
//!
//! So every read goes through here, and here is `#[cfg]`-gated on the `devtriggers` feature. In a
//! `--no-default-features` build [`flag`] is `false` and [`read`] is `None` at COMPILE time, the
//! branches behind them fold away, and the binary opens nothing under `/tmp` but its own logs.
//!
//! Two rules for anything added later:
//!
//! 1. **Never open a `/tmp` path directly.** The grep that audits this (`/tmp/plxnative-` outside
//!    this module and the four unconditional log sinks) is the only thing keeping the property
//!    true. The two profiler logs are dev-only and listed in [`DIAG`] below.
//! 2. **A gate is not always a path.** `any_trigger_present` scans the whole directory and names
//!    no file at all — it was the one surface a literal-replacement sweep would have missed, and
//!    it silently changes which screen the app boots to. Structural surfaces that take no name
//!    (the capture listener's `INADDR_ANY` socket, the remote FIFO's `mkfifo`) are gated at their
//!    call sites in `app.rs` for the same reason.
//!
//! The four unconditional LOG sinks are deliberately NOT here and stay in every build: they are creates, not
//! reads, they are how on-device crash triage works at all, and writing them is not a way for
//! another process to steer this one.

/// Files that are pure diagnostics rather than automation — see [`any_trigger_present`].
///
/// Every log this app writes belongs here, not just its trigger. `plxnative-anim` was listed and
/// `plxnative-anim.log` was not, while arming the overlay creates exactly that file and nothing
/// ever removes it (`make run` clears only the event log; `tests/run.py` spares every `*.log` by
/// design) — so a single historical anim session skipped the who's-watching picker on every later
/// boot, interactive ones included.
// `test` as well as the feature: `any_trigger_present` is the only caller and it is cfg'd out of a
// release build, but the test below asserts this list's contents and runs with default features.
#[cfg(any(feature = "devtriggers", test))]
const DIAG: [&str; 19] = [
    "plxnative-events.log",
    "plxnative-stderr.log",
    "plxnative-crash.log",
    "plxnative-anim.log",
    "plxnative-profile",
    "plxnative-gputime.jsonl",
    "plxnative-hwcnt",
    "plxnative-hwcnt.jsonl",
    "plxnative-anim",
    "plxnative-remote",
    "plxnative-capture",
    "plxnative-noidle",
    // The focus fingerprint ([`crate::focusprobe`]). Diagnostic for `noidle`'s reason and one of
    // its own: it only READS focus and writes a log line, and a (route × key) characterization
    // harness has to be able to observe the who's-watching picker, which a non-DIAG trigger would
    // suppress — the observer would remove the screen it was armed to watch.
    "plxnative-focus",
    // The two OVERDRAW surfaces and the hero-ground fold ([`crate::ui::overdraw`],
    // `docs/backdrop-blur-profiling.md` Part 5). All three are measurement knobs whose whole
    // method is an A/B against an unmasked control leg — and a non-DIAG trigger suppresses the
    // who's-watching picker, so the control leg and the masked leg would boot to DIFFERENT
    // SCREENS and the difference between them would be the screen, not the class being priced.
    // That is the exact failure this list exists to stop, and it is invisible in the numbers.
    "plxnative-overdraw",
    "plxnative-drawmask",
    "plxnative-heroground",
    // LG's own GStreamer logging ([`arm_gst_logging`]) and the file it writes. Both are DIAG for
    // the same reason `plxnative-profile` is: the whole point is to observe a playback that would
    // otherwise be unobservable, and a non-DIAG trigger would silently move the boot screen out
    // from under the very session being measured.
    "plxnative-gstlog",
    "plxnative-gst.log",
    // The forced Stats-for-nerds overlay. It only changes presentation, and grading a playback
    // case without the read-out risks a failure whose evidence was never put on screen.
    "plxnative-stats",
];

/// Is the trigger `name` (bare, without the `plxnative-` prefix) present?
#[cfg(feature = "devtriggers")]
pub(crate) fn flag(name: &str) -> bool {
    path(name).exists()
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn flag(_name: &str) -> bool {
    false
}

/// [`flag`], answered ONCE for the whole process.
///
/// **A `dev::flag` is a `stat`, so a trigger read every frame is a syscall on the 60 fps path.**
/// Latching also fixes a correctness wrinkle that has nothing to do with cost: `tests/run.py`
/// clears `/tmp/plxnative-*` between cases, so a later read can legitimately find the file gone
/// mid-run and a per-frame probe would change its answer half way through a case.
///
/// A macro rather than a function because the latch has to be a `static` per trigger, and a
/// function would need a map behind a lock — which is the thing being avoided. It lives HERE
/// because this module is the one door onto the `/tmp` surface; it was briefly a file-local macro
/// in `ui/widgets.rs`, which walled it off from the other per-frame `flag` callers
/// ([`crate::focusprobe::armed`] had already hand-rolled exactly this body, doc comment and all).
///
/// No `#[cfg]` arms, deliberately: [`flag`] is already `false` at COMPILE time without the
/// `devtriggers` feature, so a second gate here would only re-derive what the door behind it
/// guarantees.
///
/// ```ignore
/// crate::dev::latched_flag!(
///     /// `/tmp/plxnative-flattabs` — the material off, for an A/B against the flat capsule.
///     fn flat_tabs_armed = "flattabs";
/// );
/// ```
macro_rules! latched_flag {
    ($(#[$m:meta])* $vis:vis fn $name:ident = $trigger:literal;) => {
        $(#[$m])*
        $vis fn $name() -> bool {
            static SEEN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            *SEEN.get_or_init(|| $crate::dev::flag($trigger))
        }
    };
}
pub(crate) use latched_flag;

/// **Turn on the TELEVISION'S OWN GStreamer logging** — `/tmp/plxnative-gstlog`.
///
/// This is the only instrument that can see inside LG's Dolby Vision chain. That chain is
/// `dvbin` → `h265parse` → `dvsplitter` → {`lxvideodec`, `dvmdparse`} → `dualsequencer`, all of it
/// closed, and the app's own logs stop at `Feed()`. Decompilation established that
/// `mediapipeline::PlayerFactory::create()` calls `gst_debug_is_active()` and, when it is, honours
/// `GST_DEBUG_FILE_OVERWRITE` / `GST_DEBUG_FILE` by installing its own log function — so these four
/// variables are read by libpf itself and need no cooperation from us beyond setting them.
///
/// **Timing is the whole reason this is here and not later.** Neither `libpf` nor `libplayerAPIs`
/// imports `gst_init`; they use LG's lazy `gst_cool_init_check`, which does not run until a player
/// is created. `plex_run` is therefore comfortably early — but anything that arms this AFTER the
/// first `Load` would be setting variables nobody reads again.
///
/// An empty trigger takes the five Dolby categories at level 6; content overrides the whole
/// `GST_DEBUG` spec, so `dvbin:9,dualsequencer:9` or `*:3` both work. The log goes to the runtime
/// directory beside the event log.
///
/// **Not free.** Level 6 on five categories is a lot of formatted I/O on an ARM TV and it is not a
/// setting to leave armed while measuring anything about frame pacing.
#[cfg(feature = "devtriggers")]
pub(crate) fn arm_gst_logging() {
    let Some(spec) = read("gstlog") else { return };
    let spec = if spec.is_empty() {
        "dvbin:6,dvsplitter:6,dvsplitter_algo:6,dvmdparse:6,dualsequencer:6".to_string()
    } else {
        spec
    };
    let log = crate::paths::in_runtime_dir("plxnative-gst.log");
    // SAFETY: single-threaded here by construction — `plex_run` has not yet minted a worker, and
    // this runs before SDL init. `set_var` is only unsound against a concurrent reader.
    std::env::set_var("GST_DEBUG", &spec);
    std::env::set_var("GST_DEBUG_FILE", &log);
    std::env::set_var("GST_DEBUG_FILE_OVERWRITE", "enable");
    std::env::set_var("GST_DEBUG_NO_COLOR", "1");
    crate::log(&format!("gstlog: GST_DEBUG={spec} -> {}", log.display()));
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn arm_gst_logging() {}

/// The trigger's CONTENT, trimmed. `Some("")` for a trigger armed as an empty file — several
/// distinguish empty (take the default) from a value (`autoseek`, `library`, `marker`), so an
/// empty file must not read the same as an absent one.
#[cfg(feature = "devtriggers")]
pub(crate) fn read(name: &str) -> Option<String> {
    std::fs::read_to_string(path(name)).ok().map(|s| s.trim().to_string())
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn read(_name: &str) -> Option<String> {
    None
}

/// A test-only playback policy override from `plxnative-quality`.
///
/// The server matrix grades established direct-play/remux/transcode routes. It must not silently
/// become an Auto-HLS matrix because the television happened to persist that user preference in
/// an earlier run. This is deliberately an in-memory boot override: writing the session would
/// make a test change the owner's real preference. Unknown and empty values fail closed by
/// producing no override.
pub(crate) fn playback_quality_override() -> Option<crate::plex::session::PlaybackQuality> {
    let value = read("quality")?;
    parse_playback_quality(&value)
}

/// The inverse of [`parse_playback_quality`], and it lives here so the two cannot drift.
///
/// The log line a test greps must carry the SAME string the trigger accepts. `Quality::label()`
/// is display text ("1080p \u{b7} 8 Mbps") and would make a case state its rung twice, in two
/// spellings, with nothing keeping them in step — which is the shape that rots.
pub(crate) fn quality_wire_name(q: crate::plex::session::PlaybackQuality) -> &'static str {
    use crate::plex::session::PlaybackQuality as Q;
    match q {
        Q::Auto => "auto",
        Q::Original => "original",
        Q::P1080High => "1080p_20_mbps",
        Q::P1080 => "1080p_8_mbps",
        Q::P720 => "720p_4_mbps",
        Q::P720Low => "720p_2_mbps",
        Q::P480 => "480p_720_kbps",
    }
}

fn parse_playback_quality(value: &str) -> Option<crate::plex::session::PlaybackQuality> {
    use crate::plex::session::PlaybackQuality;
    match value {
        "auto" => Some(PlaybackQuality::Auto),
        "original" => Some(PlaybackQuality::Original),
        "1080p_20_mbps" => Some(PlaybackQuality::P1080High),
        "1080p_8_mbps" => Some(PlaybackQuality::P1080),
        "720p_4_mbps" => Some(PlaybackQuality::P720),
        "720p_2_mbps" => Some(PlaybackQuality::P720Low),
        "480p_720_kbps" => Some(PlaybackQuality::P480),
        _ => None,
    }
}

/// **Switch the playback quality MID-PLAYBACK** — `plxnative-qualityswitch=[gap=<ms>,]<q>[,<q>…]`.
///
/// The one thing [`playback_quality_override`] above cannot do. That is a BOOT override: it decides
/// what the playback starts as and is read once. What a person actually does at the television is
/// start something, watch it, and then change the quality while it plays — which re-asks the
/// routing question against a stream already on screen, reloads if the answer moved, and (on the
/// way out of Auto) tears down a running ABR controller. None of that is reachable from a boot
/// value, and none of it was reachable from a test at all.
///
/// Same vocabulary as `plxnative-quality`, deliberately — one spelling of a rung across both
/// triggers, so a case cannot name a quality one of them accepts and the other silently ignores.
/// Same grammar as `plxnative-autoseek`: an optional leading `gap=<ms>` then comma-separated steps
/// fired one per gap. There is no default gap and none is invented: with a single step no cadence
/// exists to state, and a case wanting several states its own, in the manifest, where a reader can
/// see it beside the assertions it enables.
///
/// **This writes the stored preference, and that is not incidental.** `route::set_quality` persists
/// through `plex::session::update`, because a person picking a rung means it. A test therefore
/// leaves the value behind — on the `debug` flavour's own session, never the install you watch
/// with, and `tests/run.py` writes `plxnative-quality` on every server case, so the next boot
/// overrides whatever this left. Both halves are load-bearing; neither alone would make it safe.
fn parse_quality_switch_script(
    raw: &str,
) -> Option<(u32, Vec<crate::plex::session::PlaybackQuality>)> {
    let mut steps: Vec<&str> = raw.split(',').map(str::trim).filter(|t| !t.is_empty()).collect();
    let mut gap_ms = None;
    if let Some(g) = steps.first().and_then(|f| f.strip_prefix("gap=")) {
        gap_ms = Some(g.parse().ok()?);
        steps.remove(0);
    }
    // A sequence needs an authored cadence. Without one, several valid rungs would all fire on
    // successive render loops: technically ordered, but not the mid-playback interactions the
    // trigger claims to reproduce.
    if steps.len() > 1 && gap_ms.is_none() {
        return None;
    }
    // Fail the WHOLE script when one rung is unparseable. Running a valid subset would still
    // change playback to a sequence nobody requested, merely without inventing a default rung.
    let qs: Vec<_> = steps
        .iter()
        .map(|t| parse_playback_quality(t))
        .collect::<Option<Vec<_>>>()?;
    if qs.is_empty() { None } else { Some((gap_ms.unwrap_or(0), qs)) }
}

pub(crate) fn quality_switch_script() -> Option<(u32, Vec<crate::plex::session::PlaybackQuality>)> {
    parse_quality_switch_script(&read("qualityswitch")?)
}

/// **Pin Auto's HLS ladder to one actuator, by request rate** — `plxnative-abrpin=<kbps>`.
///
/// Measurement-only, for step M4 of `docs/adaptive-playback-plan.md`: reading a settled reserve at
/// a given rung means holding that rung for minutes, and nothing in the app could do that.
/// [`playback_quality_override`] above cannot serve, for two independent reasons — a non-Auto
/// quality returns `None` from `route::hls_abr_control` before a controller is ever constructed,
/// so it measures a different transport path entirely; and [`crate::plex::session::PlaybackQuality`]
/// has no mid-1080p points, while the ladder this pins has eight of them.
///
/// The value is the actuator's REQUEST rate (`Rung::kbps`) — 320, 720, 2000, 4000, 6000, 8000,
/// 10000, 12000, 14000, 16000, 18000, 20000, 22000 — because that is the number PMS is given and
/// the one identity in the catalog that does not move when somebody re-measures the server.
/// An unrecognised or empty value pins nothing, deliberately: a typo must leave Auto alone rather
/// than silently park playback on the bottom rung for a whole measurement session.
///
/// Compiled out with `devtriggers`, so a release build cannot be pinned at all.
pub(crate) fn abr_pin() -> Option<crate::abr::Rung> {
    let raw = read("abrpin")?;
    let kbps: u32 = raw.trim().parse().ok()?;
    crate::abr::Rung::from_request_kbps(kbps)
}

/// A raw dev payload in the runtime root, by bare NAME — only `sample.h264` and `sample.h265`,
/// which predate the `plxnative-` prefix and feed the player a local Annex-B sample instead of a
/// stream. Everything else here is `plxnative-<name>`; these two are the exception, so they get
/// their own door rather than a prefix they do not have.
///
/// It took an ABSOLUTE path until the flavour split, which left them as the last two runtime
/// surfaces still pinned to a shared `/tmp` while every other one had moved — harmless in itself
/// (two installs reading one sample is fine) but a hole in the rule that every runtime surface
/// resolves through [`crate::paths::in_runtime_dir`], and rules with holes stop being checkable.
/// NB the file now goes in the install's own root: `$(make -s print-rundir)/sample.h264`.
#[cfg(feature = "devtriggers")]
pub(crate) fn read_sample(name: &str) -> Option<Vec<u8>> {
    std::fs::read(crate::paths::in_runtime_dir(name)).ok()
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn read_sample(_name: &str) -> Option<Vec<u8>> {
    None
}

/// One ADDITIONAL server's credentials, injected for an automated run — see [`servers`].
///
/// Deliberately **not** `Debug`: `token` is a live per-(user,server) PMS access token, and a
/// derived `Debug` is exactly how a secret reaches a log by accident. [`DevServer::describe`] is
/// the only formatter this type has, and it prints everything *but* the token.
#[derive(serde::Deserialize, Clone)]
pub(crate) struct DevServer {
    /// Display name. Cosmetic — what a server picker would label it.
    #[serde(default)]
    pub(crate) name: String,
    /// The server's `machineIdentifier`: its IDENTITY, and the only thing that distinguishes it
    /// from the primary once both are installed. Public, not a secret.
    #[serde(default)]
    pub(crate) machine_id: String,
    /// Address reachable **from the TV**. `address` is accepted as an alias because that is what
    /// `plex::session::ServerRef` calls the same field, and copying one into the other by hand is
    /// the obvious way to write this file.
    #[serde(default, alias = "address")]
    pub(crate) host: String,
    #[serde(default = "default_port")]
    pub(crate) port: i64,
    /// `"http"` (the default) or `"https"` — the scheme this server is reached at.
    ///
    /// **This is how an https origin is exercised headlessly**, without a plex.tv account that has
    /// one and without a television that can reach it: write `"scheme": "https"` here and the
    /// whole control plane below `plex::register_origin` sees a TLS origin. Everything the origin
    /// model changed is otherwise invisible from outside the app, because every real origin today
    /// is `http://`.
    ///
    /// A value that is neither fails the WHOLE trigger to deserialize, which
    /// [`parse_servers`] turns into a logged error rather than a silently dropped server —
    /// deliberately: a typo'd scheme that quietly meant `http` would be a run grading the thing it
    /// was armed to test as working.
    #[serde(default)]
    pub(crate) scheme: crate::plex::Scheme,
    /// This identity's per-(user,server) access token **for this server**. A shared server is a
    /// separate authority: the account token gets a 401 from it, which is the whole reason one
    /// `plxnative-token` cannot express two servers. A SECRET — never logged.
    #[serde(default)]
    pub(crate) token: String,
    /// The owner's plex.tv handle (`sourceTitle` on the wire) — "friend". EMPTY means **your own
    /// server**, which is what the Sources list draws as the absence of an owner rather than as an
    /// anonymous one, so a harness overlay that omits it injects an owned server by definition.
    /// Public, like the machine name: it is the string every browsing surface says out loud.
    #[serde(default, alias = "sourceTitle", alias = "source_title")]
    pub(crate) handle: String,
}

fn default_port() -> i64 {
    32400
}

impl DevServer {
    /// Everything about this server except the token, for the event log.
    pub(crate) fn describe(&self) -> String {
        // A SHARED server is someone else's machine, and this line goes to
        // `/tmp/plxnative-events.log` — the file that gets pasted into issues and PR bodies. Four
        // PR bodies leaked exactly these fields on 2026-08-14 and had to be redacted after the
        // fact, which a public repository does not really allow. So a share names NOTHING that
        // identifies it or its owner: not the server's name, not the plex.tv handle, not the
        // address, not the machineIdentifier.
        //
        // The token was already excluded (`describe_never_carries_the_token`), but a token is not
        // the only thing here worth protecting — an address plus a handle is a person's home.
        //
        // What survives is what DEBUGGING actually needs: that a share is present at all, whether
        // its credentials are complete, and a stable `ref` so two lines about the same server can
        // be correlated within one log without identifying it outside one. An OWNED server is the
        // user's own machine, already in the boot line and in `config.local.h`, so it is unchanged.
        if !self.handle.is_empty() {
            return format!("SHARED ref={} port_set={}", self.reference(), self.port > 0);
        }
        // by CHARS, not bytes: a machineIdentifier is hex in practice, but a hand-written file is
        // whatever someone typed, and slicing a byte range mid-codepoint panics.
        let mut mid: String = self.machine_id.chars().take(8).collect();
        if self.machine_id.chars().nth(8).is_some() {
            mid.push_str("..");
        }
        // The origin's `log_form`, not `{host}:{port}`: this trigger's whole reason for having a
        // `scheme` field is to put a TLS origin through the registry headlessly, and a description
        // that cannot say which one it injected is the `[[silent-instrument-trap]]` again. It is
        // byte-identical for the plaintext servers every overlay writes today.
        let where_ = self.origin().map(|o| o.log_form()).unwrap_or_else(|| format!("{}:{}", self.host, self.port));
        format!("name={:?} handle={:?} {where_} mid={mid}", self.name, self.handle)
    }

    /// A short, stable, NON-reversible tag for a shared server — enough to tell two shares apart in
    /// one log and to follow one across a boot, and useless to anyone reading that log elsewhere.
    ///
    /// FNV-1a over the machineIdentifier: not a cryptographic choice, a legibility one. The id is
    /// the right input because it is the only field that survives the server changing address.
    fn reference(&self) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.machine_id.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
        format!("{:06x}", h & 0xff_ffff)
    }
    /// Are these credentials complete enough to reach the server at all?
    ///
    /// The port is judged by [`crate::plex::probe::dial_port`], not by `> 0`: `app.rs` registers
    /// every server that passes this with `s.port as c_int`, and this file is a hand-written JSON
    /// blob under `/tmp` — an out-of-range `i64` wraps in that cast into a port nobody wrote down.
    pub(crate) fn usable(&self) -> bool {
        !self.token.is_empty() && self.origin().is_some()
    }

    /// **Where this server is** — the [`crate::plex::Origin`] to register it at, `None` when the
    /// trigger did not write enough to dial.
    ///
    /// The port goes through `probe::dial_port` for the reason [`DevServer::usable`] gives: this
    /// is a hand-written JSON blob under `/tmp`, and `port as i32` wraps an out-of-range `i64`
    /// into a plausible-looking one.
    pub(crate) fn origin(&self) -> Option<crate::plex::Origin> {
        if self.host.is_empty() {
            return None;
        }
        crate::plex::probe::dial_port(self.port).map(|p| crate::plex::Origin::new(self.scheme, &self.host, p))
    }
}

/// Parse the `servers` trigger's content: a JSON array of [`DevServer`], or a single bare object.
///
/// An `Err` rather than an empty list on malformed JSON, deliberately — a run that injected
/// credentials and got them silently dropped would grade as "the feature is broken", when the real
/// fault is a typo in the harness overlay. The caller logs the parse error.
#[cfg(any(feature = "devtriggers", test))]
fn parse_servers(s: &str) -> Result<Vec<DevServer>, String> {
    // Many FIRST: `untagged` tries the variants in order, and a derived struct deserializer also
    // accepts a SEQUENCE (positional fields), so with `One` first an empty `[]` came back as one
    // all-defaults server instead of no servers at all.
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum ManyOrOne {
        Many(Vec<DevServer>),
        One(DevServer),
    }
    if s.trim().is_empty() {
        return Ok(Vec::new()); // an empty file means "no extra servers", not a syntax error
    }
    match serde_json::from_str::<ManyOrOne>(s) {
        Ok(ManyOrOne::Many(v)) => Ok(v),
        Ok(ManyOrOne::One(d)) => Ok(vec![d]),
        Err(e) => Err(e.to_string()),
    }
}

/// The EXTRA servers this boot was given credentials for — `/tmp/plxnative-servers`.
///
/// Purely ADDITIVE: the primary server is still the compiled-in host plus `plxnative-token` (or the
/// signed-in session), untouched, so a run that names one server behaves exactly as it always has.
/// This is the channel for the *second* authority — a friend's shared server, which has its own
/// `machineIdentifier` and its own access token and answers 401 to anybody else's.
///
/// Read and parsed **once**. `tests/run.py` wipes `/tmp/plxnative-*` between cases and again on
/// exit (pass, fail or Ctrl-C — that is how a live token stops surviving in world-readable `/tmp`),
/// so a second read could legitimately see the file gone. The credentials a boot was handed are a
/// property of that boot, not of what `/tmp` happens to hold when someone asks.
#[cfg(feature = "devtriggers")]
pub(crate) fn servers() -> Result<Vec<DevServer>, String> {
    static ONCE: std::sync::OnceLock<Result<Vec<DevServer>, String>> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| match read("servers") {
        Some(s) => parse_servers(&s),
        None => Ok(Vec::new()),
    })
    .clone()
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn servers() -> Result<Vec<DevServer>, String> {
    Ok(Vec::new())
}

/// A stream to play and **the Load payload declaration to play it with**, with no library item
/// behind it — `/tmp/plxnative-playurl`, the player-PIPELINE test tier's one entry point.
///
/// This is the trigger that makes the pipeline testable without Plex. `plxnative-url` already
/// hands the engine a URL, and everything downstream of it — `stream.rs`, `ff.rs`, `aq.rs`, the
/// pump's `Feed()`, the ACB bind — is byte-identical to a real playback. What it CANNOT do is say
/// what the stream *is*: the Starfish `Load` payload takes its codecs from `route::stream_vcodec`
/// / `stream_acodec`, and its Dolby nodes from `stream_dovi` / `stream_immersive`, all five of
/// which are written only by `route::apply_plan` from a PMS decision. So a URL-fed 4K HEVC file
/// was declared to the television as whatever the route happened to hold — in a fresh boot, the
/// empty string, which falls through [`crate::player::engine`]'s `_ =>` arm to `"AC3"` and an
/// H264 payload. The declaration is precisely what governs HEVC-vs-H264 payload selection, the
/// `"AC3 PLUS"` naming trap, and both Dolby nodes, so a tier that cannot set it cannot test them.
///
/// JSON, whole-file, one object. Chosen over a `key=value` line for three reasons: the DV node is
/// nested, `serde_json` is already a dependency and [`DevServer`] is the established precedent for
/// exactly this shape, and JSON contains no apostrophes — which matters because `tests/run.py`
/// writes triggers through a single-quoted `printf` with no escaping.
///
/// ```jsonc
/// {"url":"http://192.0.2.10:8020/pipe_hevc_eac3_4k_dovi_p8.mkv",
///  "vcodec":"hevc", "acodec":"eac3", "fps":23.976,
///  "dovi":{"profile":8,"bl_compat":1,"el_present":false},
///  "atmos":false}
/// ```
///
/// Deliberately **not** `Debug`, for [`DevServer`]'s reason one step removed: `url` is a free
/// string, and while the pipeline tier's own URLs carry no credentials, the field is the same
/// shape as `route::url()` — which for a real playback carries `X-Plex-Token` in its query. A
/// derived `Debug` is how that reaches a log the day someone points this trigger at a PMS part.
#[derive(serde::Deserialize, Clone)]
pub(crate) struct PlayUrl {
    /// `http://<dotted-quad>:<port>/<path>`. **A dotted quad, not a hostname** — `stream.rs` does
    /// no DNS on this path, so a name is a flat failure to open with nothing to read it by.
    #[serde(default)]
    pub(crate) url: String,
    /// The Load payload's video codec: `"hevc"` selects the H265 payload, anything else H264.
    #[serde(default)]
    pub(crate) vcodec: String,
    /// The Load payload's audio codec, in FFmpeg's spelling (`"eac3"`, not `"AC3 PLUS"`) — the
    /// engine does the LG-side renaming, which is the trap this tier exists to keep testing.
    #[serde(default)]
    pub(crate) acodec: String,
    /// Source frame rate for the Load `esInfo`; 0 omits it, exactly as a transcode does.
    #[serde(default)]
    pub(crate) fps: f64,
    /// Dolby Vision layering, for the payload's `contents.DolbyHdrInfo` node. Absent = none.
    #[serde(default)]
    pub(crate) dovi: PlayDovi,
    /// Dolby Atmos, for the payload's `contents.immersive` node.
    #[serde(default)]
    pub(crate) atmos: bool,
    /// Pipeline-tier Auto watchdog seam: whole Original wire bitrate. Zero leaves the ordinary
    /// Plex-free one-shot playback unchanged.
    #[serde(default)]
    pub(crate) auto_source_kbps: u32,
    /// Same-origin fixture HLS root used after the synthetic Original becomes unsustainable.
    /// Present only in debug/test artifacts; production playback obtains replacement URLs from
    /// PMS through `HlsAbrControl`.
    #[serde(default)]
    pub(crate) auto_hls_base: String,
    /// **Start in HLS instead of arriving there through a starvation.** The pipeline tier's ABR
    /// cases exist to exercise the HLS controller, and until 2026-08-27 their only way in was to
    /// declare an Original source rate no link could carry (900 000 kbps) and let the starvation
    /// horizon fire. That worked only because the horizon fired without checking whether the
    /// reserve was actually draining — on an unshaped link it was FILLING — so the entry depended
    /// on a defect, and it stopped working the moment the defect was fixed
    /// (`docs/measurements/local-original-blind.md`, `docs/measurements/orig-first-window-fallback.md`).
    /// With this set, `route::arm_auto_fixture` installs the post-fallback state directly and the
    /// controller runs from the first segment. `pipe_auto_original_slow_recover` deliberately does
    /// NOT set it: the transition is what that case grades.
    #[serde(default)]
    pub(crate) auto_start_hls: bool,
}

/// The four DV fields the Load payload actually decides on — [`crate::metadata::Dovi`]'s
/// decision half. The three descriptive fields (level, version, bl/rpu present) are read by the
/// tracks panel and by nothing on the playback path, so this trigger does not carry them.
#[derive(serde::Deserialize, Clone, Copy, Default)]
pub(crate) struct PlayDovi {
    /// 5 / 7 / 8. **Zero means no Dolby Vision at all** — it is what drives `present` below,
    /// rather than a separate flag that could disagree with it.
    #[serde(default)]
    pub(crate) profile: i64,
    /// `DOVIBLCompatID` — 0 none (P5) / 1 HDR10 / 2 SDR / 4 HLG.
    #[serde(default)]
    pub(crate) bl_compat: i64,
    /// An enhancement layer is present (P7).
    #[serde(default)]
    pub(crate) el_present: bool,
}

impl PlayDovi {
    /// The engine-facing record. `present` is DERIVED from a non-zero profile rather than carried
    /// separately: two fields that can disagree is a way to declare "Dolby Vision, profile 0",
    /// which is not a thing, and the harness would have to keep them in step by hand in every case.
    pub(crate) fn to_dovi(self) -> crate::metadata::Dovi {
        crate::metadata::Dovi {
            present: self.profile > 0,
            profile: self.profile,
            bl_compat: self.bl_compat,
            el_present: self.el_present,
            ..crate::metadata::Dovi::NONE
        }
    }
}

/// Parse the `playurl` trigger's content. Pure, so the host suite can pin it.
///
/// An `Err` rather than a defaulted object on malformed input, for [`parse_servers`]' reason: a
/// run whose declaration was silently dropped grades as "the payload is wrong", when the fault is
/// a typo in the harness. An empty `url` is an `Err` too — an all-defaults object would send the
/// engine looking for `plxnative-url` instead and the case would play something else entirely.
#[cfg(any(feature = "devtriggers", test))]
fn parse_playurl(s: &str) -> Result<PlayUrl, String> {
    let p: PlayUrl = serde_json::from_str(s).map_err(|e| e.to_string())?;
    if p.url.is_empty() {
        return Err("no `url`".to_string());
    }
    Ok(p)
}

/// This boot's URL-and-declaration, if one was armed — `/tmp/plxnative-playurl`.
///
/// `None` = not armed; `Some(Err)` = armed but unreadable, which the caller logs.
///
/// **Not memoized**, unlike [`servers`]: every `start_bufferfeed` re-reads it, because a seek that
/// escalates to a full reload tears the engine down and builds the payload again, and a
/// declaration that applied only to the first `Load` would make the second one silently wrong.
/// `servers` is memoized for the opposite reason — credentials are a property of the boot.
#[cfg(feature = "devtriggers")]
pub(crate) fn playurl() -> Option<Result<PlayUrl, String>> {
    read("playurl").map(|s| parse_playurl(&s))
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn playurl() -> Option<Result<PlayUrl, String>> {
    None
}

/// Is ANY non-diagnostic trigger armed? Used to skip the boot who's-watching picker, so that a
/// headless run lands on a deterministic Home.
///
/// This is the surface with no path literal: it `read_dir`s the runtime root and matches by
/// prefix, so in a release build it would still have run — and still have changed the boot
/// screen from a squatted file — after every named read had been compiled out.
///
/// **A trigger is a FILE.** Nothing else here names a path, so nothing else could have caught a
/// non-file entry whose name happens to match: a directory called `plxnative-anything` sitting in
/// the runtime root would read as an armed trigger and permanently suppress the boot picker,
/// silently changing which screen this install comes up on with no line in any log. That became
/// reachable the moment two installs could share `/tmp` — the obvious name for a second install's
/// runtime root is exactly `plxnative-<flavour>`, which is why `paths::resolve_runtime_dir` spells
/// it with a DOT instead. Two independent reasons is the right number for a failure this quiet.
#[cfg(feature = "devtriggers")]
pub(crate) fn any_trigger_present() -> bool {
    std::fs::read_dir(crate::paths::runtime_dir())
        .ok()
        .map(|rd| rd.filter_map(|e| e.ok()).any(|e| is_armed_trigger(&e)))
        .unwrap_or(false)
}

#[cfg(feature = "devtriggers")]
fn is_armed_trigger(entry: &std::fs::DirEntry) -> bool {
    if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
        return false;
    }
    let name = entry.file_name();
    let name = name.to_string_lossy();
    name.starts_with("plxnative-") && !DIAG.contains(&name.as_ref())
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn any_trigger_present() -> bool {
    false
}

/// `true` when this build reads `/tmp` at all — for the one boot log line that says so, and for
/// call sites gating a whole subsystem (the capture listener, the remote FIFO) rather than a read.
pub(crate) const ENABLED: bool = cfg!(feature = "devtriggers");

/// The trigger's absolute path. `/tmp/plxnative-<name>` on the television; see
/// [`crate::paths::runtime_dir`] for why a host build may put the whole namespace elsewhere.
///
/// `test` is in the cfg beside the feature, and only for a compile reason: the test below writes
/// through this door rather than through a literal, and it guards itself at RUNTIME on
/// [`ENABLED`] — but a runtime guard cannot stop a call from being compiled, so without this the
/// whole crate failed to build under `--no-default-features --test` (E0425, "cannot find function
/// `path` in module `super`"). A shipping release build is unchanged: `cfg(test)` is false there,
/// and the fn is gone exactly as before.
#[cfg(any(feature = "devtriggers", test))]
fn path(name: &str) -> std::path::PathBuf {
    crate::paths::in_runtime_dir(&format!("plxnative-{name}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn quality_trigger_accepts_only_persisted_policy_spellings() {
        use crate::plex::session::PlaybackQuality;

        assert_eq!(super::parse_playback_quality("auto"), Some(PlaybackQuality::Auto));
        assert_eq!(
            super::parse_playback_quality("original"),
            Some(PlaybackQuality::Original)
        );
        assert_eq!(
            super::parse_playback_quality("720p_4_mbps"),
            Some(PlaybackQuality::P720)
        );
        for invalid in ["", "Auto", "720p", "unlimited"] {
            assert_eq!(super::parse_playback_quality(invalid), None, "{invalid}");
        }
    }

    /// The DIAG list must name every log the app writes, or that log permanently suppresses the
    /// boot picker. This asserts the property against the paths the code actually opens rather
    /// than against a copy of the list, so adding another log sink without listing it fails here.
    #[test]
    fn diag_names_every_log_this_app_writes() {
        for log in [
            "plxnative-events.log",
            "plxnative-stderr.log",
            "plxnative-crash.log",
            "plxnative-anim.log",
            "plxnative-gputime.jsonl",
            "plxnative-hwcnt.jsonl",
        ] {
            assert!(super::DIAG.contains(&log), "{log} is written by this app but absent from DIAG — it would suppress the boot picker forever");
        }
    }

    /// A DIRECTORY whose name matches the trigger prefix must not read as an armed trigger.
    ///
    /// The failure it prevents is silent and permanent: `any_trigger_present` suppresses the boot
    /// who's-watching picker, so a squatted entry changes which screen this install comes up on
    /// with nothing logged anywhere. It became reachable when two installs started sharing `/tmp`
    /// — the second install's runtime root is a directory sitting right there.
    #[test]
    fn a_directory_is_not_an_armed_trigger() {
        if !super::ENABLED {
            return; // a release build reads nothing
        }
        // Test the exact entry rather than scanning the whole host /tmp. Developers legitimately
        // keep captured TV artifacts there, and their names are intentionally outside DIAG.
        let _g = crate::testlock::serial();
        let d = crate::paths::in_runtime_dir("plxnative-notatrigger");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let entry = std::fs::read_dir(d.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.path() == d)
            .unwrap();
        let armed = super::is_armed_trigger(&entry);
        let _ = std::fs::remove_dir_all(&d);
        assert!(!armed, "a directory named {} read as an armed trigger", d.display());
    }

    /// An empty trigger file and an absent one mean different things to several call sites
    /// (`autoseek` empty = one seek to 140s; `navosc` empty = Home <-> the first library section).
    #[test]
    fn empty_trigger_is_some_not_none() {
        if !super::ENABLED {
            return; // a release build reads nothing; nothing to distinguish
        }
        // Arms a real trigger in the shared runtime root, which is what
        // `a_directory_is_not_an_armed_trigger` scans — they must not overlap.
        let _g = crate::testlock::serial();
        // Write through `path()` itself, NOT a literal and NOT `env::temp_dir()`. The literal was
        // right when the namespace was always `/tmp/plxnative-…`, but it stops meeting the read as
        // soon as an instance root is in effect; `env::temp_dir()` never met it at all, since on
        // the dev Mac that is a per-user `/var/folders/…/T/` path. Going through the same door the
        // code under test uses keeps the write and the read together wherever the root points.
        let p = super::path("devtest-empty");
        std::fs::write(&p, "").unwrap();
        let got = super::read("devtest-empty");
        let _ = std::fs::remove_file(p);
        assert_eq!(got.as_deref(), Some(""), "an empty trigger must not read as absent");
    }

    /// **How an https origin is exercised without a plex.tv account that has one.** The
    /// `plxnative-servers` trigger is the only surface that can inject a server the app did not
    /// discover, so it is also the only way any lane or the integrator can put a TLS origin
    /// through `plex::register_origin` headlessly.
    ///
    /// The default is `http`, because that is what every server this app has ever talked to is and
    /// what every overlay written before this field meant.
    #[test]
    fn a_dev_server_scheme_defaults_to_http_and_can_be_told_https() {
        let one = |json: &str| super::parse_servers(json).expect("parses").pop().expect("one server");

        let plain = one(r#"{"machine_id":"m","host":"10.0.0.2","port":32400,"token":"t"}"#);
        assert_eq!(plain.scheme, crate::plex::Scheme::Http, "an overlay that says nothing means http");
        assert_eq!(plain.origin().expect("dialable").base(), "http://10.0.0.2:32400");

        let tls = one(r#"{"machine_id":"m","host":"nas.hash.plex.direct","port":32400,"token":"t","scheme":"https"}"#);
        assert!(tls.origin().expect("dialable").is_tls());
        assert_eq!(tls.origin().unwrap().base(), "https://nas.hash.plex.direct:32400");
        assert!(tls.usable());

        // A scheme this app does not speak fails the WHOLE trigger, loudly — the caller logs the
        // parse error. Silently meaning `http` would be a run grading the very thing it was armed
        // to test as working.
        assert!(super::parse_servers(r#"{"host":"h","token":"t","scheme":"ftp"}"#).is_err());

        // and the port narrowing still applies: an out-of-range one costs the server, not the run
        let wrapped = one(r#"{"machine_id":"m","host":"10.0.0.2","port":4294999696,"token":"t"}"#);
        assert!(wrapped.origin().is_none() && !wrapped.usable(), "32400 is what that number wraps to");
    }

    /// The wire format the harness writes: a JSON ARRAY of servers, and — because a human arming
    /// this by hand will write one server — a bare OBJECT too.
    #[test]
    fn servers_parse_array_and_single_object() {
        let arr = r#"[{"name":"Mine","machine_id":"aaaa1111bbbb","host":"10.0.0.2","port":32400,
                       "token":"t1"},
                      {"name":"Friend","machine_id":"cccc2222dddd","host":"10.0.0.9","port":32401,
                       "token":"t2"}]"#;
        let v = super::parse_servers(arr).expect("array must parse");
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].host, "10.0.0.9");
        assert_eq!(v[1].port, 32401);
        assert!(v.iter().all(|s| s.usable()));

        let one = r#"{"name":"Friend","host":"10.0.0.9","token":"t2"}"#;
        let v = super::parse_servers(one).expect("a bare object must parse too");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].port, 32400, "port must default to the PMS port, not 0");
    }

    /// The harness's payload, verbatim — `tests/run.py::shared_servers_json` emits exactly this
    /// (compact, an array of one, these SIX keys). It is the only automated link between the two
    /// halves of the mechanism: rename a field on either side and this fails instead of a device
    /// run quietly booting with one server.
    ///
    /// **`handle` is the load-bearing one**, and it is the reason this assertion is worth more than
    /// it looks. `app.rs` derives `owned = handle.is_empty()` from it, which decides whether the
    /// injected server's libraries are pinned to Home and whether they get tab pills of their own
    /// (`browse::tabs`). Drop or rename it on the Python side and every injected server silently
    /// becomes one of YOURS — a friend's libraries pinned and pilled — with nothing else failing.
    #[test]
    fn servers_parse_the_harness_payload_verbatim() {
        let payload = concat!(
            r#"[{"name":"Bob's Plex","machine_id":"friend222","handle":"bob","host":"10.0.0.9","#,
            r#""port":32400,"token":"FRIENDTOK"}]"#
        );
        let v = super::parse_servers(payload).expect("the harness payload must parse");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "Bob's Plex");
        assert_eq!(v[0].machine_id, "friend222");
        assert_eq!(v[0].handle, "bob", "the owner's handle — an empty one means YOUR OWN server");
        assert_eq!(v[0].host, "10.0.0.9");
        assert_eq!(v[0].port, 32400);
        assert!(v[0].usable());

        // …and the wire spelling plex.tv itself uses, so a hand-written file copied straight off a
        // /api/v2/resources row works too
        let wire = r#"[{"sourceTitle":"bob","host":"10.0.0.9","token":"t"}]"#;
        assert_eq!(super::parse_servers(wire).unwrap()[0].handle, "bob");
    }

    /// `address` is what `plex::session::ServerRef` calls the host field, so a file written by
    /// copying one is the likeliest hand-authored shape there is.
    #[test]
    fn servers_accept_address_as_host_alias() {
        let v = super::parse_servers(r#"[{"address":"10.0.0.9","token":"t"}]"#).unwrap();
        assert_eq!(v[0].host, "10.0.0.9");
        assert!(v[0].usable());
    }

    /// Malformed JSON must be an ERROR, never an empty list: a run that injected credentials and
    /// had them silently dropped looks like a broken feature instead of a typo'd overlay.
    #[test]
    fn servers_malformed_is_an_error_not_an_empty_list() {
        assert!(super::parse_servers("{not json").is_err());
        assert!(super::parse_servers("").unwrap().is_empty(), "an empty file = no extra servers");
        assert!(super::parse_servers("[]").unwrap().is_empty());
        // …and the error text goes to the EVENT LOG, so it must not quote the input back. It is a
        // half-written credentials file: the byte after the truncation could be the token.
        // (matched rather than `unwrap_err`, which wants `T: Debug` — the absent derive this type
        // relies on for its no-secret-in-a-log property is load-bearing right here.)
        let e = match super::parse_servers(r#"[{"token":"SECRETTOKENVALUE""#) {
            Err(e) => e,
            Ok(_) => panic!("truncated JSON must not parse"),
        };
        assert!(!e.contains("SECRETTOKENVALUE"), "the parse error echoed the input: {e}");
    }

    /// Half a credential is worse than none — it reaches the server and 401s. The boot log says so
    /// per entry rather than installing it.
    ///
    /// The last entry is the one that is not obviously broken: `app.rs` registers whatever passes
    /// this with `s.port as c_int`, and `4_294_999_696 as i32` is **32400**, so a hand-written
    /// number no port can be would have installed a server pointing at the most ordinary port
    /// there is. Judged by `plex::probe::dial_port`, it is refused like the rest.
    #[test]
    fn servers_incomplete_credentials_are_not_usable() {
        let v = super::parse_servers(
            r#"[{"host":"10.0.0.9"},{"token":"t"},{"host":"10.0.0.9","port":0,"token":"t"},
                {"host":"10.0.0.9","port":4294999696,"token":"t"}]"#,
        )
        .unwrap();
        assert!(v.iter().all(|s| !s.usable()), "no-token / no-host / no-port / a port that wraps");
    }

    /// The one formatter this type has must not be a way for a token to reach the event log.
    #[test]
    fn describe_never_carries_the_token() {
        let v = super::parse_servers(
            r#"[{"name":"Friend","machine_id":"0123456789abcdef","host":"10.0.0.9",
                 "token":"SECRETTOKENVALUE"}]"#,
        )
        .unwrap();
        let d = v[0].describe();
        assert!(!d.contains("SECRETTOKENVALUE"), "describe() leaked the token: {d}");
        assert!(d.contains("10.0.0.9:32400"), "{d}");
        assert!(d.contains("mid=01234567.."), "the machine id is truncated, not dropped: {d}");
    }

    /// …and a SHARED server names nothing at all. The event log is pasted into public issues and PR
    /// bodies — it has already happened, to these exact fields — so a share's owner, machine and
    /// address must not be in it. The token was never the only thing worth protecting here.
    #[test]
    fn describe_redacts_everything_identifying_about_someone_elses_server() {
        let v = super::parse_servers(
            r#"[{"name":"Film Club","machine_id":"0123456789abcdef","host":"10.9.9.7",
                 "port":31234,"token":"SECRETTOKENVALUE","handle":"friend"}]"#,
        )
        .unwrap();
        let d = v[0].describe();
        for leak in ["SECRETTOKENVALUE", "Film Club", "friend", "10.9.9.7", "31234", "0123456789", "01234567"] {
            assert!(!d.contains(leak), "describe() leaked {leak:?}: {d}");
        }
        assert!(d.contains("SHARED"), "a share still says it is one: {d}");
        assert!(d.contains("port_set=true"), "…and that its credentials look complete: {d}");
    }

    /// The correlation tag is stable for one server and different for another — the whole point of
    /// keeping a tag rather than dropping the field. A reader can follow one share across a boot
    /// and tell two shares apart, and learn nothing about either.
    #[test]
    fn the_shared_reference_is_stable_per_machine_and_distinct_across_them() {
        let mk = |mid: &str| {
            super::parse_servers(&format!(
                r#"[{{"machine_id":"{mid}","host":"10.9.9.7","token":"t","handle":"friend"}}]"#
            ))
            .unwrap()[0]
                .describe()
        };
        assert_eq!(mk("aaaaaaaaaaaa"), mk("aaaaaaaaaaaa"), "same machine, same tag");
        assert_ne!(mk("aaaaaaaaaaaa"), mk("bbbbbbbbbbbb"), "two shares must be tellable apart");
    }

    /// The tag is computed in TWO languages: `tests/run.py`'s `server_ref`/`describe_server` print
    /// the same line for the same server, so a harness transcript and an event log can be read as
    /// one story — and so that the harness, which also prints to a pasteable stream, is held to
    /// this module's redaction contract rather than to its own.
    ///
    /// Nothing links the two implementations at build time, so the whole line is pinned to a
    /// literal here. **If this assertion has to change, `run.py`'s copy changes with it.**
    #[test]
    fn the_shared_reference_is_the_same_tag_the_harness_prints() {
        let v = super::parse_servers(
            r#"[{"name":"Film Club","machine_id":"abcd1234efgh","host":"10.9.9.7",
                 "port":31234,"token":"t","handle":"friend"}]"#,
        )
        .unwrap();
        assert_eq!(v[0].describe(), "SHARED ref=71c955 port_set=true");
    }

    /// `plxnative-servers` must NOT be exempt from the picker-suppression scan: it names a host and
    /// carries the token to trust it with, which is automation of the strongest kind. Listing it in
    /// DIAG would let a headless run boot to the who's-watching picker instead of Home.
    #[test]
    fn servers_trigger_is_not_diagnostic() {
        assert!(!super::DIAG.contains(&"plxnative-servers"));
    }

    // ---- plxnative-playurl (the pipeline test tier's one trigger) ----

    /// The payload `tests/run.py` writes, verbatim. Pinned as a literal for `parse_servers`'
    /// reason: nothing links the two languages at build time, so if this has to change, the
    /// harness's writer changes with it.
    #[test]
    fn the_harness_payload_parses_to_the_declaration_it_names() {
        let p = super::parse_playurl(
            r#"{"url":"http://192.0.2.10:8020/pipe_hevc_eac3_4k_dovi_p8.mkv","vcodec":"hevc",
                "acodec":"eac3","fps":23.976,
                "dovi":{"profile":8,"bl_compat":1,"el_present":false},"atmos":false}"#,
        )
        .unwrap();
        assert_eq!(p.url, "http://192.0.2.10:8020/pipe_hevc_eac3_4k_dovi_p8.mkv");
        assert_eq!((p.vcodec.as_str(), p.acodec.as_str()), ("hevc", "eac3"));
        assert!((p.fps - 23.976).abs() < 1e-9);
        assert!(!p.atmos);
        let dv = p.dovi.to_dovi();
        assert!(dv.present, "profile 8 must derive present=true");
        assert_eq!((dv.profile, dv.bl_compat, dv.el_present), (8, 1, false));
    }

    /// The baseline case declares two fields and omits the rest, which must mean "no Dolby
    /// anything" — not a parse failure and not an inherited value.
    #[test]
    fn omitted_fields_default_to_a_silent_declaration() {
        let p = super::parse_playurl(r#"{"url":"http://10.0.0.2:8020/a.mkv","vcodec":"h264","acodec":"ac3"}"#)
            .unwrap();
        assert_eq!(p.fps, 0.0);
        assert!(!p.atmos);
        let dv = p.dovi.to_dovi();
        assert!(!dv.present);
        assert_eq!(dv, crate::metadata::Dovi::NONE, "an absent dovi node must be silence itself");
    }

    /// The synthetic Auto case carries only public fixture coordinates and a declared source
    /// rate. Keep that cross-language seam pinned separately from the ordinary declaration: a
    /// parser that silently defaults either field would play Original forever and make the TV
    /// network-profile case grade the wrong path.
    #[test]
    fn the_auto_fixture_fields_reach_the_runtime_verbatim() {
        let p = super::parse_playurl(
            r#"{"url":"http://192.0.2.10:8020/original.mp4","vcodec":"h264","acodec":"aac",
                "auto_source_kbps":8000,"auto_hls_base":"http://192.0.2.10:8020/__abr"}"#,
        )
        .unwrap();
        assert_eq!(p.auto_source_kbps, 8_000);
        assert_eq!(p.auto_hls_base, "http://192.0.2.10:8020/__abr");
    }

    /// `present` is DERIVED, so it cannot disagree with the profile in either direction.
    #[test]
    fn dovi_presence_follows_the_profile() {
        let none = super::PlayDovi { profile: 0, bl_compat: 1, el_present: true }.to_dovi();
        assert!(!none.present, "profile 0 is not Dolby Vision whatever else is set");
        let p7 = super::PlayDovi { profile: 7, bl_compat: 6, el_present: true }.to_dovi();
        assert!(p7.present);
        assert_eq!((p7.profile, p7.bl_compat, p7.el_present), (7, 6, true));
    }

    /// An empty `url` must be an Err, not an all-defaults object: on `Ok` the engine would take
    /// the empty URL, fall through to `plxnative-url` or a local sample, and PLAY SOMETHING ELSE —
    /// a case grading a stream it was never pointed at. Same class as `parse_servers`' untagged
    /// ordering trap, in different clothes.
    #[test]
    fn an_empty_url_is_refused_rather_than_defaulted() {
        assert!(super::parse_playurl(r#"{"vcodec":"hevc"}"#).is_err());
        assert!(super::parse_playurl(r#"{"url":""}"#).is_err());
        assert!(super::parse_playurl("").is_err());
        assert!(super::parse_playurl("not json at all").is_err());
    }

    /// The trigger decides WHAT THE APP PLAYS. Listing it in DIAG would leave a headless pipeline
    /// run booting to the who's-watching picker — with no session, to the sign-in screen — instead
    /// of into the player.
    #[test]
    fn playurl_trigger_is_not_diagnostic() {
        assert!(!super::DIAG.contains(&"plxnative-playurl"));
    }

    /// The harness writes triggers through a single-quoted `printf` with NO escaping
    /// (`tests/run.py::apply_triggers`), so an apostrophe anywhere in the payload would end the
    /// quoting and hand the rest to the TV's shell. JSON has no apostrophe in its syntax; this
    /// pins that the fields we generate carry none either.
    #[test]
    fn the_harness_payload_carries_no_apostrophe() {
        let payload = r#"{"url":"http://192.0.2.10:8020/pipe_h264_ac3_1080p.mkv","vcodec":"h264","acodec":"ac3","fps":24.0}"#;
        assert!(!payload.contains('\''), "would break apply_triggers' single-quoted printf");
        assert!(super::parse_playurl(payload).is_ok());
    }

    /// **One vocabulary for a rung, in both directions.** `plxnative-quality` and
    /// `plxnative-qualityswitch` accept these strings and `quality: switch → …` prints them, so a
    /// case states its rung once and asserts on the same word. A one-way table would let the log
    /// drift from what the trigger accepts and the drift would show up as a case that arms
    /// correctly and then matches nothing — indistinguishable from the feature not working.
    #[test]
    fn every_quality_wire_name_parses_back_to_itself() {
        use crate::plex::session::PlaybackQuality as Q;
        for q in [Q::Auto, Q::Original, Q::P1080High, Q::P1080, Q::P720, Q::P720Low, Q::P480] {
            let name = super::quality_wire_name(q);
            assert_eq!(super::parse_playback_quality(name), Some(q), "{name} does not round-trip");
        }
    }

    /// The script grammar, including the two ways it must FAIL CLOSED. A typo that resolved to a
    /// default would switch the playback to something the case never asked for — the same hazard
    /// `plxnative-abrpin` documents for its own value.
    #[test]
    fn a_quality_script_fails_closed_and_never_substitutes() {
        use crate::plex::session::PlaybackQuality as Q;
        let parse = super::parse_quality_switch_script;
        assert_eq!(parse("720p_4_mbps"), Some((0, vec![Q::P720])), "one step needs no cadence");
        assert_eq!(
            parse("gap=9000,1080p_8_mbps,auto"),
            Some((9_000, vec![Q::P1080, Q::Auto])),
            "a leading gap is consumed, not treated as a rung",
        );
        assert_eq!(parse("gap=9000,nonsense"), None, "a typo must arm NOTHING, not a default");
        assert_eq!(parse("gap=oops,auto"), None, "an invalid cadence must not become zero");
        assert_eq!(
            parse("720p_4_mbps,auto"),
            None,
            "a multi-step script must state its cadence",
        );
        assert_eq!(parse(""), None);
        assert_eq!(
            parse("nonsense,auto"),
            None,
            "one invalid rung must not leave a valid subset running",
        );
    }
}
