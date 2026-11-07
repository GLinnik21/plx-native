//! The pure data a URI-mode source and its display path carry between the route owner and the
//! URI driver. Route policy qualifies a source and the application stages it into a driver; neither
//! names the driver's crate, so the shapes live here and the driver re-exports them.

use std::os::raw::c_int;

use crate::{AttemptId, DriverSet, Failure, Requirements, SourceKey, TrackChoice, TrackKind,
    TrackSelection};

/// An explicitly qualified display path. `Acb` is the webOS 4.x plane, bound through the plane
/// owner; `Exported` is the exported window at a canvas size; `Unbound` is no certified path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding { Exported { canvas: (c_int, c_int) }, Acb, Unbound }

/// Explicit translation from a neutral selected track to this source's URI-native index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeTrackIndex { pub selection: TrackSelection, pub index: i64 }

/// The native index of `selected`, only when exactly one mapping names it and the index is valid.
pub fn native_index(tracks: &[NativeTrackIndex], selected: TrackSelection) -> Option<i64> {
    let mut matches = tracks.iter().filter(|track| track.selection == selected);
    let first = matches.next()?;
    (first.index >= 0 && matches.next().is_none()).then_some(first.index)
}

/// The authorized URL stays opaque; callers provide the exact native track map with it.
pub struct UriSource { url: String, tracks: Vec<NativeTrackIndex> }
impl UriSource {
    pub fn new(url: String, tracks: Vec<NativeTrackIndex>) -> Self { Self { url, tracks } }

    pub fn has_url(&self) -> bool { !self.url.is_empty() }

    /// The locator and its track map, handed to the driver that executes the source.
    pub fn into_parts(self) -> (String, Vec<NativeTrackIndex>) { (self.url, self.tracks) }

    pub fn valid_track_map(&self) -> bool {
        !self.tracks.iter().enumerate().any(|(i, track)| track.index < 0
            || self.tracks[..i].iter().any(|prior| prior.selection == track.selection
                || (prior.selection.kind == track.selection.kind && prior.index == track.index)))
    }

    /// Whether this owned source can execute its required native track choices. The same
    /// check guards qualification and the physical Start; no URL or index map escapes.
    pub fn can_supply(&self, requirements: Requirements) -> bool {
        let mapped = |choice, kind| match choice {
            TrackChoice::Unspecified => true,
            TrackChoice::Off => false,
            TrackChoice::Selected(id) => native_index(&self.tracks, TrackSelection { kind, id }).is_some(),
        };
        self.has_url() && self.valid_track_map()
            && mapped(requirements.audio_track, TrackKind::Audio)
            && mapped(requirements.subtitle_track, TrackKind::Subtitle)
    }
}

/// What the player adapter needs of a URI driver beyond [`DriverSet`]: the escrow of an
/// authorized source, and two ownership facts the route reads. The adapter holds one as
/// `Box<dyn UriDriver<Context = MainThread>>`, so the product driver (`plx_play_uri`) and a
/// test fake are interchangeable and the player crate names neither.
pub trait UriDriver: DriverSet {
    /// Give exactly one authorized, owned URL to the next matching Start. Superseding a staged
    /// source destroys its old locator; a failed, cancelled or expired Start cannot reuse it.
    fn stage_source(&mut self, attempt: AttemptId, key: SourceKey, source: UriSource)
        -> Result<(), Failure>;
    /// Cancel an unconsumed locator without changing a live native attempt.
    fn cancel_staged(&mut self, attempt: AttemptId);
    /// The plane remains owned after the service reports Playing until native release. This is
    /// an ownership fact, not proof that a picture reached the panel.
    fn plane_owned(&self) -> bool;
    /// Whether the active source has an exact native index for this selected source track.
    fn maps_track(&self, selection: TrackSelection) -> bool;
}
