//! Profile-scoped recent Search terms, independent of text measurement and rendering.
//! Session reads are cached by profile generation; writes use the session's atomic worker door.
//! Query/server resets do not erase history. Snapshots retain their original profile's terms.
use std::sync::{Arc, Mutex};

/// Current Search layout admits four terms above the raised keyboard; the UI derives its cap.
pub(crate) const CAP: usize = 4;

#[derive(Clone, Default)]
pub(crate) struct RecentsSnapshot {
    generation: u32,
    terms: Arc<Vec<String>>,
}
impl RecentsSnapshot {
    #[cfg(test)]
    pub(crate) fn fixture(generation: u32, terms: Vec<String>) -> Self {
        Self { generation, terms: Arc::new(sanitize(terms)) }
    }
    pub(crate) fn terms(&self) -> &[String] { &self.terms }
    pub(crate) fn generation(&self) -> u32 { self.generation }
    pub(crate) fn same_publication(&self, other: &Self) -> bool {
        self.generation == other.generation && Arc::ptr_eq(&self.terms, &other.terms)
    }
}

struct Store {
    generation: u32,
    who: String,
    account: Account,
    terms: Arc<Vec<String>>,
}
static STORE: Mutex<Option<Store>> = Mutex::new(None);

fn with_store<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    let mut guard = STORE.lock().unwrap_or_else(|e| e.into_inner());
    let generation = crate::plex::session::current_gen();
    if guard.as_ref().map(|s| s.generation) != Some(generation) {
        let who = crate::plex::session::current_profile_key();
        // Copy before reading disk: a worker may drain this entry while peek waits for IO.
        // Taking PENDING across peek would invert flush's IO -> PENDING lock order.
        let pending = PENDING.lock().unwrap_or_else(|e| e.into_inner())
            .iter().find(|p| p.who == who).cloned();
        let session = crate::plex::session::peek();
        let account = Account::of(&session);
        let terms = pending.filter(|p| p.account == account).map(|p| p.terms)
            .unwrap_or_else(|| sanitize(session.recents_for(&who).to_vec()));
        *guard = Some(Store { generation, who, account, terms: Arc::new(terms) });
    }
    f(guard.as_mut().expect("profile cache is populated"))
}

/// Capture before screen processing. Borrowed terms thereafter perform no file or global read.
pub(crate) fn snapshot() -> RecentsSnapshot {
    with_store(|s| RecentsSnapshot { generation: s.generation, terms: s.terms.clone() })
}

/// Update data and capture the persistence payload under the same profile-cache lock.
/// Unchanged edits keep the same Arc and neither spawn a worker nor invalidate the frame.
fn edit(profile_generation: u32, change: impl FnOnce(&[String]) -> Option<Vec<String>>, submit: impl FnOnce()) -> bool {
    let mut retry_drain = false;
    let changed = with_store(|s| {
        if s.generation != profile_generation { return false; }
        let Some(terms) = change(&s.terms) else { return false };
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|p| p.account == s.account);
        let slot = pending.iter().position(|p| p.who == s.who);
        if slot.is_none() && pending.len() == PENDING_CAP {
            crate::log("search: recent-history queue full; edit refused");
            retry_drain = true;
            return false;
        }
        let payload = Pending { account: s.account.clone(), who: s.who.clone(), terms: terms.clone() };
        if let Some(i) = slot { pending[i] = payload; } else { pending.push(payload); }
        s.terms = Arc::new(terms);
        true
    });
    if changed || retry_drain { submit(); }
    if changed { crate::ui::idle::invalidate(); }
    changed
}

pub(crate) fn remember(profile_generation: u32, term: &str) -> bool {
    remember_with(profile_generation, term, submit)
}

fn submit() { let _ = crate::task::spawn_small("recents-save", flush); }

fn remember_with(profile_generation: u32, term: &str, submit: impl FnOnce()) -> bool {
    let term = term.trim();
    if !usable(term) { return false; }
    edit(profile_generation, |old| {
        if old.first().is_some_and(|first| first == term) { return None; }
        let mut next = old.to_vec();
        promote(&mut next, term);
        Some(next)
    }, submit)
}

pub(crate) fn clear(profile_generation: u32) -> bool {
    clear_with(profile_generation, submit)
}

fn clear_with(profile_generation: u32, submit: impl FnOnce()) -> bool {
    edit(profile_generation, |old| if old.is_empty() { None } else { Some(Vec::new()) }, submit)
}

/// What the store will hold, whatever the file said. `de_soft_vec` guarantees each entry is a
/// `String` and nothing more, so a hand-edited file can still hand us blanks, whitespace, repeats
/// or a hundred of them.
///
/// Deliberately not a fold of [`promote`], which inserts at the FRONT: replaying a
/// newest-first file through it would build the list backwards, and the [`CAP`] would then drop
/// the newest terms instead of the oldest. This walks the file in its own order and keeps the
/// FIRST spelling of each term, which for a newest-first list is the most recent one.
fn sanitize(raw: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in raw {
        let t = t.trim();
        if !usable(t) {
            continue;
        }
        let key = t.to_lowercase();
        if out.iter().any(|s| s.to_lowercase() == key) {
            continue;
        }
        out.push(t.to_string());
        if out.len() == CAP {
            break;
        }
    }
    out
}

/// Can this ALREADY-TRIMMED term be stored and drawn? Blank is not a search. An interior NUL is
/// the non-obvious half: `de_soft_vec` accepts it (it is a valid `String`) and trimming and
/// de-duplication both survive it, but `CString::new` refuses it, so the row's label would be
/// skipped and a focused term would draw as a **full-width accent pill with nothing in it** — a
/// control the user can move onto and press with no way to tell what it is. This module's stated
/// job is to re-impose its own invariants on the way in, and drawability is one of them.
fn usable(trimmed: &str) -> bool {
    !trimmed.is_empty() && !trimmed.contains('\0')
}

/// The pure list operation behind [`remember`]: `term` becomes the most recent, an existing spelling
/// of it is REMOVED rather than duplicated, and the oldest fall off the end at [`CAP`].
///
/// Case-insensitive by `to_lowercase`, not `eq_ignore_ascii_case`: the libraries measured here are
/// Cyrillic, and a term is whatever the user typed. The NEW spelling is what is kept — you get back
/// the words you just searched, capitalised the way you just wrote them.
///
/// A term that is not [`usable`] is dropped.
fn promote(list: &mut Vec<String>, term: &str) {
    let t = term.trim();
    if !usable(t) {
        return;
    }
    let key = t.to_lowercase();
    list.retain(|s| s.to_lowercase() != key);
    list.insert(0, t.to_string());
    list.truncate(CAP);
}


// Resource ceiling, not a Plex profile limit. Coalesce per profile, never evict another
// profile's accepted edit. If workers cannot drain 64 distinct profiles, refuse new entries.
const PENDING_CAP: usize = 64;
static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());

// Private persistence guard, never exposed in a view, log or replay payload. The installation
// client_id alone cannot distinguish accounts; account_token survives Home profile switches.
#[derive(Clone, PartialEq, Eq)]
struct Account { client_id: String, token: String }
impl Account {
    fn of(s: &crate::plex::session::Session) -> Self {
        Self { client_id: s.client_id.clone(), token: s.account_token.clone() }
    }
}
#[derive(Clone)]
struct Pending { account: Account, who: String, terms: Vec<String> }

/// The session to WRITE `terms` for `who`, or `None` when there is nothing to write.
///
/// Pure — `who` is captured with the edit, never read from the active profile on the worker —
/// and split out from [`flush`] so both refusals are host-testable. The second is the single line
/// standing between a search term and a wiped credentials file, and it is invisible to every other
/// test in the suite.
///
/// **Never write a session we could not READ.** `peek` hands back a default `Session` both for "no
/// file yet" and for "the file did not parse", and saving that would truncate a live one — a
/// silent sign-out, caused by a search term. `client_id` is minted once by `session::load` on the
/// boot path and is never empty afterwards, so it is exactly the test for "something real came
/// back": with no readable session the terms stay in memory for this run and are dropped with it.
/// `session::update` refuses the same case one layer up, for every caller rather than this one;
/// the test stays here because this is where it is *graded*, and because a rule worth having in
/// two places is one whose cost is a string comparison.
fn merged(
    s: &crate::plex::session::Session,
    who: &str,
    terms: &[String],
) -> Option<crate::plex::session::Session> {
    if s.client_id.is_empty() || s.recents_for(who) == terms {
        return None;
    }
    // `set_recents_for`, never a struct update with `recent_searches:` — the field now holds EVERY
    // profile's history, so assigning it here would drop everyone else's. That is the whole reason
    // the setter exists rather than the field being written at this call site.
    let mut next = s.clone();
    next.set_recents_for(who, terms.to_vec());
    Some(next)
}


fn flush() {
    crate::plex::session::update(|s| {
        let pending = std::mem::take(&mut *PENDING.lock().unwrap_or_else(|e| e.into_inner()));
        let account = Account::of(s);
        let mut next = None;
        for p in pending.into_iter().filter(|p| p.account == account) {
            if let Some(updated) = merged(next.as_ref().unwrap_or(s), &p.who, &p.terms) {
                next = Some(updated);
            }
        }
        next
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ClearCaches;
    impl Drop for ClearCaches {
        fn drop(&mut self) {
            *STORE.lock().unwrap_or_else(|e| e.into_inner()) = None;
            PENDING.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
    }

    #[test]
    fn queue_capacity_refuses_new_profiles_without_evicting_accepted_edits() {
        let _guard = crate::testlock::serial();
        let session = crate::plex::session::TempSession::new("recents-pending-cap");
        let _caches = ClearCaches;
        session.watching("overflow");
        let before = snapshot();
        let account = Account::of(&crate::plex::session::peek());
        *PENDING.lock().unwrap() = (0..PENDING_CAP).map(|i| Pending {
            account: account.clone(), who: format!("queued-{i}"), terms: vec!["accepted".into()],
        }).collect();
        let mut retries = 0;
        assert!(!remember_with(before.generation(), "overflow", || retries += 1));
        assert_eq!(retries, 1, "a previously refused worker can be retried without inline IO");
        assert!(before.same_publication(&snapshot()));
        assert_eq!(PENDING.lock().unwrap().len(), PENDING_CAP);
        // Existing profiles still coalesce even at capacity.
        session.watching("queued-0");
        assert!(remember_with(snapshot().generation(), "newest", || {}));
        assert_eq!(PENDING.lock().unwrap().len(), PENDING_CAP);
        flush();
        let saved = crate::plex::session::peek();
        assert_eq!(saved.recents_for("queued-0"), &["newest", "accepted"]);
        for i in 1..PENDING_CAP {
            assert_eq!(saved.recents_for(&format!("queued-{i}")), &["accepted"]);
        }
        assert!(saved.recents_for("overflow").is_empty());
        session.watching("overflow");
        assert!(remember_with(snapshot().generation(), "now admitted", || {}));
        flush();
        assert_eq!(crate::plex::session::peek().recents_for("overflow"), &["now admitted"]);
    }

    #[test]
    fn pending_writes_preserve_each_profiles_latest_committed_history() {
        let _guard = crate::testlock::serial();
        let session = crate::plex::session::TempSession::new("recents-pending-profiles");
        let _caches = ClearCaches;
        session.watching("pending-a");
        assert!(remember_with(snapshot().generation(), "alpha", || {}));
        session.watching("pending-b");
        assert!(remember_with(snapshot().generation(), "beta", || {}));
        session.watching("pending-a");
        assert_eq!(snapshot().terms(), &["alpha"], "return before the worker drains preserves history");
        assert!(remember_with(snapshot().generation(), "alpha latest", || {}));
        flush();
        let saved = crate::plex::session::peek();
        assert_eq!(saved.recents_for("pending-a"), &["alpha latest", "alpha"], "B cannot replace A's queued write");
        assert_eq!(saved.recents_for("pending-b"), &["beta"]);
    }

    #[test]
    fn pending_writes_cannot_cross_an_account_replacement() {
        let _guard = crate::testlock::serial();
        let session = crate::plex::session::TempSession::new("recents-pending-account");
        let _caches = ClearCaches;
        crate::plex::session::update(|s| {
            let mut next = s.clone();
            next.account_token = "synthetic-old-account".into();
            Some(next)
        });
        session.watching("old-profile");
        assert!(remember_with(snapshot().generation(), "old history", || {}));
        let old = crate::plex::session::peek();
        crate::plex::session::save(&crate::plex::session::Session {
            client_id: old.client_id,
            account_token: "synthetic-new-account".into(),
            ..Default::default()
        });
        // Even a matching profile key (notably the empty owner key) grants no authority.
        session.watching("old-profile");
        assert!(snapshot().terms().is_empty(), "pending old-account terms must not reappear in a view");
        flush();
        assert!(crate::plex::session::peek().recents_for("old-profile").is_empty(),
            "a valid replacement session is not authority to persist the departing account's terms");
        assert!(remember_with(snapshot().generation(), "new account history", || {}));
        flush();
        assert_eq!(crate::plex::session::peek().recents_for("old-profile"), &["new account history"]);
    }

    #[test]
    fn retained_terms_survive_edits_and_noops_do_not_republish_or_submit() {
        let _guard = crate::testlock::serial();
        let session = crate::plex::session::TempSession::new("recents-publication");
        let _caches = ClearCaches;
        session.watching("recent-a");
        let empty = snapshot();
        assert!(empty.terms().is_empty());
        let mut submissions = 0;
        assert!(remember_with(empty.generation(), " alpha ", || submissions += 1));
        let first = snapshot();
        assert_eq!(first.terms(), &["alpha"]);
        assert!(empty.terms().is_empty());
        assert!(first.same_publication(&snapshot()));
        for same in ["alpha", " alpha ", "", "bad\0term"] {
            assert!(!remember_with(first.generation(), same, || panic!("no-op submitted a write")));
            assert!(first.same_publication(&snapshot()));
        }
        let outer = crate::search::view::snapshot();
        assert!(remember_with(first.generation(), "beta", || submissions += 1));
        assert_eq!(first.terms(), &["alpha"]);
        assert_eq!(snapshot().terms(), &["beta", "alpha"]);
        let changed = crate::search::view::snapshot();
        assert_eq!(outer.view().query_gen(), changed.view().query_gen());
        assert!(!outer.same_publication(&changed), "recents change independently of query results");
        assert_eq!(outer.view().recents().terms(), &["alpha"]);
        assert!(clear_with(first.generation(), || submissions += 1));
        let cleared = snapshot();
        assert!(cleared.terms().is_empty());
        assert!(!clear_with(cleared.generation(), || panic!("empty clear submitted a write")));
        assert!(cleared.same_publication(&snapshot()));
        assert_eq!(submissions, 3);
        flush();
        assert!(crate::plex::session::peek().recents_for("recent-a").is_empty());
    }

    #[test]
    fn profile_snapshots_and_persisted_lists_stay_separate_across_switches() {
        let _guard = crate::testlock::serial();
        let session = crate::plex::session::TempSession::new("recents-profiles");
        let _caches = ClearCaches;
        crate::plex::session::update(|s| {
            let mut next = s.clone();
            next.set_recents_for("recent-a", vec!["alpha".into()]);
            next.set_recents_for("recent-b", vec!["beta".into()]);
            Some(next)
        });
        session.watching("recent-a");
        let a = snapshot();
        assert_eq!(a.terms(), &["alpha"]);
        assert!(remember_with(a.generation(), "new-a", || {}));
        // The queued payload still names A when the worker runs after a profile switch.
        session.watching("recent-b");
        flush();
        let b = snapshot();
        assert_ne!(a.generation(), b.generation());
        assert_eq!(a.terms(), &["alpha"]);
        assert_eq!(b.terms(), &["beta"]);
        assert!(!remember_with(a.generation(), "stale-a", || panic!("stale profile write submitted")));
        assert!(!clear_with(a.generation(), || panic!("stale profile clear submitted")));
        assert!(b.same_publication(&snapshot()), "stale commands leave the new profile untouched");
        assert!(clear_with(b.generation(), || {}));
        flush();
        let saved = crate::plex::session::peek();
        assert_eq!(saved.recents_for("recent-a"), &["new-a", "alpha"]);
        assert!(saved.recents_for("recent-b").is_empty());
        assert_eq!(b.terms(), &["beta"]);
        session.watching("recent-a");
        let restored = snapshot();
        assert_eq!(restored.terms(), &["new-a", "alpha"]);
        crate::search::reset();
        assert!(restored.same_publication(&snapshot()), "query/server reset does not erase history");
    }

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The whole point of `remember`: searching something you have searched before REORDERS the
    /// list, it does not lengthen it — and the spelling you just typed is the one you get back.
    #[test]
    fn remembering_a_term_moves_it_to_the_front_instead_of_duplicating_it() {
        let mut l = list(&["wallace", "laura"]);
        promote(&mut l, "laura");
        assert_eq!(
            l,
            list(&["laura", "wallace"]),
            "an existing term is moved, not added"
        );

        // a different CASE is the same term — the new spelling wins
        promote(&mut l, "WALLACE");
        assert_eq!(l, list(&["WALLACE", "laura"]));

        // …and so is one the user typed with stray whitespace around it
        promote(&mut l, "  laura  ");
        assert_eq!(l, list(&["laura", "WALLACE"]));

        // a blank is not a search, and neither is anything undrawable
        for junk in ["", "   ", "\t\n", "wal\0lace"] {
            promote(&mut l, junk);
            assert_eq!(
                l,
                list(&["laura", "WALLACE"]),
                "{junk:?} must not enter the list"
            );
        }
    }

    /// A term carrying an interior NUL is not storable, because it is not DRAWABLE: `CString::new`
    /// refuses it, the row's label is skipped, and a focused term becomes a full-width accent pill
    /// with nothing in it. `de_soft_vec` cannot catch this — it is a perfectly good `String`.
    #[test]
    fn an_undrawable_term_never_reaches_the_store() {
        assert!(usable("wallace"));
        assert!(!usable("") && !usable("wal\0lace") && !usable("\0"));
        assert_eq!(sanitize(list(&["wal\0lace", "gromit"])), list(&["gromit"]));
    }

    /// The one line between a search term and a wiped credentials file. Both refusals matter, and
    /// neither is visible to any other test in the suite — delete the `client_id` guard and 542
    /// tests still pass.
    #[test]
    fn a_session_that_could_not_be_read_is_never_written_back() {
        use crate::plex::session::Session;
        let terms = list(&["wallace"]);

        // `peek` hands back a DEFAULT session both for "no file yet" and for "the file did not
        // parse" — writing that would truncate a live one, i.e. sign the device out over a search.
        assert!(
            merged(&Session::default(), "uu-1", &terms).is_none(),
            "an unreadable session is never written"
        );

        let live = Session {
            client_id: "cid-1".into(),
            account_token: "acct".into(),
            ..Default::default()
        };
        let next = merged(&live, "uu-1", &terms).expect("a real session takes the terms");
        assert_eq!(next.recents_for("uu-1"), terms);
        assert_eq!(
            next.account_token, "acct",
            "everything else in the file is carried over untouched"
        );

        // and an unchanged list is not a write: the worker re-reads the file on every flush
        assert!(
            merged(&next, "uu-1", &terms).is_none(),
            "no change, no write"
        );
    }

    /// The profile key is an ARGUMENT, not a global read — which is what makes the write safe to do
    /// on a worker. `persist` captures it on the SDL thread at commit time, so a profile switch
    /// landing between the commit and the write cannot file one person's terms under the next
    /// person's key, and cannot touch the list already stored for anybody else.
    #[test]
    fn terms_are_written_under_the_profile_that_searched_them() {
        use crate::plex::session::Session;
        let live = Session {
            client_id: "cid-1".into(),
            ..Default::default()
        };

        let a = merged(&live, "uu-a", &list(&["wallace"])).expect("a's terms land");
        let b = merged(&a, "uu-b", &list(&["gromit"])).expect("b's terms land beside them");
        assert_eq!(
            b.recents_for("uu-a"),
            list(&["wallace"]),
            "the other profile's list is untouched"
        );
        assert_eq!(b.recents_for("uu-b"), list(&["gromit"]));

        // the same terms under a DIFFERENT key are still a change — the guard compares this
        // profile's stored list, never the file as a whole
        assert!(
            merged(&b, "uu-c", &list(&["gromit"])).is_some(),
            "a third profile gets its own entry"
        );
        assert!(
            merged(&b, "uu-b", &list(&["gromit"])).is_none(),
            "…but the same profile's is a no-op"
        );
    }

    /// The cap drops the OLDEST, which is the only end that can be dropped without contradicting
    /// "most recent first".
    ///
    /// Written against [`CAP`] rather than against a literal count — the cap is a LAYOUT answer
    /// (see the clearance test), and it moved the day the keyboard was measured. A test that spelt
    /// the number would have failed for a correct change and taught nothing about the rule.
    #[test]
    fn the_cap_drops_the_oldest_term() {
        // One more term than fits, newest last, so the survivors are the reverse of the tail.
        let typed: Vec<String> = (0..CAP + 1).map(|i| format!("q{i}")).collect();
        let mut l = Vec::new();
        for t in &typed {
            promote(&mut l, t);
        }
        let want: Vec<String> = typed[1..].iter().rev().cloned().collect();
        assert_eq!(l, want, "the oldest fell off, order is newest-first");
        assert_eq!(l.len(), CAP);
    }

    /// A file is not a promise. `de_soft_vec` guarantees every entry is a `String` and nothing
    /// else, so the store re-imposes its own invariants on read — order preserved, blanks and
    /// repeats gone, length bounded.
    #[test]
    fn a_hand_edited_list_is_cleaned_up_on_the_way_in() {
        let raw = list(&[
            "laura",
            "",
            "  ",
            "LAURA",
            "wallace",
            "gromit",
            "feathers",
            "wendolene",
            "grue",
        ]);
        let got = sanitize(raw);
        // The three rules, stated separately from the LENGTH so the cap can move on its own (see
        // `the_cap_drops_the_oldest_term`): order preserved, blanks gone, the repeat collapsed onto
        // its FIRST place, and whatever survives is bounded.
        let kept = [
            "laura",
            "wallace",
            "gromit",
            "feathers",
            "wendolene",
            "grue",
        ];
        assert_eq!(
            got,
            list(&kept[..CAP.min(kept.len())]),
            "newest-first order kept, blanks dropped, the repeat collapsed onto its FIRST place"
        );
        assert!(got.len() <= CAP);
        assert!(
            !got.iter().any(|t| t.trim().is_empty()),
            "a blank is not a term"
        );
        assert_eq!(sanitize(Vec::new()), Vec::<String>::new());
    }

}
