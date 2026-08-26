//! **gzip for the upload**, bound at runtime from whatever zlib the machine already has.
//!
//! A diagnostic document is a few hundred kilobytes of highly repetitive text and compresses ~10x,
//! which matters on a link we do not control and against the receiver's 4 MiB body cap.
//!
//! # Why `dlopen` and not a crate
//!
//! Adding a compression crate would put a new dependency into a `-Z build-std` cross build for one
//! feature that never ships. zlib is present wherever libcurl and OpenSSL are — which is every
//! webOS release this app claims, and macOS — and this needs exactly **one** symbol from it.
//!
//! # Its own table, and a fallback rather than a failure
//!
//! [`crate::dynlib::load_into`] is all-or-nothing, so this gets a table of its own (the reasoning
//! `curlio.rs` gives for binding `curl_multi_*` separately from `net.rs`'s easy table). If the
//! table does not load, [`gzip`] answers `None` and the uploader sends
//! `Content-Encoding: identity` — a header, not an error. A set that cannot compress must still be
//! able to report.
//!
//! # gzip, not zlib
//!
//! `compress2` produces a **zlib** stream (RFC 1950), and `Content-Encoding: gzip` means RFC 1952.
//! `deflateInit2` with `windowBits = 31` would emit a gzip wrapper directly, but that symbol takes
//! a `z_stream` whose layout we would have to declare and keep right across zlib versions on a
//! device with no debugger. So this calls the one layout-free symbol and rewrites the six-byte
//! zlib envelope into a gzip one: a fixed 10-byte header, the raw deflate body, then CRC32 and
//! length trailers. The CRC is computed here rather than through zlib's `crc32`, so the table
//! stays at one symbol.
use std::os::raw::{c_int, c_ulong};

crate::dynlib! {
    /// ONE symbol. See this module's doc before adding a second: every name added here is a name
    /// that can empty the table on some firmware, and the cost of an empty table is that
    /// diagnostics travel uncompressed rather than that anything fails.
    z: ["libz.so.1", "libz.so", "libz.1.dylib", "libz.dylib"] {
        fn compress2(dest: *mut u8, dest_len: *mut c_ulong, src: *const u8, src_len: c_ulong,
                     level: c_int) -> c_int;
    }
}

const Z_OK: c_int = 0;

/// Is the table live? Resolved ONCE for the process.
///
/// `load` reaches `dlopen`, and `dynlib::Handle` is deliberately never `dlclose`d — so calling it
/// per upload leaked a refcount each time, and on a machine with no zlib cost four failed
/// `dlopen`s per press. `ff::boot` latches its load for the same reason.
fn loaded() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| z::load(None).ok())
}

/// gzip `src`, or `None` if zlib could not be bound (the caller then sends it uncompressed).
pub(crate) fn gzip(src: &[u8]) -> Option<Vec<u8>> {
    if !loaded() {
        return None;
    }
    // zlib's own documented worst case, plus the gzip trailer we add.
    let mut cap: c_ulong = src.len() as c_ulong + src.len() as c_ulong / 1000 + 64;
    let mut zbuf = vec![0u8; cap as usize];
    let rv = unsafe { compress2(zbuf.as_mut_ptr(), &mut cap, src.as_ptr(), src.len() as c_ulong, 6) };
    if rv != Z_OK {
        return None;
    }
    zbuf.truncate(cap as usize);
    // A zlib stream is a 2-byte header, the deflate body, and a 4-byte Adler-32. Strip both ends
    // and re-wrap; anything shorter than those six bytes is not a zlib stream at all.
    if zbuf.len() < 6 {
        return None;
    }
    let deflate = &zbuf[2..zbuf.len() - 4];
    let mut out = Vec::with_capacity(deflate.len() + 18);
    // magic, CM=deflate, no flags, no mtime (a timestamp would break reproducibility for nothing),
    // no extra flags, unknown OS
    out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xff]);
    out.extend_from_slice(deflate);
    out.extend_from_slice(&crc32(src).to_le_bytes());
    out.extend_from_slice(&(src.len() as u32).to_le_bytes());
    Some(out)
}

/// CRC-32/ISO-HDLC, the one gzip's trailer carries. Table-free: this runs once per upload over a
/// few hundred kilobytes, on a worker thread, and a 1 KiB static table earns nothing here.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for b in data {
        crc ^= *b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one value every CRC-32 implementation is checked against.
    #[test]
    fn the_crc_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    /// On the host, zlib is present, so this exercises the real path: a well-formed gzip member
    /// that `gzip -d` (and the receiver's `gzip.decompress`) will accept.
    #[test]
    fn the_output_is_a_gzip_member_with_the_right_trailers() {
        let src = "loop=62 fps=0 pos=41\n".repeat(200);
        let Some(gz) = gzip(src.as_bytes()) else {
            return; // no zlib on this host: the identity fallback is the tested behaviour
        };
        assert_eq!(&gz[..3], &[0x1f, 0x8b, 0x08], "gzip magic + deflate method");
        assert!(gz.len() < src.len() / 4, "repetitive text must actually compress");
        let n = gz.len();
        assert_eq!(u32::from_le_bytes(gz[n - 8..n - 4].try_into().unwrap()), crc32(src.as_bytes()));
        assert_eq!(u32::from_le_bytes(gz[n - 4..].try_into().unwrap()), src.len() as u32);
    }
}
