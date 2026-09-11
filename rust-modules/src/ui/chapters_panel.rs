//! In-player Chapters strip: a horizontal row of chapter cards (thumbnail + name + timestamp) over
//! the transport, opened from the HUD's Chapters tab. LEFT/RIGHT pick a chapter, OK seeks to its
//! start. Card layout mirrors the detail-page episode picker; modal wiring mirrors info_panel.
//!
//! Data comes from the PLAYING leaf (`metadata::playing_chapters`, loaded with `?includeChapters=1`
//! on the same fetch the track store already makes), never from `metadata::current()` — the same
//! identity rule `ui/track_menu.rs` and `screens/player/skip_pill.rs` state. Reading `current()` is what made
//! the Chapters tab vanish for every episode started from a show detail page: `current()` is then
//! the SHOW, and a show container carries no `Chapter[]`.
#![allow(dead_code)]
use crate::metadata;
use crate::ui::consts::{MARGIN_X, SCR_W, SDLK_LEFT, SDLK_RIGHT};
use crate::ui::theme;
use crate::ui::{Rect, Spring};
use std::ffi::CString;
use std::os::raw::c_int;

const CH_W: f32 = 288.0;
const CH_H: f32 = 162.0; // 16:9 still
const CH_GAP: f32 = 24.0;
const CH_TOP: f32 = 684.0; // thumbnail top — name/time fit above the tabs (SCR_H-128)
const CH_RAD: f32 = 10.0;
/// Focus rim on the selected chapter. The card family's resting sheen ([`theme::CARD_SHEEN`] .22 /
/// 1px) washes out over the hardware video plane the same way an unkeyed control's edge did — pure
/// white and a thicker stroke are what keep "which chapter" readable from the couch while scrolling.
/// Colour is the unkeyed focus edge ([`theme::CONTROL_RIM_FOCUS_UNKEYED`]); width is deliberately a
/// step over that control's 1.25 so a 288-wide still reads as selected, not merely edged.
const CH_FOCUS_RING_W: f32 = 2.5;
use crate::ui::widgets::CARD_FOCUS_SCALE;

/// The strip's whole state, owned by the container that mounts this panel — the modal PHASE and
/// the appear spring belong to `ui::containers::modal::ModalStack` now, not to this struct; `draw`
/// takes the appear fraction as a parameter instead of stepping its own `Popover`.
pub(crate) struct ChaptersState {
    sel: c_int,
    scroll: Spring, // horizontal scroll offset (px)
    scale: Spring,  // focused-card pop (springs 1.0 → FOCUS_SCALE on each move)
}

impl ChaptersState {
    /// focus the chapter that contains the current playhead
    pub(crate) fn new() -> Self {
        let pos_ms = crate::player::playpos_ns() / 1_000_000;
        let sel = chapters()
            .iter()
            .rposition(|c| c.start_ms <= pos_ms)
            .unwrap_or(0) as c_int;
        ChaptersState {
            sel,
            scroll: Spring::at(scroll_target(sel)),
            scale: Spring::at(1.0), // pop in
        }
    }

    /// The highlighted chapter, for the focus probe (`crate::focusprobe`). The strip's LEFT/RIGHT
    /// arm in `app.rs` moves this and nothing else, so the fingerprint is blind to it without a
    /// reader.
    pub(crate) fn sel(&self) -> c_int {
        self.sel
    }

    pub(crate) fn move_focus(&mut self, sym: c_int) {
        let sym = sym as u32;
        let nn = n();
        if nn == 0 {
            return;
        }
        let s = self.sel;
        let ns = if sym == SDLK_LEFT {
            (s - 1).max(0)
        } else if sym == SDLK_RIGHT {
            (s + 1).min(nn - 1)
        } else {
            s
        };
        if ns != s {
            self.scale.jump(1.0); // re-pop the newly-focused card
        }
        self.sel = ns;
    }

    /// seek target (nanoseconds) for the focused chapter, or -1 if none.
    pub(crate) fn on_ok(&self) -> i64 {
        let s = self.sel;
        chapters()
            .get(s.max(0) as usize)
            .map(|c| c.start_ms * 1_000_000)
            .unwrap_or(-1)
    }

    pub(crate) fn update(&mut self, dt: f32) {
        // The store this indexes belongs to the PLAYING item and a new play retires it, so re-clamp
        // rather than spring the scroll toward a slot that no longer exists (which culls every card
        // and leaves an empty panel). `on_ok`/`draw` are `.get()`-based, so this is about the strip
        // staying coherent, not about safety.
        let sel = self.sel.min((n() - 1).max(0));
        self.sel = sel;
        let sctgt = scroll_target(sel);
        self.scroll.step(sctgt, 220.0, dt);
        crate::ui::anim::probe("chapters.scroll", self.scroll.pos, self.scroll.vel, sctgt, dt);
        self.scale.step(CARD_FOCUS_SCALE, 300.0, dt);
        crate::ui::anim::probe(
            "chapters.scale",
            self.scale.pos,
            self.scale.vel,
            CARD_FOCUS_SCALE,
            dt,
        );
    }

    pub(crate) fn draw(&mut self, ps: &crate::route::PlaybackSession, appear: f32) {
        let chs = chapters();
        if chs.is_empty() {
            return;
        }
        let scroll = self.scroll.pos;
        let sel = self.sel;
        let scale = self.scale.pos;
        // reproduces exactly what `Popover::painter(0.0, 20.0)` (no scrim + `content_painter(20.0)`)
        // used to draw, translated further by the strip's own horizontal scroll.
        let p = crate::ui::Painter::root()
            .alpha(appear)
            .translate(0.0, crate::ui::popover::Popover::RISE * (1.0 - appear))
            .translate(-scroll, 0.0);

        // timecode uses SECONDARY (not the dim TERTIARY): it's drawn straight over the video, where the
        // dim grey washed out even up close. SECONDARY matches the (readable) chapter-name grey; the
        // name still leads by size (LABEL vs CAPTION) + bold.
        let dimc = theme::TEXT_SECONDARY;
        for (i, ch) in chs.iter().enumerate() {
            let x = MARGIN_X + i as f32 * (CH_W + CH_GAP);
            if !crate::ui::on_axis(x - scroll, CH_W, SCR_W, 0.0) {
                continue; // culled off-screen (the shared cull primitive)
            }
            let focused = i as c_int == sel;
            let card = Rect::new(x, CH_TOP, CH_W, CH_H);
            crate::ui::widgets::draw_card(
                p,
                card,
                crate::route::item_sid(crate::route::cur_sid(ps)),
                &ch.thumb,
                (480, 270),
                CH_RAD,
                focused,
                scale,
            );
            if focused {
                // Same scaled frame `card()` draws into, so the rim rides the focus pop rather than
                // lagging a resting box. Full strength whenever focused — the pop already animates the
                // geometry; fading the rim with it would blank the selection mark on every LEFT/RIGHT.
                p.rring(
                    card.scaled(scale),
                    CH_RAD,
                    CH_FOCUS_RING_W,
                    theme::CONTROL_RIM_FOCUS_UNKEYED,
                );
            }
            // name + timestamp beneath the card
            let ty = CH_TOP + CH_H + 26.0;
            let titc = if focused {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_SECONDARY
            };
            let name = if ch.title.trim().is_empty() {
                format!("Chapter {}", ch.index)
            } else {
                ch.title.clone()
            };
            if let Ok(tc) = CString::new(crate::text::elide(
                &name,
                CH_W,
                theme::size::LABEL,
                1,
                false,
            )) {
                p.text(tc.as_ptr(), x, ty, theme::size::LABEL, titc, 0, 1);
            }
            if let Ok(sc) = CString::new(crate::ui::fmt::clock(ch.start_ms)) {
                p.text(sc.as_ptr(), x, ty + 34.0, theme::size::CAPTION, dimc, 0, 0);
            }
        }
    }
}

/// the playing leaf's chapters — the ONE read, so within a frame the count, the open, the seek and
/// the draw cannot end up describing different items. ACROSS frames the store can still be replaced
/// (a new play retires it, `route::request_play`), which is why `update` re-clamps the selection.
fn chapters() -> &'static [metadata::Chapter] {
    metadata::playing_chapters()
}
fn n() -> c_int {
    chapters().len() as c_int
}
/// whether the PLAYING item has chapters — drives showing/hiding the Chapters tab
pub(crate) fn has_chapters() -> bool {
    n() > 0
}

fn scroll_target(sel: c_int) -> f32 {
    // pin the focused card to the 2nd slot (like the episode picker)
    if sel > 1 {
        (sel as f32 - 1.0) * (CH_W + CH_GAP)
    } else {
        0.0
    }
}
