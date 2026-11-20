//! The session of a platform that keeps no credential across launches
//! ([`StoreMode::MemoryOnly`](plx_os::tv::secure::StoreMode)).
//!
//! There, the live read cache ([`CACHE`], what [`peek`] answers) is the single process-local
//! authority: reads, guarded edits, revocation and clear all resolve against it, and no credential
//! store is read or written, not the canonical one, not a sealed or plaintext fallback file and not a
//! revocation marker. A signed-in session ends with the process, and the next launch asks for a
//! sign-in again.
//!
//! What persists is the install's own file ([`install_preferences`], version 2): the client id, the
//! language and the Interface resolution. They hold no credential and cannot authorize a login, and
//! they are what a launch must know before anyone is signed in. Every other preference, per-profile
//! settings included, is forgotten with the process.
//!
//! The three entry points below are the whole mode. `read_live_locked`, `save_locked_with_authority`
//! and `clear_for_erase` each hand over to one of them before they reach any store, so nothing
//! behind those doors is reachable here, and every caller above them is unchanged.
use super::*;
use plx_os::tv::secure::{mode, StoreMode};

/// Whether this process runs without a credential store.
pub(super) fn active() -> bool {
    mode() == StoreMode::MemoryOnly
}

/// What the authority says. While the cell holds a session (a write installed it) that session is
/// the answer. Before the first write, and after a sign-out revoked the cell, the install's own file
/// is read: a client id there reads as a signed-out, first-run record that carries it; none leaves
/// the id to be minted. A credential file next to it is never looked at.
pub(super) fn read_locked() -> ReadState {
    let held = match &*CACHE.lock().unwrap_or_else(|e| e.into_inner()) {
        Cached::Settled(state) | Cached::Transient { state } => Some(state.clone()),
        Cached::Unloaded | Cached::Revoked => None,
    };
    if let Some(state) = held {
        return (*state).clone();
    }
    match install_preferences::read() {
        Some((kept, Some(client_id))) => {
            // The record `prepare_load` builds for a first run, so a launch that reads this file and
            // one that minted its id this run start from the same defaults.
            let mut session = kept.into_session();
            session.prefs.trailer_autoplay = true;
            seed_fresh_quality(&mut session, false, auto_quality_ready());
            session.client_id = client_id;
            ReadState::Ready { session: std::sync::Arc::new(session), plaintext: false, retry_canonical: false }
        }
        Some((kept, None)) => ReadState::Cleared { kept },
        None => ReadState::Missing,
    }
}

/// The write: the cell takes the session, and the install's own choices go to their file. Neither a
/// canonical commit nor a fallback file is attempted, so the outcome is [`LiveWrite::memory`] and
/// never a failure to warn about. The cell is installed as a protected, settled record, because a
/// plaintext one would make `prepare_load` write it out again.
///
/// A routine write over a revoked cell leaves it revoked, exactly as on a durable platform: only a
/// fresh sign-in ends a revocation.
pub(super) fn save_locked(s: &Session, authority: SaveAuthority, generation: u64) -> async_persistence::LiveWrite {
    let install_kept = install_preferences::save_v2(Retained::of(s), &s.client_id);
    // A sign-in writes more than once (discovery, then the final commit): the log says so once, when
    // the cell first takes this account, not once per write.
    let first_for_this_account = authority == SaveAuthority::FreshReauthentication
        && !s.account_token.is_empty()
        && !holds_account(&s.account_token);
    install_write_locked(std::sync::Arc::new(ReadState::Ready {
        session: std::sync::Arc::new(s.clone()),
        plaintext: false,
        retry_canonical: false,
    }), authority, generation);
    if first_for_this_account {
        plx_base::eventlog::log("session: signed in for this run only; this platform keeps no login across launches");
    }
    async_persistence::LiveWrite::memory(install_kept)
}

/// Whether the cell already holds a session of this account.
fn holds_account(account_token: &str) -> bool {
    match &*CACHE.lock().unwrap_or_else(|e| e.into_inner()) {
        Cached::Settled(state) | Cached::Transient { state } => session_from_read(state).account_token == account_token,
        Cached::Unloaded | Cached::Revoked => false,
    }
}

/// Sign-out (`all_local == false`) forgets the session and keeps the install's file, with the
/// choices a sign-out retains. "Delete all local data" removes the file too. Either way the cell
/// is revoked first, so the account and server tokens leave memory before anything else happens,
/// and there is nothing to sweep: no credential was ever written.
///
/// A retry carries the first attempt's confirmed choices, as on a durable platform. It cannot carry
/// the client id, so a retry after a failed first write re-mints one.
pub(super) fn clear_locked(all_local: bool, retry: Option<Retained>) -> Erasure {
    let session = session_from_read(&read_locked());
    let kept = retry.unwrap_or_else(|| Retained::of(&session));
    revoke_cached_session();
    let mut preference_failures = Vec::new();
    let mut retained = kept;
    if all_local {
        preference_failures.extend(install_preferences::erase());
        if preference_failures.is_empty() {
            retained = Retained { language: plx_os::i18n::Preference::System, ui_resolution: UiResolution::Auto };
        }
    } else if !install_preferences::save_v2(kept, &session.client_id) {
        preference_failures.push("install preferences could not be retained".into());
    }
    Erasure { outcome: ClearOutcome::Durable { legacy_swept: true }, retained, preference_failures }
}
