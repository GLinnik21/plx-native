//! Marker preferences and the five-second offer, as values. The app supplies viewer grants,
//! playback identity, visibility and Tick time; this machine reads no registry or clock.
use plx_data::metadata::{Marker, MarkerKind};
use plx_plex::plex::session::MarkerSkipMode;

pub const COUNTDOWN_US: u32 = 5_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarkerPrefsView {
    /// Monotonic identity of the seated profile in this process; zero seats nobody.
    pub scope: u64,
    pub needs_refresh: bool,
    pub intro_granted: bool,
    pub credits_granted: bool,
    pub intro: MarkerSkipMode,
    pub credits: MarkerSkipMode,
}
impl MarkerPrefsView {
    pub fn granted(self, kind: MarkerKind) -> bool {
        self.scope != 0
            && match kind {
                MarkerKind::Intro => self.intro_granted,
                MarkerKind::Credits => self.credits_granted,
            }
    }
    pub fn mode(self, kind: MarkerKind) -> MarkerSkipMode {
        if !self.granted(kind) {
            return MarkerSkipMode::Manual;
        }
        match kind {
            MarkerKind::Intro => self.intro,
            MarkerKind::Credits => self.credits,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfferId {
    pub scope: u64,
    pub entry: u32,
    pub playback: u32,
    pub marker: Marker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfferOutcome {
    Applied,
    Deferred,
    Retired,
}

#[derive(Default)]
pub struct OfferTimer {
    id: Option<OfferId>,
    elapsed_us: u32,
    consumed: bool,
    in_flight: bool,
    presented: bool,
}
impl OfferTimer {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn cancel(&mut self) {
        self.consumed = true;
        self.in_flight = false;
    }
    pub fn resolve(&mut self, id: OfferId, outcome: OfferOutcome) {
        if self.id != Some(id) || !self.in_flight {
            return;
        }
        self.in_flight = false;
        if outcome == OfferOutcome::Deferred {
            self.consumed = false;
            self.presented = false;
        }
    }
    pub fn snapshot(&self) -> (Option<OfferId>, u32, bool, bool, bool) {
        (
            self.id,
            self.elapsed_us,
            self.consumed,
            self.presented,
            self.in_flight,
        )
    }
    pub fn id(&self) -> Option<OfferId> {
        self.id
    }
    pub fn pending(&self) -> bool {
        self.id.is_some() && !self.consumed
    }
    pub fn progress(&self) -> f32 {
        self.elapsed_us as f32 / COUNTDOWN_US as f32
    }
    /// A rendered, actionable Watch opportunity was actually presented. Drawing records the
    /// renderer's receipt here; the next Tick may count that presentation.
    pub fn presented(&mut self) {
        self.presented = true;
    }
    /// `id` follows the *real* marker interval, independent of a transient Playing/buffering slot.
    /// The first eligible tick arms; it never counts time before the pill could be seen.
    pub fn tick(
        &mut self,
        id: Option<OfferId>,
        advancing: bool,
        visible: bool,
        dt_us: u32,
    ) -> Option<OfferId> {
        if id != self.id {
            self.reset();
            self.id = id;
        }
        let shown = std::mem::take(&mut self.presented);
        if !self.pending() || !advancing || !visible || !shown {
            return None;
        }
        self.elapsed_us = self.elapsed_us.saturating_add(dt_us).min(COUNTDOWN_US);
        if self.elapsed_us == COUNTDOWN_US {
            self.consumed = true;
            self.in_flight = true;
            self.id
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(playback: u32) -> OfferId {
        OfferId {
            scope: 1,
            entry: 1,
            playback,
            marker: Marker {
                kind: MarkerKind::Intro,
                start_ms: 1_000,
                end_ms: 60_000,
                final_seg: false,
            },
        }
    }
    fn step(t: &mut OfferTimer, id: OfferId, us: u32) -> Option<OfferId> {
        t.presented();
        t.tick(Some(id), true, true, us)
    }
    #[test]
    fn five_presented_seconds_fire_once_and_cancel_wins_the_expiry_frame() {
        let mut t = OfferTimer::default();
        let offer = id(1);
        assert_eq!(step(&mut t, offer, 1_000_000), None); // first appearance has not been presented
        assert_eq!(step(&mut t, offer, 4_999_000), None);
        t.cancel();
        assert_eq!(step(&mut t, offer, 1_000), None);
        assert_eq!(step(&mut t, offer, 10_000_000), None);
        assert_eq!(step(&mut t, id(2), 1_000_000), None);
        assert_eq!(step(&mut t, id(2), 5_000_000), Some(id(2)));
        assert_eq!(step(&mut t, id(2), 5_000_000), None);
    }
    #[test]
    fn pause_buffer_and_hidden_time_do_not_count_and_exit_resets() {
        let mut t = OfferTimer::default();
        let offer = id(1);
        step(&mut t, offer, 0);
        step(&mut t, offer, 2_000_000);
        t.presented();
        assert_eq!(t.tick(Some(offer), false, true, 20_000_000), None);
        t.presented();
        assert_eq!(t.tick(Some(offer), true, false, 20_000_000), None);
        assert_eq!(step(&mut t, offer, 2_999_000), None);
        assert_eq!(step(&mut t, offer, 1_000), Some(offer));
        t.tick(None, true, true, 0);
        assert_eq!(step(&mut t, offer, 5_000_000), None);
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use crate::player_hud::{slot_for, slot_for_preferences, ControlSlot};
    use plx_media::route::NextEpisodeMode;
    #[test]
    fn automatic_final_credits_keep_both_existing_up_next_controls() {
        let marker = Marker {
            kind: MarkerKind::Credits,
            start_ms: 100_000,
            end_ms: 120_000,
            final_seg: true,
        };
        let prefs = MarkerPrefsView {
            scope: 1,
            credits_granted: true,
            credits: MarkerSkipMode::Automatic,
            ..Default::default()
        };
        let slot = slot_for_preferences(Some(marker), true, true, NextEpisodeMode::Countdown, prefs);
        assert!(matches!(slot, ControlSlot::AutomaticUpNext(m) if m == marker));
        assert_eq!(slot.items(), 2, "Watch Credits and Next Episode stay available");
        assert_eq!(slot.primary_btn(), crate::up_next::BTN_NEXT);
    }
    #[test]
    fn automatic_marker_policy_preserves_legacy_and_the_existing_pair() {
        for kind in [MarkerKind::Intro, MarkerKind::Credits] {
            for final_seg in [false, true] {
                let marker = Marker {
                    kind,
                    start_ms: 1_000,
                    end_ms: 60_000,
                    final_seg,
                };
                for has_next in [false, true] {
                    for next in NextEpisodeMode::LADDER {
                        let stored = MarkerPrefsView {
                            scope: 1,
                            intro: MarkerSkipMode::Automatic,
                            credits: MarkerSkipMode::Automatic,
                            ..Default::default()
                        };
                        assert!(
                            slot_for_preferences(Some(marker), true, has_next, next, stored)
                                == slot_for(Some(marker), has_next, next)
                        );
                        let granted = MarkerPrefsView {
                            intro_granted: true,
                            credits_granted: true,
                            ..stored
                        };
                        let automatic = slot_for_preferences(Some(marker), true, has_next, next, granted);
                        assert_eq!(automatic.automatic_marker(), Some(marker));
                        assert_eq!(automatic.is_up_next_pair(), slot_for(Some(marker), has_next, next).is_up_next_pair());
                        assert!(
                            slot_for_preferences(Some(marker), false, has_next, next, granted)
                                == slot_for(Some(marker), has_next, next),
                            "synthesized tails keep their own policy"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;
    #[test]
    fn deferred_expiry_retries_only_after_presentation_and_cancel_ignores_late_reply() {
        let id = OfferId {
            scope: 1,
            entry: 1,
            playback: 1,
            marker: Marker {
                kind: MarkerKind::Intro,
                start_ms: 1_000,
                end_ms: 60_000,
                final_seg: false,
            },
        };
        let mut t = OfferTimer::default();
        t.tick(Some(id), true, true, 0);
        t.presented();
        assert_eq!(t.tick(Some(id), true, true, COUNTDOWN_US), Some(id));
        assert_eq!(t.tick(Some(id), true, true, COUNTDOWN_US), None);
        t.resolve(id, OfferOutcome::Deferred);
        assert_eq!(t.tick(Some(id), true, true, 0), None);
        t.presented();
        assert_eq!(t.tick(Some(id), true, true, 0), Some(id));
        t.cancel();
        t.resolve(id, OfferOutcome::Deferred);
        t.presented();
        assert_eq!(t.tick(Some(id), true, true, COUNTDOWN_US), None);
    }
}
