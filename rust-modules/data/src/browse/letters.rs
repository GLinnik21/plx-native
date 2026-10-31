//! The A-Z rail's data: the first-letter list of a title sort, the band counts of a value sort, and
//! what each counted.
//!
//! The rail jumps to a letter by the running sum of the counts before it, so the counts have to
//! be those of the very listing the grid shows: the same `type=`, the same Unwatched key
//! (`unwatched` or `unwatchedLeaves`) and the same genre, sorted by title. The letters read
//! therefore carries the terms of the grid's own page read ([`SecState::query_filters`], the one
//! source of both), and a landed list is KEYED by what it counted ([`LettersKey`]).
//!
//! A list is shown only while its key is the one the section's present query would ask with
//! ([`SecState::shown_letters`]), and that is checked where the letters are READ, not at the
//! edit sites: the first page's landing applies a saved genre after the letters were asked
//! (`pump_owned_with_gate`), and nothing there has to remember to forget them. Two things move the
//! key without a term changing by bumping [`LettersKey::gen`]: the server's membership changed
//! under the section ([`SecState::membership_changed`] and the stale flag spent by
//! [`SecState::reset_listing`]), which every key carries; and a title of the section's server had
//! its watched state read back ([`SecState::viewing_changed`]), which only a key whose counts move
//! with a watch carries ([`SecState::key_gen`]): one counted under Unwatched, or a bands family a
//! watch changes (plays, unplayed; `BandFamily::moves_with_viewing`). Nothing else bumps it; a third
//! thing moves a key by a FIELD of it, not by `gen`: the day turning, for a calendar family
//! ([`LettersKey::today`], below). A library
//! TYPE change forgets the list outright ([`SecState::forget_letters`]); the
//! Collections page's loan does not carry it.
//!
//! A VALUE sort's rail is the same shape with another payload: one kept slot of band counts
//! ([`SecState::shown_bands`]) keyed by the same terms and the sort's [`BandFamily`], so [`LettersKey`]
//! carries the family (`None` for the letters) and, for a calendar family, the day it was counted on
//! (`today`, a field of the key and no `gen`: the bands are that day's, so a day turn is a new key and
//! asks once more, and a fixed ladder's key never carries it). While that read is out the rail keeps
//! showing the previous day's bands, when the day is all that differs ([`SecState::shown_bands`]),
//! whichever of the two slots holds them (a round trip through another sort or the Unwatched switch
//! leaves them in the kept one): a focus seated in the rail must not lose it to a second of nothing.
//! The direction is deliberately NOT in the key: the counts are of values, and a landed [`BandRail`]
//! holds the layout of both directions, so a flip of the direction asks the server nothing. A family
//! that a watch moves (`BandFamily::moves_with_viewing`) takes the viewing generation as an Unwatched
//! key does.
//! A read whose counts were not trusted lands as a list with no rail, not as a failure
//! ([`SecState::land_bands`]): over a listing that held still the answer would not change on a retry,
//! so only a moved key asks again (a listing that moved under the reads is a failure, and is asked
//! again after the back-off).
//!
//! A read that FAILED leaves the list as it was (not current, so hidden: never shown over a grid it
//! does not describe) and holds the same key back for [`LETTERS_RETRY_STEPS`] steps, as the page
//! lane's `retry_cd` does; any edit asks at once.

use super::bands::{BandFamily, BandRail, Day};
use super::{Arc, BrowseState, DirectoryResult, SecKind, SecState};

/// What a first-letter list (or a value sort's band counts) counted: the grid's own query as it stood
/// when the list was asked. Compared field by field wherever the list is read ([`LettersKey::is`]),
/// without allocating.
///
/// There is deliberately no listing type here: the landing refuses an answer whose
/// `DirectoryResult::library_type` is not the section's, and a type change forgets the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LettersKey {
    /// The band family the counts are of; `None` for the title letters.
    pub(super) family: Option<BandFamily>,
    /// Today's date for a calendar family (its edges and its bands are this day's), `None` for every
    /// other key: a day turn is a new key and asks once more.
    pub(super) today: Option<Day>,
    /// The Unwatched switch AS APPLIED (false while a type the filters do not apply to is listed).
    pub(super) unwatched: bool,
    /// The genre tag id AS APPLIED.
    pub(super) genre: Option<String>,
    /// [`SecState::key_gen`] when asked: moves when the server's membership changed under the
    /// section, or (a key [`Self::viewing_sensitive`]) a title's watched state was read back, which
    /// no term shows.
    pub(super) gen: u32,
}

impl LettersKey {
    fn is(&self, family: Option<BandFamily>, today: Option<Day>, unwatched: bool, genre: Option<&str>, gen: u32) -> bool {
        self.family == family && self.today == today && self.unwatched == unwatched
            && self.genre.as_deref() == genre && self.gen == gen
    }

    /// Do the counts move with every watch? Those counted under Unwatched, and a family whose
    /// values a watch changes.
    fn viewing_sensitive(&self) -> bool {
        sensitive(self.family, self.unwatched)
    }

    /// Is this the count [`Self::is`] describes, asked on ANOTHER day: the same family, terms and
    /// generation, and a calendar family's `today` the only difference?
    fn is_but_for_the_day(&self, family: Option<BandFamily>, today: Option<Day>, unwatched: bool, genre: Option<&str>, gen: u32) -> bool {
        self.today.is_some() && today.is_some() && self.today != today && self.is(family, self.today, unwatched, genre, gen)
    }

    /// [`Self::is_but_for_the_day`] against another key.
    fn of_another_day_than(&self, other: &LettersKey) -> bool {
        self.is_but_for_the_day(other.family, other.today, other.unwatched, other.genre.as_deref(), other.gen)
    }
}

/// Does a key counted for `family` under `unwatched` move with a watch ([`SecState::key_gen`])?
fn sensitive(family: Option<BandFamily>, unwatched: bool) -> bool {
    unwatched || family.is_some_and(BandFamily::moves_with_viewing)
}

/// Steps a failed letters read holds the same key back (the page lane's `retry_cd` is 120 too).
pub(super) const LETTERS_RETRY_STEPS: u32 = 120;

impl SecState {
    /// The terms the section's query applies now: Unwatched and the genre's tag id, both `off` while a
    /// type the filters do not apply to is listed ([`super::LibraryType::filters`]).
    fn applied_terms(&self) -> (bool, Option<&str>) {
        if !self.library_type.filters() {
            return (false, None);
        }
        (self.unwatched, self.genre.as_ref().map(|genre| genre.id.as_str()))
    }

    /// The generation a key carries: the membership's, and for a key that moves with a watch
    /// (`viewing`: Unwatched, or a family a watch changes) the viewing one as well. Both only count
    /// up, so the sum moves whenever either does, and an unfiltered or genre-only list is left alone
    /// by a watch.
    fn key_gen(&self, viewing: bool) -> u32 {
        if viewing { self.letters_gen.wrapping_add(self.viewing_gen) } else { self.letters_gen }
    }

    /// The key a letters read asked NOW would carry.
    pub(super) fn letters_key(&self) -> LettersKey {
        let (unwatched, genre) = self.applied_terms();
        LettersKey { family: None, today: None, unwatched, genre: genre.map(str::to_owned), gen: self.key_gen(unwatched) }
    }

    /// The day a key for `family` is of: today for a calendar family, nothing for a fixed ladder (its
    /// bands do not move with the date).
    fn key_today(&self, family: BandFamily) -> Option<Day> {
        family.basis().and(self.today)
    }

    /// The key a bands read for `family` asked NOW would carry.
    pub(super) fn bands_key(&self, family: BandFamily) -> LettersKey {
        let (unwatched, genre) = self.applied_terms();
        LettersKey {
            family: Some(family), today: self.key_today(family), unwatched, genre: genre.map(str::to_owned),
            gen: self.key_gen(sensitive(Some(family), unwatched)),
        }
    }

    /// The list that describes the listing the grid shows now, if one is on hand: the landed one,
    /// or the one it replaced ([`Self::land_letters`]). Runs every Library step and every snapshot,
    /// so it compares in place, and it is the one place a list is picked: every reader goes
    /// through it, so nothing has to remember to swap a list back.
    pub(super) fn shown_letters(&self) -> Option<&Arc<Vec<(String, i64)>>> {
        let (unwatched, genre) = self.applied_terms();
        let gen = self.key_gen(unwatched);
        if self.letters_for.as_ref().is_some_and(|key| key.is(None, None, unwatched, genre, gen)) {
            return Some(&self.letters);
        }
        self.letters_kept
            .as_ref()
            .filter(|(key, _)| key.is(None, None, unwatched, genre, gen))
            .map(|(_, list)| list)
    }

    /// Does a list on hand describe the listing the grid shows now?
    pub(super) fn letters_current(&self) -> bool {
        self.shown_letters().is_some()
    }

    /// Is a letters read owed: the list is not current, and a failed read of THIS key is not
    /// holding the next one back?
    pub(super) fn letters_owed(&self) -> bool {
        if self.letters_current() {
            return false;
        }
        let (unwatched, genre) = self.applied_terms();
        let gen = self.key_gen(unwatched);
        !self.letters_backoff.as_ref().is_some_and(|(key, steps)| *steps > 0 && key.is(None, None, unwatched, genre, gen))
    }

    /// Is a back-off counting down? The store must be stepped while it is, as for `retry_cd`.
    pub(super) fn letters_waiting(&self) -> bool {
        self.letters_backoff.is_some()
    }

    /// One step of the back-off; spent at zero.
    pub(super) fn tick_letters_backoff(&mut self) {
        if let Some((_, steps)) = &mut self.letters_backoff {
            *steps = steps.saturating_sub(1);
            if *steps == 0 {
                self.letters_backoff = None;
            }
        }
    }

    /// A read asked under `key` came back with `list`. It becomes the landed list, and is shown only
    /// if `key` is still the section's ([`Self::shown_letters`]): an answer asked under older terms is
    /// stored but never current, so the next step asks again.
    ///
    /// The list it replaces is KEPT, one slot (so the last two keys are held, never more), unless it
    /// was counted under Unwatched: turning a genre or Unwatched on and then off again finds the
    /// unfiltered counts where they were instead of reading them anew. An Unwatched list is the
    /// exception because which titles it counts changes with every watch
    /// ([`Self::viewing_changed`]), so it is the one least likely to be worth showing again. The
    /// landed slot is no safer a place for it: it stays there, hidden, while a kept list is shown,
    /// and its key's `gen` is what keeps a watch made meanwhile from bringing it back. A list a
    /// membership change, a watch or a type change made unshowable ([`Self::membership_changed`],
    /// [`Self::viewing_changed`], [`Self::forget_letters`]) is never brought back: its key carries
    /// the old `gen`, or it is gone.
    pub(super) fn land_letters(&mut self, key: LettersKey, list: Vec<(String, i64)>) {
        match self.letters_for.take() {
            Some(previous) if previous != key => {
                let replaced = std::mem::replace(&mut self.letters, Arc::new(list));
                self.letters_kept = (!previous.viewing_sensitive()).then_some((previous, replaced));
            }
            _ => {
                self.letters = Arc::new(list);
                if self.letters_kept.as_ref().is_some_and(|(kept, _)| *kept == key) {
                    self.letters_kept = None;
                }
            }
        }
        self.letters_for = Some(key);
        self.letters_backoff = None;
    }

    /// A read asked under `key` FAILED: the list stays as it was and the same key waits.
    fn letters_failed(&mut self, key: LettersKey) {
        self.letters_backoff = Some((key, LETTERS_RETRY_STEPS));
    }

    /// Drop the list and everything about it. Runs on EVERY library type change, entering and
    /// leaving Collections included: counts describe one metadata type, and the loan holds none.
    pub(super) fn forget_letters(&mut self) {
        self.letters = Arc::default();
        self.letters_for = None;
        self.letters_kept = None;
        self.letters_backoff = None;
        self.forget_bands();
    }

    /// What the server lists under this section changed (a library scan, a read-back that moved a
    /// row's membership) though no term did: every offset after the change is off by one until the
    /// counts are asked again, and the listing is forgotten as a query edit forgets it.
    ///
    /// This is the GRID's side of a change, reached only for a row a loaded page holds. A watched
    /// state read back for a title in no loaded page (one in a page not fetched, or finished from
    /// Home) comes here for nothing, and the grid's later pages keep the blind spot they always had;
    /// the rail's counts do not: [`Self::viewing_changed`] stales every count made under Unwatched on
    /// any such read-back.
    pub(super) fn membership_changed(&mut self) {
        self.letters_gen = self.letters_gen.wrapping_add(1);
        self.reset_listing();
    }

    /// The server's read-back gave the watched state of one of ITS titles, which may be in this
    /// section and may be in a page the grid has not loaded: a [`crate::viewstate::ViewAt`] names a
    /// server, not a section, and the counts are cheap to ask again, so every section of the server
    /// answers. Every list counted under Unwatched is stale from here on, whether or not Unwatched
    /// is applied now: the one behind a shown unfiltered list is brought back by the switch, and
    /// the read that is out was asked before the title changed. Only the counts that move with a watch
    /// depend on viewing: those counted under Unwatched, and a bands family a watch changes (plays,
    /// unplayed), whatever its terms. An unfiltered or genre-only list of the letters, or of another
    /// family, stands ([`Self::key_gen`]). Returns true when the rail on screen lost its list, so the
    /// store publishes a change. `kind` is the section's, which says the family of the sort.
    pub(super) fn viewing_changed(&mut self, kind: SecKind) -> bool {
        let (letters, bands) = (self.letters_current(), self.bands_current(kind));
        self.viewing_gen = self.viewing_gen.wrapping_add(1);
        (letters && !self.letters_current()) || (bands && !self.bands_current(kind))
    }

    // ---- the value sorts' bands ------------------------------------------------------------------

    /// The family whose bands the rail draws for the sort the grid is ordered by, or `None` (a title
    /// sort has the letters; a sort without a ladder, or a family the rail is not ready for, none). A
    /// calendar family also needs the day: the application has none to hand, no bands.
    pub(super) fn rail_family(&self, kind: SecKind) -> Option<BandFamily> {
        let sort = self.sorts.get(self.sort_idx)?;
        BandFamily::of_sort(&sort.key, self.sort_desc, kind, self.library_type)
            .filter(|family| family.rail_ready() && (family.basis().is_none() || self.today.is_some()))
    }

    /// What the rail shows: the verdict of the band counts that describe the listing the grid shows now
    /// ([`Self::bands_verdict_for`]; `Some(Some(rail))` a rail, `Some(None)` counted and not trusted or
    /// empty, no rail) or, while this day's read is out, the one the day before counted. A
    /// calendar rail is re-asked when the day turns, and a rail that vanished for the length of a read
    /// would take a focus seated in it away; the previous day's bands (same family, same terms, same
    /// generation, only `today` differs) are a second off and stand until the new verdict lands
    /// ([`Self::land_bands`] drops them then). They are looked for in both slots, as the verdict is: a
    /// round trip through another sort or the Unwatched switch leaves them in the kept one.
    fn bands_on_screen(&self, kind: SecKind) -> Option<&Option<Arc<BandRail>>> {
        let family = self.rail_family(kind)?;
        self.bands_verdict_for(family).or_else(|| {
            let (unwatched, genre) = self.applied_terms();
            let (gen, today) = (self.key_gen(sensitive(Some(family), unwatched)), self.key_today(family));
            self.bands_slots()
                .find(|(key, _)| key.is_but_for_the_day(Some(family), today, unwatched, genre, gen))
                .map(|(_, verdict)| verdict)
        })
    }

    /// The two slots of landed band counts, the landed one first.
    fn bands_slots(&self) -> impl Iterator<Item = &(LettersKey, Option<Arc<BandRail>>)> {
        [self.bands.as_ref(), self.bands_kept.as_ref()].into_iter().flatten()
    }

    /// The verdict of the band counts of `family` asked under the terms and the day the grid has NOW, if
    /// one is on hand, whether or not the rail is ready for the family.
    pub(super) fn bands_verdict_for(&self, family: BandFamily) -> Option<&Option<Arc<BandRail>>> {
        let (unwatched, genre) = self.applied_terms();
        let gen = self.key_gen(sensitive(Some(family), unwatched));
        let today = self.key_today(family);
        self.bands_slots()
            .find(|(key, _)| key.is(Some(family), today, unwatched, genre, gen))
            .map(|(_, verdict)| verdict)
    }

    /// The rail of the band counts that describe the listing the grid shows now; like
    /// [`Self::shown_letters`], the one place a bands list is picked, at the READ site.
    pub(super) fn shown_bands(&self, kind: SecKind) -> Option<&Arc<BandRail>> {
        self.bands_on_screen(kind)?.as_ref()
    }

    /// Is a bands list on screen for the listing the grid shows now (a rail or a verdict, the previous
    /// day's included while this day's is out)?
    pub(super) fn bands_current(&self, kind: SecKind) -> bool {
        self.bands_on_screen(kind).is_some()
    }

    /// Is a bands read owed: the sort has a ladder, no list on hand describes the grid, and a failed
    /// read of THIS key is not holding the next one back?
    pub(super) fn bands_owed(&self, kind: SecKind) -> bool {
        self.rail_family(kind).is_some_and(|family| self.bands_owed_for(family))
    }

    /// [`Self::bands_owed`] for `family`, whether or not the rail is ready for it.
    pub(super) fn bands_owed_for(&self, family: BandFamily) -> bool {
        if self.bands_verdict_for(family).is_some() {
            return false;
        }
        let (unwatched, genre) = self.applied_terms();
        let gen = self.key_gen(sensitive(Some(family), unwatched));
        let today = self.key_today(family);
        !self.bands_backoff.as_ref().is_some_and(|(key, steps)| *steps > 0 && key.is(Some(family), today, unwatched, genre, gen))
    }

    /// Is a bands back-off counting down? The store must be stepped while it is.
    pub(super) fn bands_waiting(&self) -> bool {
        self.bands_backoff.is_some()
    }

    /// One step of the back-off; spent at zero.
    pub(super) fn tick_bands_backoff(&mut self) {
        if let Some((_, steps)) = &mut self.bands_backoff {
            *steps = steps.saturating_sub(1);
            if *steps == 0 {
                self.bands_backoff = None;
            }
        }
    }

    /// A bands read asked under `key` came back: with a rail, or with none (`None`: an empty listing
    /// or counts not trusted over a listing that held still, a verdict that a retry would not change
    /// until the key moves). It becomes the landed list and
    /// the one it replaces is kept in the one slot, as [`Self::land_letters`] keeps its own, unless it
    /// moves with a watch.
    pub(super) fn land_bands(&mut self, key: LettersKey, rail: Option<BandRail>) {
        let landed = rail.map(Arc::new);
        match self.bands.take() {
            // The previous day's list of the same terms is of no use once this day's is here: it is dropped
            // and the kept slot (another set of terms) stays.
            Some(previous) if previous.0.of_another_day_than(&key) => {}
            Some(previous) if previous.0 != key => {
                self.bands_kept = (!previous.0.viewing_sensitive()).then_some(previous);
            }
            _ => {
                if self.bands_kept.as_ref().is_some_and(|(kept, _)| *kept == key) {
                    self.bands_kept = None;
                }
            }
        }
        // …and so is a previous day's list the kept slot holds (the day's list a round trip through another
        // sort or the Unwatched switch left there), which the on-screen fallback stands on until now.
        if self.bands_kept.as_ref().is_some_and(|(kept, _)| kept.of_another_day_than(&key)) {
            self.bands_kept = None;
        }
        self.bands = Some((key, landed));
        self.bands_backoff = None;
    }

    /// A bands read asked under `key` FAILED to get its counts: what was on hand stays and the same
    /// key waits.
    fn bands_failed(&mut self, key: LettersKey) {
        self.bands_backoff = Some((key, LETTERS_RETRY_STEPS));
    }

    /// Drop the bands and everything about them (every library type change, as the letters).
    pub(super) fn forget_bands(&mut self) {
        self.bands = None;
        self.bands_kept = None;
        self.bands_backoff = None;
    }
}

impl BrowseState {
    /// Land the letters mailbox's answer. A read that SUCCEEDED replaces the list under the key it
    /// was asked with (an empty answer is a landed list: a filter matching nothing); one that failed
    /// touches nothing but the back-off. `#[track_caller]` for the paced dump, which keys a landing's
    /// claim by CALL SITE, as [`Self::land_directory_owned_with_gate`] says.
    #[track_caller]
    pub(super) fn land_letters_owned(
        &mut self,
        gate: &plx_machine::landgate::Gate,
        fetch: &crate::stores::Fetch<DirectoryResult<(String, i64)>>,
    ) -> bool {
        self.land_directory_owned_with_gate(gate, fetch, |state, list, ok, asked| {
            let Some(asked) = asked else { return };
            if ok {
                state.land_letters(asked, list);
            } else {
                state.letters_failed(asked);
            }
        })
    }

    /// Land the bands mailbox's answer: the same landing as the letters', for the counts of a value
    /// sort. A read that answered lands under the key it was asked with, a rail or none; one that
    /// failed touches nothing but the back-off. `#[track_caller]` as [`Self::land_letters_owned`].
    #[track_caller]
    pub(super) fn land_bands_owned(
        &mut self,
        gate: &plx_machine::landgate::Gate,
        fetch: &crate::stores::Fetch<DirectoryResult<BandRail>>,
    ) -> bool {
        self.land_directory_owned_with_gate(gate, fetch, |state, list, ok, asked| {
            let Some(asked) = asked else { return };
            if ok {
                state.land_bands(asked, list.into_iter().next());
            } else {
                state.bands_failed(asked);
            }
        })
    }

    /// [`Self::land_letters_owned`] on the fixture gate.
    #[cfg(test)]
    #[track_caller]
    pub(super) fn land_letters_for_test(
        &mut self,
        fetch: &crate::stores::Fetch<DirectoryResult<(String, i64)>>,
    ) -> bool {
        self.land_letters_owned(plx_machine::landgate::fixture_gate(), fetch)
    }
}
