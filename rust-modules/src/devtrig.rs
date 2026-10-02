//! The `/tmp` developer-trigger PRIMITIVES: the one door onto `/tmp/plxnative-*`.
//!
//! This app is driven headlessly by ~44 files under `/tmp/plxnative-*`: which screen to boot to,
//! which item to play, which URL to stream, whether to auto-press OK, which PMS token to use.
//! That is how `tests/run.py` and every capture scene work, and it is not going away. This module
//! is the READ side of that surface — [`flag`], [`read`], [`read_sample`], the per-process latch
//! [`latched_flag!`], and the handful of triggers whose answer is a plain value ([`no_wan`],
//! [`holdload_delay_ms`], [`guard_log_only`]). It names no application type, so every layer can
//! call it. What a trigger DOES once read — the arms that boot a screen, press a key, feed the
//! player a URL — is `crate::dev` and its `scenarios`, which names the app and sits above
//! everything; a typed trigger (a value that needs an upper layer's type to parse into) lives next
//! to the one lower-layer module that consumes it, parsed from [`read`].
//!
//! It must not exist in a public build. `/tmp` is the SHARED system `/tmp` in the production jail
//! too (mode 1777, both jail profiles), so on an ordinary user's TV every one of those files is a
//! behaviour switch any co-resident process can throw. Three are outright takeovers:
//! `plxnative-token` beats the signed-in session (`app.rs`'s boot gate), `plxnative-servers` hands
//! the app a whole additional server — an address AND the token to trust it with — and
//! `plxnative-url` replaces the stream the player feeds.
//!
//! So every read goes through here, and here is `#[cfg]`-gated on the `devtriggers` feature. In a
//! `--no-default-features` build [`flag`] is `false` and [`read`] is `None` at COMPILE time, so
//! no trigger can be armed. Storage and diagnostics still use runtime files; none are developer
//! triggers.
//!
//! That does NOT keep a trigger's NAME out of the binary. A branch behind `flag`/`read` usually
//! folds away, but not reliably: once the answer is carried through a struct field (the
//! `nobudget` flag on `DevFlags`), the optimizer may keep the branch and its string literals
//! in a release build, and `ci/check-package.py` fails the package because it greps the shipped
//! bytes for every trigger name `crate::dev`'s `DIAG` list and [`CONTROLLED`] declare. So every
//! statement whose literal names a trigger (a log line saying `/tmp/plxnative-…`) carries its own
//! `#[cfg(feature = "devtriggers")]`. Never rely on constant folding for this.
//!
//! **Never open a `/tmp` path directly.** The grep that audits this (`/tmp/plxnative-` outside
//! `crate::dev` and the unconditional log sinks, `ci/check-deps.sh`'s `tmppath`) is the only thing
//! keeping the property true. A trigger's bare name is the argument to [`flag`] / [`read`] and the
//! `plxnative-` prefix is added in [`path`], so no literal path exists to find.

/// The triggers a CONTROLLED boot (`app::bootstrap`: the recorder, a replay, an explicit
/// `app-init`) may carry, as bare names. Anything else armed on a recording boot makes its typed
/// initial unsupported, so a replay cannot silently run under a trigger it never modelled.
///
/// One gated table rather than literals at each consumer, for the reason `crate::dev`'s `DIAG` is
/// gated: a full trigger name in the release binary is exactly what `ci/check-package.py` grades as
/// "dev triggers compiled in", and `plxnative-noidle` is its witness. A release build never arms a
/// trigger (`crate::dev::armed_triggers` is empty there), so it has no vocabulary to check against
/// and the accessors below answer "not supported" / "not listed" without naming one.
///
/// `ci/check-package.py` parses this array out of THIS file (and `DIAG` out of `dev.rs`), so it
/// stays one array literal of bare names, and no other text in this file spells its declaration.
#[cfg(any(feature = "devtriggers", test))]
const CONTROLLED: &[&str] = &[
    "rec", "recplay", "focus", "noidle", "token", "app-init", "settings",
    "detail", "detailsec", "detailok", "filmography", "personcredits", "nowan",
];

/// Is the recorded trigger `trigger` (full `plxnative-<name>` form, as `crate::dev::armed_triggers`
/// lists it) one a controlled boot supports? See [`CONTROLLED`].
#[cfg(any(feature = "devtriggers", test))]
pub(crate) fn controlled_trigger(trigger: &str) -> bool {
    trigger.strip_prefix("plxnative-").is_some_and(|name| CONTROLLED.contains(&name))
}
#[cfg(not(any(feature = "devtriggers", test)))]
pub(crate) fn controlled_trigger(_trigger: &str) -> bool {
    false
}

/// Does a recorded trigger list (full names, as `crate::dev::armed_triggers` returns them) carry
/// the trigger `name` (bare)? The typed-initial counterpart of [`flag`]: it reads the list a boot
/// was captured with, never the filesystem. Always `false` in a release build, whose list is empty.
#[cfg(any(feature = "devtriggers", test))]
pub(crate) fn listed(triggers: &[String], name: &str) -> bool {
    triggers.iter().any(|trigger| trigger.strip_prefix("plxnative-") == Some(name))
}
#[cfg(not(any(feature = "devtriggers", test)))]
pub(crate) fn listed(_triggers: &[String], _name: &str) -> bool {
    false
}

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
/// **A `devtrig::flag` is a `stat`, so a trigger read every frame is a syscall on the 60 fps path.**
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
/// crate::devtrig::latched_flag!(
///     /// `/tmp/plxnative-flattabs` — the material off, for an A/B against the flat capsule.
///     fn flat_tabs_armed = "flattabs";
/// );
/// ```
macro_rules! latched_flag {
    ($(#[$m:meta])* $vis:vis fn $name:ident = $trigger:literal;) => {
        $(#[$m])*
        $vis fn $name() -> bool {
            static SEEN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            *SEEN.get_or_init(|| $crate::devtrig::flag($trigger))
        }
    };
}
pub(crate) use latched_flag;

/// The trigger's CONTENT, trimmed. `Some("")` for a trigger armed as an empty file — several
/// distinguish empty (take the default) from a value (`autoseek`, `library`, `marker`), so an
/// empty file must not read the same as an absent one.
#[cfg(feature = "devtriggers")]
pub(crate) fn read(name: &str) -> Option<String> {
    std::fs::read_to_string(path(name))
        .ok()
        .map(|s| s.trim().to_string())
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn read(_name: &str) -> Option<String> {
    None
}

/// Boot-latched main-thread checker escape hatch. File content must be exactly `log`.
#[cfg(feature = "threadcheck")]
pub(crate) fn guard_log_only() -> bool {
    static MODE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| read("guard").as_deref() == Some("log"))
}

/// **`/tmp/plxnative-nowan` — refuse every name lookup, as a dead resolver would.**
///
/// The offline-mode reproduction. A household whose internet is down but whose LAN is up resolves
/// no public name at all: `plex.tv`, `discover.provider.plex.tv` and — the one that matters — the
/// `plex.direct` hostname the app persisted for its OWN server on the LAN. Nothing on a desk can
/// take the router's uplink away deterministically, so this trigger does it inside the app: while
/// armed, [`crate::net`], [`crate::curlio`] and [`crate::stream`] refuse any host that is not a
/// numeric literal, at the point where they would otherwise hand it to a resolver, and return the
/// same error a failed resolution returns. A name reaches the wire only when the request carries a
/// resolve pin ([`crate::plex::ResolvePin`]) — which is exactly what the fix provides, so the same
/// trigger shows the defect red and the fix green with no network condition arranged anywhere.
///
/// Content `slow` first sleeps the connect budget an API call would have spent waiting on a dead
/// resolver ([`crate::net::API`]'s `connect_s`), so a worker that would have stalled stalls here
/// too. Empty is the fast variant. Latched at first read like every per-frame trigger, and `None`
/// at COMPILE time without `devtriggers`, so a public binary carries no such switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NoWan {
    pub(crate) slow: bool,
}

pub(crate) fn no_wan() -> Option<NoWan> {
    static SEEN: std::sync::OnceLock<Option<NoWan>> = std::sync::OnceLock::new();
    *SEEN.get_or_init(|| {
        read("nowan").map(|s| NoWan {
            slow: s.trim() == "slow",
        })
    })
}

/// **Hold the Load-returned flag** — `plxnative-holdload[=ms]`.
///
/// `Some(ms)` when armed (default 30000 for a bare/empty trigger, per its own `parse` fallback),
/// else `None`. `player::threads::load_thread` sleeps this many milliseconds right after the real
/// `sf_load` call returns and BEFORE `Shared::mark_native_load_returned` publishes that fact — so
/// issue #74 D.1's budget (the pump's `deferring` line, then, past `NATIVE_LOAD_BUDGET`, the
/// failure read-out) becomes observable on a real television on demand, rather than only on a
/// k5lp set that happens to hang there for real. Deliberately NOT `DIAG`: it changes playback
/// behaviour (a real Load attempt now waits), so arming it must suppress the who's-watching
/// picker like every other automation trigger.
#[cfg(feature = "devtriggers")]
pub(crate) fn holdload_delay_ms() -> Option<u64> {
    let raw = read("holdload")?;
    Some(if raw.is_empty() {
        30_000
    } else {
        raw.parse().unwrap_or(30_000)
    })
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn holdload_delay_ms() -> Option<u64> {
    None
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
///
/// `pub(crate)` so `crate::dev::scenarios`, which hands a trigger's PATH (not its content) to the
/// recorder's own parser, goes through the same door instead of re-spelling the prefix.
#[cfg(any(feature = "devtriggers", test))]
pub(crate) fn path(name: &str) -> std::path::PathBuf {
    crate::paths::in_runtime_dir(&format!("plxnative-{name}"))
}

#[cfg(test)]
mod tests {
    /// An empty trigger file and an absent one mean different things to several call sites
    /// (`autoseek` empty = one seek to 140s; `navosc` empty = Home <-> the first library section).
    #[test]
    fn empty_trigger_is_some_not_none() {
        if !super::ENABLED {
            return; // a release build reads nothing; nothing to distinguish
        }
        // Arms a real trigger in the shared runtime root, which is what
        // `crate::dev`'s `a_directory_is_not_an_armed_trigger` scans — they must not overlap.
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
        assert_eq!(
            got.as_deref(),
            Some(""),
            "an empty trigger must not read as absent"
        );
    }
}
