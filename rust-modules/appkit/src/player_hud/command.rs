//! **The HUD's command vocabulary** (`docs/hud-plugins.md`, "Commands"): the closed set of things
//! a control may ask the app to do, each with typed parameters and an APP-OWNED enabled predicate.
//!
//! A control has an IDENTITY and a COMMAND, and the two are separate (D4). The identity is the
//! control's ordinal in its layout ([`super::control_elem`] turns it into the engine's element
//! id); the command is one value of [`HudCommand`], and several controls may carry the same
//! command with different parameters. Nothing here performs anything: the app's one performer
//! (`app::playback::perform_hud_command`) matches on the command, so a layout, a package or the
//! More menu's folded rows can name an action but never implement one.
//!
//! [`HudCommand::enabled`] reads [`HudModel`] and nothing else, so it needs no global and a test
//! builds the state it grades. It answers "may this press do anything right now", which is the
//! table's "Enabled" column: a disabled control has no focus stop and no pointer target.

use super::model::HudModel;
use super::state::{Action, BTN_N};
use std::num::NonZeroI32;

/// Which way a `setting`-sized jump goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekDir {
    Back,
    Forward,
}

/// How far a [`HudCommand::Seek`] jumps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekBy {
    /// A literal number of seconds whose sign is the direction. Never zero: a zero jump does
    /// nothing, so it is not representable. Not capped: the target is clamped to the content at
    /// the press, as every jump is (`SeekBurst::jump`).
    Secs(NonZeroI32),
    /// The viewer's Skip interval (Settings > Playback), in `dir`'s direction.
    Setting(SeekDir),
}

impl SeekBy {
    /// The signed step in nanoseconds. `interval_ns` is the viewer's Skip interval, which the
    /// caller reads (this module reads no global): only [`SeekBy::Setting`] uses it.
    pub fn step_ns(self, interval_ns: i64) -> i64 {
        match self {
            SeekBy::Secs(secs) => i64::from(secs.get()) * 1_000_000_000,
            SeekBy::Setting(SeekDir::Forward) => interval_ns,
            SeekBy::Setting(SeekDir::Back) => -interval_ns,
        }
    }
}

/// The track menu's two panels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackTab {
    Audio,
    Subtitles,
}

impl TrackTab {
    /// The tab number `OverlayKind::Tracks { tab }` addresses: Audio 0, Subtitles 1.
    pub fn index(self) -> i32 {
        match self {
            TrackTab::Audio => 0,
            TrackTab::Subtitles => 1,
        }
    }
}

/// A page of the More menu a command can open it on: the root, or a drill-in
/// ([`crate::more_menu::MorePage`], which has no root, because the root is the empty page stack).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MorePage {
    Root,
    Quality,
    Speed,
}

impl MorePage {
    /// The drill-in page this addresses, `None` for the root.
    pub fn page(self) -> Option<crate::more_menu::MorePage> {
        match self {
            MorePage::Root => None,
            MorePage::Quality => Some(crate::more_menu::MorePage::Quality),
            MorePage::Speed => Some(crate::more_menu::MorePage::Speed),
        }
    }
}

/// The two sections a control can open or close in the middle of the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Chapters,
    Info,
}

/// One thing the HUD can ask the app to do. Exactly the vocabulary of the design's "Commands"
/// table; a new command is a format MINOR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HudCommand {
    /// Pause when playing, resume when paused: the Play/Pause/PlayPause key, extracted.
    TogglePlay,
    /// A relative jump.
    Seek(SeekBy),
    /// The previous episode (or a restart of this one, the Previous disc's glyph says which).
    Previous,
    /// The next episode.
    Next,
    /// The audio or subtitle picker, on its own tab.
    OpenTracks(TrackTab),
    /// The More menu, on a page.
    OpenMore(MorePage),
    /// Open the section, or close it when it is already up.
    ToggleSection(Section),
    /// Start the item over.
    FromBeginning,
    /// Leave playback for the playing item's own page.
    GoToDetail,
    /// Show or hide Stats for nerds.
    ToggleStats,
}

impl HudCommand {
    /// **Is a press of this command meaningful now?** The table's "Enabled" column, from the
    /// frame's [`HudModel`] alone.
    pub fn enabled(self, model: &HudModel) -> bool {
        match self {
            // a jump needs a known duration to clamp against (`commit_jump` returns without one)
            HudCommand::Seek(_) => model.duration_ns > 0,
            // `Controls` already folds a restart target into `previous` (`Controls::for_item`)
            HudCommand::Previous => model.controls.previous,
            HudCommand::Next => model.controls.next,
            HudCommand::ToggleSection(Section::Chapters) => model.chapters,
            HudCommand::GoToDetail => model.detail,
            HudCommand::TogglePlay
            | HudCommand::OpenTracks(_)
            | HudCommand::OpenMore(_)
            | HudCommand::ToggleSection(Section::Info)
            | HudCommand::FromBeginning
            | HudCommand::ToggleStats => true,
        }
    }
}

/// The `by` of a `seek` item as the package wrote it: a nonzero integer of seconds, or the name `setting`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ByParam<'a> {
    Secs(i64),
    Name(&'a str),
}

/// A control item's command parameters, the sibling keys of its `command` as the package wrote them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Params<'a> {
    pub by: Option<ByParam<'a>>,
    pub dir: Option<&'a str>,
    pub tab: Option<&'a str>,
    pub page: Option<&'a str>,
    pub section: Option<&'a str>,
}

impl HudCommand {
    /// **The command a control item names**, from its `command` string and parameter keys. `None` for a
    /// name outside the vocabulary or a parameter outside its type (a zero or out-of-range `by`, a
    /// `setting` seek with no `dir`); the format's validation rejects those before a compile, so a `None`
    /// here is a control that performs nothing rather than something wrong.
    pub fn from_item(name: &str, p: &Params<'_>) -> Option<Self> {
        Some(match name {
            "toggle_play" => HudCommand::TogglePlay,
            "seek" => HudCommand::Seek(match p.by? {
                ByParam::Secs(n) => SeekBy::Secs(NonZeroI32::new(i32::try_from(n).ok()?)?),
                ByParam::Name("setting") => SeekBy::Setting(match p.dir? {
                    "back" => SeekDir::Back,
                    "forward" => SeekDir::Forward,
                    _ => return None,
                }),
                ByParam::Name(_) => return None,
            }),
            "previous" => HudCommand::Previous,
            "next" => HudCommand::Next,
            "open_tracks" => HudCommand::OpenTracks(match p.tab? {
                "subtitles" => TrackTab::Subtitles,
                "audio" => TrackTab::Audio,
                _ => return None,
            }),
            "open_more" => HudCommand::OpenMore(match p.page.unwrap_or("root") {
                "root" => MorePage::Root,
                "quality" => MorePage::Quality,
                "speed" => MorePage::Speed,
                _ => return None,
            }),
            "toggle_section" => HudCommand::ToggleSection(match p.section? {
                "chapters" => Section::Chapters,
                "info" => Section::Info,
                _ => return None,
            }),
            "from_beginning" => HudCommand::FromBeginning,
            "go_to_detail" => HudCommand::GoToDetail,
            "toggle_stats" => HudCommand::ToggleStats,
            _ => return None,
        })
    }
}

/// Classic's five discs as the controls they are, in the order they are drawn and walked:
/// Previous, Next, Subtitles, Audio, More. The index is the control's ordinal and
/// [`super::BTN_PREVIOUS`] .. [`super::BTN_MORE`] name them.
pub const CLASSIC_COMMANDS: [HudCommand; BTN_N as usize] = [
    HudCommand::Previous,
    HudCommand::Next,
    HudCommand::OpenTracks(TrackTab::Subtitles),
    HudCommand::OpenTracks(TrackTab::Audio),
    HudCommand::OpenMore(MorePage::Root),
];

impl From<Action> for HudCommand {
    /// The slot-addressed form of the five classic controls, one for one. [`Action`] is what
    /// `Controls::action` answers (the HUD pins grade it); this is what the app performs.
    fn from(action: Action) -> Self {
        match action {
            Action::Previous => HudCommand::Previous,
            Action::Next => HudCommand::Next,
            Action::Subtitles => HudCommand::OpenTracks(TrackTab::Subtitles),
            Action::Audio => HudCommand::OpenTracks(TrackTab::Audio),
            Action::More => HudCommand::OpenMore(MorePage::Root),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player_hud::state::{
        control_elem, row_index, ControlSlot, Controls, BTN_AUDIO, BTN_MORE, BTN_NEXT, BTN_PREVIOUS,
        BTN_SUBTITLES, ELEM_CHAPTER_BASE, ELEM_CONTROL_BASE, ELEM_FAILURE_END, ELEM_SCRUB, ELEM_TAB_BASE,
    };

    fn secs(n: i32) -> SeekBy {
        SeekBy::Secs(NonZeroI32::new(n).unwrap())
    }

    fn model(f: impl FnOnce(&mut HudModel)) -> HudModel {
        let mut m = HudModel::default();
        f(&mut m);
        m
    }

    /// Every classic control, pressed, is today's action one for one: the table the app's
    /// performer used to spell as a `match` on `Action`.
    #[test]
    fn each_classic_control_is_the_command_of_the_action_it_always_performed() {
        let episode = Controls::for_item(true, true, true, false);
        let want = [
            (BTN_PREVIOUS, Action::Previous, HudCommand::Previous),
            (BTN_NEXT, Action::Next, HudCommand::Next),
            (BTN_SUBTITLES, Action::Subtitles, HudCommand::OpenTracks(TrackTab::Subtitles)),
            (BTN_AUDIO, Action::Audio, HudCommand::OpenTracks(TrackTab::Audio)),
            (BTN_MORE, Action::More, HudCommand::OpenMore(MorePage::Root)),
        ];
        for (i, action, command) in want {
            assert_eq!(episode.action(i), Some(action), "control {i}");
            assert_eq!(episode.command(i), Some(command), "control {i}");
            assert_eq!(HudCommand::from(action), command, "control {i}");
            assert_eq!(CLASSIC_COMMANDS[i as usize], command, "control {i} is in today's order");
            assert_eq!(ControlSlot::Discs(episode).command(i), Some(command), "control {i} through the slot");
        }
        // Subtitles opens tab 1 and Audio tab 0, as `OverlayKind::Tracks { tab }` always was
        assert_eq!(TrackTab::Subtitles.index(), 1);
        assert_eq!(TrackTab::Audio.index(), 0);
    }

    /// `command` and `action` are two spellings of one answer, whatever the row holds: the same
    /// controls are absent, and the rest agree.
    #[test]
    fn command_and_action_agree_for_every_row_the_discs_can_hold() {
        for previous in [false, true] {
            for next in [false, true] {
                for restart in [false, true] {
                    let controls = Controls { previous, next, restart };
                    for i in -1..=BTN_N {
                        assert_eq!(
                            controls.command(i),
                            controls.action(i).map(HudCommand::from),
                            "{controls:?} control {i}"
                        );
                    }
                }
            }
        }
        assert_eq!(ControlSlot::Discs(Controls::NONE).command(BTN_PREVIOUS), None, "an absent disc");
    }

    /// Every enabled predicate, pinned (the design's "Enabled" column).
    #[test]
    fn every_command_is_enabled_by_what_the_table_says() {
        let idle = HudModel::default();
        // always
        for always in [
            HudCommand::TogglePlay,
            HudCommand::OpenTracks(TrackTab::Audio),
            HudCommand::OpenTracks(TrackTab::Subtitles),
            HudCommand::OpenMore(MorePage::Root),
            HudCommand::OpenMore(MorePage::Quality),
            HudCommand::OpenMore(MorePage::Speed),
            HudCommand::ToggleSection(Section::Info),
            HudCommand::FromBeginning,
            HudCommand::ToggleStats,
        ] {
            assert!(always.enabled(&idle), "{always:?}");
        }

        // seek needs a known duration, in either spelling
        for by in [secs(-10), secs(30), SeekBy::Setting(SeekDir::Back), SeekBy::Setting(SeekDir::Forward)] {
            assert!(!HudCommand::Seek(by).enabled(&idle), "{by:?} without a duration");
            assert!(HudCommand::Seek(by).enabled(&model(|m| m.duration_ns = 1)), "{by:?} with one");
        }

        // Previous is enabled by `previous || restart` (a restart target counts), Next by `next`
        let controls = |previous, next, restart| model(|m| m.controls = Controls::for_item(true, previous, next, restart));
        assert!(!HudCommand::Previous.enabled(&idle) && !HudCommand::Next.enabled(&idle), "not an episode");
        assert!(!HudCommand::Previous.enabled(&controls(false, true, false)), "first episode, nothing to restart");
        assert!(HudCommand::Previous.enabled(&controls(false, true, true)), "a restart target counts");
        assert!(HudCommand::Previous.enabled(&controls(true, false, false)));
        assert!(!HudCommand::Next.enabled(&controls(true, false, true)), "the last episode has no Next");
        assert!(HudCommand::Next.enabled(&controls(false, true, false)));
        assert!(!HudCommand::Next.enabled(&controls(false, false, true)), "neither neighbour: no pair");

        // sections and the detail page need what they open
        assert!(!HudCommand::ToggleSection(Section::Chapters).enabled(&idle));
        assert!(HudCommand::ToggleSection(Section::Chapters).enabled(&model(|m| m.chapters = true)));
        assert!(!HudCommand::GoToDetail.enabled(&idle), "no ratingKey");
        assert!(HudCommand::GoToDetail.enabled(&model(|m| m.detail = true)));
    }

    /// A control's predicate is the one the row's focus stop uses: for classic's five discs,
    /// `HudCommand::enabled` over the model sampled from `Controls` is `Controls::enabled`.
    #[test]
    fn a_discs_command_is_enabled_exactly_when_its_disc_has_a_stop() {
        for previous in [false, true] {
            for next in [false, true] {
                for restart in [false, true] {
                    let controls = Controls::for_item(true, previous, next, restart);
                    let m = model(|m| m.controls = controls);
                    for i in 0..BTN_N {
                        assert_eq!(
                            CLASSIC_COMMANDS[i as usize].enabled(&m),
                            controls.enabled(i),
                            "{controls:?} control {i}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_seek_steps_by_its_literal_or_the_viewers_interval_with_the_sign_of_its_direction() {
        let interval = 15_000_000_000;
        assert_eq!(secs(-10).step_ns(interval), -10_000_000_000);
        assert_eq!(secs(30).step_ns(interval), 30_000_000_000);
        assert_eq!(SeekBy::Setting(SeekDir::Forward).step_ns(interval), interval);
        assert_eq!(SeekBy::Setting(SeekDir::Back).step_ns(interval), -interval);
        assert!(NonZeroI32::new(0).is_none(), "a zero jump is not representable");
    }

    /// Every command of the vocabulary is built from the keys a package writes, a parameter outside its
    /// type builds nothing, and a literal `by` keeps its sign.
    #[test]
    fn a_control_item_names_its_command_by_its_sibling_keys() {
        let none = Params::default();
        assert_eq!(HudCommand::from_item("toggle_play", &none), Some(HudCommand::TogglePlay));
        assert_eq!(HudCommand::from_item("previous", &none), Some(HudCommand::Previous));
        assert_eq!(HudCommand::from_item("next", &none), Some(HudCommand::Next));
        assert_eq!(HudCommand::from_item("from_beginning", &none), Some(HudCommand::FromBeginning));
        assert_eq!(HudCommand::from_item("go_to_detail", &none), Some(HudCommand::GoToDetail));
        assert_eq!(HudCommand::from_item("toggle_stats", &none), Some(HudCommand::ToggleStats));
        let by = |by| Params { by: Some(by), ..Params::default() };
        assert_eq!(HudCommand::from_item("seek", &by(ByParam::Secs(-10))), Some(HudCommand::Seek(secs(-10))));
        assert_eq!(HudCommand::from_item("seek", &by(ByParam::Secs(30))), Some(HudCommand::Seek(secs(30))));
        assert_eq!(HudCommand::from_item("seek", &by(ByParam::Secs(0))), None, "a zero jump is nothing");
        assert_eq!(HudCommand::from_item("seek", &by(ByParam::Secs(i64::MAX))), None);
        assert_eq!(HudCommand::from_item("seek", &none), None, "a seek needs its `by`");
        let setting = Params { by: Some(ByParam::Name("setting")), dir: Some("forward"), ..Params::default() };
        assert_eq!(HudCommand::from_item("seek", &setting), Some(HudCommand::Seek(SeekBy::Setting(SeekDir::Forward))));
        assert_eq!(HudCommand::from_item("seek", &Params { dir: None, ..setting }), None, "`setting` needs a `dir`");
        let tab = |t| Params { tab: Some(t), ..Params::default() };
        assert_eq!(HudCommand::from_item("open_tracks", &tab("subtitles")), Some(HudCommand::OpenTracks(TrackTab::Subtitles)));
        assert_eq!(HudCommand::from_item("open_tracks", &tab("audio")), Some(HudCommand::OpenTracks(TrackTab::Audio)));
        assert_eq!(HudCommand::from_item("open_tracks", &tab("video")), None);
        assert_eq!(HudCommand::from_item("open_more", &none), Some(HudCommand::OpenMore(MorePage::Root)), "`page` defaults to the root");
        let page = Params { page: Some("speed"), ..Params::default() };
        assert_eq!(HudCommand::from_item("open_more", &page), Some(HudCommand::OpenMore(MorePage::Speed)));
        let section = |s| Params { section: Some(s), ..Params::default() };
        assert_eq!(HudCommand::from_item("toggle_section", &section("chapters")), Some(HudCommand::ToggleSection(Section::Chapters)));
        assert_eq!(HudCommand::from_item("toggle_section", &section("info")), Some(HudCommand::ToggleSection(Section::Info)));
        assert_eq!(HudCommand::from_item("dance", &none), None);
    }

    #[test]
    fn open_more_addresses_the_root_or_a_drill_in() {
        assert_eq!(MorePage::Root.page(), None);
        assert_eq!(MorePage::Quality.page(), Some(crate::more_menu::MorePage::Quality));
        assert_eq!(MorePage::Speed.page(), Some(crate::more_menu::MorePage::Speed));
    }

    /// A control's element is `ELEM_CONTROL_BASE + ordinal` in an open-ended range that meets no
    /// other element and stops short of the chapter strip's range at the top of `u32`.
    #[test]
    fn a_controls_element_is_its_ordinal_in_an_open_range_clear_of_every_other() {
        for ordinal in [0, 1, BTN_N - 1, 100, 10_000] {
            assert_eq!(control_elem(ordinal), ELEM_CONTROL_BASE + ordinal as u32);
            assert_eq!(row_index(control_elem(ordinal)), Some(ordinal));
        }
        for other in [ELEM_SCRUB, ELEM_TAB_BASE, ELEM_TAB_BASE + 1, ELEM_FAILURE_END - 1, 40_000, 40_001] {
            assert_eq!(row_index(other), None, "{other} is not a control");
        }
        assert!(ELEM_CONTROL_BASE > 40_000 && ELEM_CONTROL_BASE > ELEM_FAILURE_END);
        assert_eq!(row_index(ELEM_CHAPTER_BASE), None, "the chapters' range is not a control's");
        assert_eq!(row_index(u32::MAX), None);
        assert!(row_index(ELEM_CHAPTER_BASE - 1).is_some(), "the control range runs up to it");
    }
}
