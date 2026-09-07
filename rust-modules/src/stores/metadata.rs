//! The detail page's item, its seasons and the playing item, as a machine over
//! `crate::metadata` (`docs/stores-as-machines.md`).

use crate::plex::ServerId;
use crate::ui::machine::{Cx, Effects, Handled, Host, Machine};

use super::{note, StoreEv, StoreId};

#[derive(Clone)]
pub(crate) enum MetadataCmd {
    /// Supersede any in-flight load and fetch `(sid, rk)` off-thread; lands through `pump_detail`.
    RequestDetail { sid: ServerId, rk: String },
    /// The BLOCKING load, for the callers that act on the item in the same frame.
    LoadDetailNow { sid: ServerId, rk: String },
    /// Close the page: drop the item and supersede everything in flight.
    Clear,
    /// The season strip: flip optimistically, fetch the episodes off-thread.
    LoadSeason(usize),
    /// The BLOCKING season load, for a caller that indexes the episodes in the same frame.
    LoadSeasonNow(usize),
    SetNowPlaying(Option<crate::metadata::NowPlaying>),
    /// The optimistic half of a view-state write on the loaded item, its episodes and Related.
    SetWatchedLocal { sid: ServerId, rk: String, on: bool },
    /// The playback plan's leaf (`route.rs`).
    InstallPlaying(Option<crate::metadata::PlayingItem>),
    MarkSkipped(crate::metadata::Marker),
    RetirePlaying,
    RetirePlayingItem,
}

pub(crate) struct MetadataStore;

/// The shim: step the store NOW through the one vocabulary and answer as the mutator did.
pub(crate) fn apply(cmd: MetadataCmd) -> bool {
    super::apply(super::StoreCmd::Metadata(cmd))
}

/// The store's own step, reached only through [`super::apply`].
pub(super) fn run(cmd: MetadataCmd) -> bool {
    let answer = match cmd {
        MetadataCmd::RequestDetail { sid, rk } => {
            crate::metadata::request_detail(sid, &rk);
            true
        }
        MetadataCmd::LoadDetailNow { sid, rk } => {
            crate::metadata::load_detail_now(sid, &rk);
            true
        }
        MetadataCmd::Clear => {
            crate::metadata::clear();
            true
        }
        MetadataCmd::LoadSeason(i) => {
            crate::metadata::load_season(i);
            true
        }
        MetadataCmd::LoadSeasonNow(i) => {
            crate::metadata::load_season_now(i);
            true
        }
        MetadataCmd::SetNowPlaying(np) => {
            crate::metadata::set_now_playing(np);
            true
        }
        MetadataCmd::SetWatchedLocal { sid, rk, on } => crate::metadata::set_watched_local(sid, &rk, on),
        MetadataCmd::InstallPlaying(p) => {
            crate::metadata::install_playing(p);
            true
        }
        MetadataCmd::MarkSkipped(m) => {
            crate::metadata::mark_skipped(m);
            true
        }
        MetadataCmd::RetirePlaying => {
            crate::metadata::retire_playing();
            true
        }
        MetadataCmd::RetirePlayingItem => {
            crate::metadata::retire_playing_item();
            true
        }
    };
    super::bump(StoreId::Metadata);
    answer
}

/// The three route-unconditional landings the loop runs every frame.
pub(crate) fn pump_detail() -> bool {
    note(StoreId::Metadata, crate::metadata::pump_detail())
}
pub(crate) fn pump_season() -> bool {
    note(StoreId::Metadata, crate::metadata::pump_season())
}
pub(crate) fn pump_alt_sources() {
    crate::metadata::pump_alt_sources();
}

impl<H: Host> Machine<H> for MetadataStore {
    type Ev = StoreEv<MetadataCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(c) => {
                run(c.clone());
            }
            StoreEv::Pump { .. } => {
                pump_detail();
                pump_season();
                pump_alt_sources();
            }
        }
        Handled::Yes
    }
}
