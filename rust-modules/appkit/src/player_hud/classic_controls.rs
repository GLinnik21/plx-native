//! Opt-in classic row. The modern HUD delegates only this composition; geometry, focus pops,
//! controls and playback requests continue to use the shared systems.
use super::*;
use plx_ui::icons::Icon;

pub const ITEMS: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Previous,
    Next,
    Subtitles,
    Audio,
    More,
}

const ACTIONS: [Action; ITEMS] = [
    Action::Previous,
    Action::Next,
    Action::Subtitles,
    Action::Audio,
    Action::More,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
    pub previous: bool,
    pub next: bool,
}

impl Controls {
    pub fn now(ps: &plx_media::route::PlaybackSession) -> Self {
        use plx_media::route::{previous_restarts, queue_neighbors};
        let (previous, next) = queue_neighbors(ps);
        Self {
            previous: previous_restarts(plx_media::player::intended_pos_ns(ps)) || previous,
            next,
        }
    }
    pub fn action(self, index: i32) -> Option<Action> {
        let action = *ACTIONS.get(usize::try_from(index).ok()?)?;
        match action {
            Action::Previous if !self.previous => None,
            Action::Next if !self.next => None,
            _ => Some(action),
        }
    }
    pub fn rect(index: i32) -> Option<Rect> {
        let index = usize::try_from(index).ok().filter(|&i| i < ITEMS)?;
        let items = [Item::new(BTN_S); ITEMS];
        let (x, w) = HStack::new(
            &items,
            plx_ui::widgets::CONTROL_GAP,
            Align::Trailing(CTRL_RIGHT),
        )
        .span(index)?;
        Some(Rect::new(x, CTRL_Y, w, CTRL_H))
    }
    pub fn draw(self, row: &TransportRow, p: Painter, focused: bool, selected: i32) {
        for (index, action) in ACTIONS.into_iter().enumerate() {
            let icon = match action {
                Action::Previous => Icon::Previous,
                Action::Next => Icon::Next,
                Action::Subtitles => Icon::Cc,
                Action::Audio => Icon::Audio,
                Action::More => Icon::More,
            };
            let index = index as i32;
            let enabled = self.action(index).is_some();
            TransportButton::new(0, Self::rect(index).expect("the row's own index"))
                .icon(icon)
                .enabled(enabled)
                .focused(focused && selected == index)
                .ground(ControlGround::Unkeyed)
                .scale(row.scale(index))
                .draw(&hud_env(), p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_controls_keep_all_three_modern_actions_and_share_draw_and_hit_geometry() {
        let controls = Controls {
            previous: true,
            next: true,
        };
        assert_eq!(controls.action(2), Some(Action::Subtitles));
        assert_eq!(controls.action(3), Some(Action::Audio));
        assert_eq!(controls.action(4), Some(Action::More));
        for index in 0..ITEMS as i32 {
            let rect = Controls::rect(index).unwrap();
            assert!(rect.x >= plx_ui::consts::MARGIN_X && rect.x + rect.w <= CTRL_RIGHT);
            if index > 0 {
                let previous = Controls::rect(index - 1).unwrap();
                assert!(
                    (rect.x - previous.x - previous.w - plx_ui::widgets::CONTROL_GAP).abs() < 0.01
                );
            }
            let slot = ControlSlot::Classic(controls);
            assert_eq!(icon_hit(slot, rect.cx(), rect.cy()), Some(index));
        }
        assert!(controls.action(ITEMS as i32).is_none());
        assert!(Controls::rect(-1).is_none());
        assert!(Controls {
            previous: false,
            next: false
        }
        .action(0)
        .is_none());
        assert!(Controls {
            previous: false,
            next: false
        }
        .action(1)
        .is_none());
    }
}
