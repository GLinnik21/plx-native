//! Shared fixtures and helpers for the `route::plan` test modules split out below.


use super::*;


/// A library file's shape, for readability at the call sites below: (kbps, w, h).
pub(super) const UHD_REMUX: (i64, i64, i64) = (60000, 3840, 2160); // a 60 Mbps 4K rip

pub(super) const HD_BIG: (i64, i64, i64) = (30000, 1920, 1080); // the case the whole feature is about

pub(super) const HD_SMALL: (i64, i64, i64) = (3000, 1280, 720); // a 3 Mbit/s 720p episode

pub(super) const UNMEASURED: (i64, i64, i64) = (0, 0, 0); // PMS said nothing (a play straight off a shelf)


/// What `build_stream` computes, spelled once.
pub(super) fn allowed(
    link: Option<crate::plex::probe::Location>,
    q: Quality,
    src: (i64, i64, i64),
) -> crate::plex::LinkPolicy {
    let auto_original = q == Quality::Auto && link == Some(crate::plex::probe::Location::Local);
    flavors_allowed(
        crate::plex::link_policy(link),
        quality_policy(q, auto_original, src.0, src.1, src.2),
    )
}


pub(super) fn trk(id: i64, codec: &str, lang: &str, default: bool) -> crate::metadata::Stream {
    // `..Default::default()` for the rest, which is what that derive is FOR (see the comment
    // above `metadata::Stream`): this ladder is about id / codec / language / default, and a
    // fixture that spells out the technical fields it does not read would have to be revisited
    // every time the Track-information panel learns another one.
    crate::metadata::Stream {
        id,
        index: id,
        lang_code: lang.into(),
        codec: codec.into(),
        channels: 2,
        default,
        ..Default::default()
    }
}


/// Mark a track as the server's CURRENT pick (PMS `Stream.selected`) — the flag a pick made
/// on a phone / Plex Web / another TV arrives on.
pub(super) fn server_selected(mut s: crate::metadata::Stream) -> crate::metadata::Stream {
    s.selected = true;
    s
}


/// A subtitle stream, spelled out because the ordinal maths depends on `index` (container
/// order, which PMS may report out of document order) and on `external` (sidecars are not in
/// the container at all, so the client renderer cannot count them).
pub(super) fn sub(id: i64, index: i64, lang: &str, external: bool) -> crate::metadata::Stream {
    crate::metadata::Stream {
        index,
        external,
        ..trk(id, "srt", lang, false)
    }
}


pub(super) use crate::metadata::{Dovi, DvPresentation};


/// The two inputs of the boot-latched `/tmp/plxnative-nodv` diagnostic, named so assertions state
/// which signal they exercise. Capability and codec are passed separately; `DECLARED` alone does
/// not authorize a node on an unsupported/unknown set.
pub(super) const DECLARED: bool = true;

pub(super) const SILENT: bool = false;


/// An ordinary non-DV file: every DOVI field absent, which is what PMS sends for one.
pub(super) fn no_dv() -> Dovi {
    Dovi::default()
}

/// The four real shapes, spelled exactly as the dev server reports them (probed live
/// 2026-08-21 by sweeping all 540 movies and episodes on the dev PMS: 28 carry Dolby Vision,
/// 8 movies and 20 episodes — the numbers are not invented, and `p7`'s `bl_compat: 6` in
/// particular is why an `== 0` test is not enough).
pub(super) fn p5() -> Dovi {
    Dovi {
        present: true,
        profile: 5,
        bl_compat: 0,
        el_present: false,
        ..Dovi::NONE
    }
}

pub(super) fn p7() -> Dovi {
    Dovi {
        present: true,
        profile: 7,
        bl_compat: 6,
        el_present: true,
        ..Dovi::NONE
    }
}

pub(super) fn p8() -> Dovi {
    Dovi {
        present: true,
        profile: 8,
        bl_compat: 1,
        el_present: false,
        ..Dovi::NONE
    }
}
