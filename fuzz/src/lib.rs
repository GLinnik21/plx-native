//! What the fuzz targets share: the finding-shape check, the mutator that keeps a bundle a bundle, and the
//! TEST ONLY keys the registry-index targets sign with ([`signing`]).
//!
//! Each target asserts only what the code under test promises (its module docs and the in-crate tests'
//! contracts): no panic, no unbounded work, and the invariants named in the target's own header. A
//! failed assertion here is a finding about the parser, not about the fuzzer.
//!
//! Every target links this crate, even one that calls nothing from it (`use plxnative_fuzz as _;`):
//! `build.rs` prints the host link lines (SDL2, OpenGL, the nanosvg object) for the package's library
//! target, and rustc passes them to a binary only through a library it links. Without the line, a
//! target that touches only `plx_data` links no OpenGL, and `plx_base` still names its symbols.

use plx_appkit::hud_format::bundle::{archive, valid_path, MAX_MEMBER_BYTES};
use plx_appkit::hud_format::{codes, Finding, MAX_NEAREST};

pub mod signing;

/// The lint shape every finding must have (`hud_format/tests.rs`, `assert_lint_shaped`): a code of the
/// table with its own rule and severity, an RFC 6901 pointer, a message and a hint, at most
/// [`MAX_NEAREST`] nearest names, and a member that is a valid package path when it names one.
pub fn lint_shaped(f: &Finding) {
    let code = codes::ALL
        .iter()
        .find(|c| c.id == f.code)
        .unwrap_or_else(|| panic!("`{}` is not a code of the table: {f:?}", f.code));
    assert_eq!(f.rule, code.rule, "a finding carries its code's rule: {f:?}");
    assert_eq!(f.severity, code.severity, "and its severity: {f:?}");
    assert!(well_formed_pointer(&f.pointer), "not an RFC 6901 pointer: {f:?}");
    assert!(!f.message.is_empty(), "no message: {f:?}");
    assert!(!f.hint.is_empty(), "no hint: {f:?}");
    assert!(f.nearest.len() <= MAX_NEAREST, "too many nearest: {f:?}");
    if let Some(member) = &f.file {
        assert!(valid_path(member), "a finding names a member that is not a package path: {f:?}");
    }
}

/// RFC 6901: empty, or `/` first and every `~` followed by `0` or `1`.
fn well_formed_pointer(pointer: &str) -> bool {
    if pointer.is_empty() {
        return true;
    }
    if !pointer.starts_with('/') {
        return false;
    }
    let bytes = pointer.as_bytes();
    bytes
        .iter()
        .enumerate()
        .all(|(i, &b)| b != b'~' || matches!(bytes.get(i + 1), Some(b'0') | Some(b'1')))
}

/// A small deterministic generator for the mutator's choices: libFuzzer hands each call a seed, and
/// the same seed must make the same mutation (xorshift64*).
struct Rng(u64);

impl Rng {
    fn new(seed: u32) -> Rng {
        Rng((u64::from(seed) << 32 | u64::from(seed)).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Member paths a mutation may add: names the format knows or ignores, and one it does not. All are
/// `valid_path` names (lowercase only): `archive::pack` refuses any other, and the mutation would add nothing.
const ADDED: &[&str] = &["assets/extra.svg", "assets/a.svg", "notice.txt", "license", "docs/readme.md", "extra.json"];

/// libFuzzer's custom mutator for every target that reads a bundle.
///
/// A byte flipped inside a STORE member breaks its CRC-32, and the reader refuses the archive before
/// the manifest, the HUD document or an SVG mask is parsed, so the byte-level mutator alone almost
/// never gets past the container. Three times in four this opens the input as a bundle, mutates ONE
/// thing in it (a member's bytes, a swap, a removal, an added member) and writes it back with
/// `archive::pack`, which seals the CRCs and the directory again; the rest of the time, and for any
/// input that is not a bundle, libFuzzer damages the raw bytes, which is what reaches the container's
/// own refusals.
pub fn mutate_bundle(data: &mut [u8], size: usize, max_size: usize, seed: u32) -> usize {
    let mut rng = Rng::new(seed);
    if rng.below(4) != 0 {
        if let Some(len) = reseal(data, size, max_size, &mut rng) {
            return len;
        }
    }
    libfuzzer_sys::fuzzer_mutate(data, size, max_size)
}

fn reseal(data: &mut [u8], size: usize, max_size: usize, rng: &mut Rng) -> Option<usize> {
    let mut files = archive::read(data.get(..size)?).ok()?;
    if files.is_empty() {
        return None;
    }
    match rng.below(8) {
        0..=4 => {
            let i = rng.below(files.len());
            let old = &files[i].bytes;
            let cap = (old.len() + 1024).min(MAX_MEMBER_BYTES);
            let mut buf = vec![0u8; cap];
            buf[..old.len()].copy_from_slice(old);
            let len = libfuzzer_sys::fuzzer_mutate(&mut buf, old.len(), cap);
            buf.truncate(len);
            files[i].bytes = buf.into();
        }
        5 => {
            let (i, j) = (rng.below(files.len()), rng.below(files.len()));
            let bytes = files[i].bytes.clone();
            files[j].bytes = bytes;
        }
        6 => {
            if files.len() < 2 {
                return None;
            }
            let i = rng.below(files.len());
            files.remove(i);
        }
        _ => {
            let from = rng.below(files.len());
            let path = ADDED[rng.below(ADDED.len())];
            let bytes = files[from].bytes.clone();
            files.push(archive::File::new(path, bytes));
        }
    }
    let packed = archive::pack(&files).ok()?;
    let out = data.get_mut(..packed.len()).filter(|_| packed.len() <= max_size)?;
    out.copy_from_slice(&packed);
    Some(packed.len())
}
