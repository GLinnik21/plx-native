//! plx_jellyfin: the Jellyfin layer of FinNative (see [`jellyfin`]).
//!
//! It is the `jellyfin` layer of `ci/module-layers.ini`: it uses `base`, `machine`, `platform`,
//! `net` and `plex` (for the common item model) and nothing above them.

pub mod jellyfin;
