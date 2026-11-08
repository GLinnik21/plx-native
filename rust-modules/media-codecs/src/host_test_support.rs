//! Host fixture for the one bundled FFmpeg loader and ABI gate.

use crate::ffi::{avcodec, avcodec_version, avformat, avformat_version, avutil,
    avutil_version, swap_abi_for_test};

/// Keep the host binding active until all fixture readers have stopped.
#[must_use]
pub struct HostFfmpeg {
    previous_abi_ok: bool,
}

impl Drop for HostFfmpeg {
    fn drop(&mut self) {
        swap_abi_for_test(self.previous_abi_ok);
    }
}

/// Bind the staged host libraries in dependency order and verify their ABI majors.
/// A nested guard restores the prior gate instead of closing its outer binding.
pub fn bind_host_ffmpeg() -> HostFfmpeg {
    plx_base::testlock::assert_held("bind_host_ffmpeg");
    let dir = std::env::var_os("PLX_FFMPEG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pkg"));
    for (what, verdict) in [
        ("avutil", avutil::load(Some(&dir))),
        ("avcodec", avcodec::load(Some(&dir))),
        ("avformat", avformat::load(Some(&dir))),
    ] {
        assert!(
            matches!(verdict, plx_base::dynlib::Loaded::Ok(_)),
            "{what}: no host FFmpeg in {} — run `make check-ffmpeg`",
            dir.display()
        );
    }
    let majors = unsafe {
        (
            avformat_version() >> 16,
            avcodec_version() >> 16,
            avutil_version() >> 16,
        )
    };
    assert_eq!(
        majors,
        (63, 63, 61),
        "the staged FFmpeg is not the one ff.rs is built for"
    );
    HostFfmpeg {
        previous_abi_ok: swap_abi_for_test(true),
    }
}
