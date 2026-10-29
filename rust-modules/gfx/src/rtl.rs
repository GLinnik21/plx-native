//! **Arabic and Hebrew for the UI's text renderer**: the half of it SDL2_ttf 2.0.x cannot do.
//!
//! Every television draws the application's text with its own SDL2_ttf 2.0.10 or 2.0.14: one face
//! per call, a left-to-right advance loop keyed on codepoints, and no shaper. That is enough for
//! Latin, Cyrillic, Greek, CJK and the emoji face, and it is structurally wrong for two scripts a
//! Plex library routinely carries. Arabic letters change shape with their neighbours (contextual
//! joining, the lam-alef ligature, marks placed by the font's anchors), and Arabic and Hebrew both
//! run right to left inside lines that may also hold Latin words and numbers (the Unicode
//! Bidirectional Algorithm, UAX #9). Drawing either through SDL2_ttf with a face that merely HAS
//! the glyphs prints fluent-looking wrong text (letters unjoined, words backwards), which is
//! worse than the boxes it replaces, and is why the chain carried no such face until this module.
//!
//! **The engine is the one the subtitles already use.** `libass-plx` (`ci/build-libass.py`) links a
//! pinned FreeType, HarfBuzz and FriBidi statically and exports only its `plx_ass_*` facade; ABI 4
//! adds `plx_ass_text_*` (`src/ass_text.c`). This module binds that surface:
//!
//! * [`bidi`]: FriBidi's resolved levels (after rule L1), the visual order (rule L2) and the
//!   mirrored brackets of one line. `text.rs` groups the line into runs from these.
//! * [`Face`]: one bundled face at one pixel size. A run is shaped by HarfBuzz from its LOGICAL
//!   text plus a direction, rasterized by FreeType, and returned as an 8-bit coverage box with its
//!   own baseline; `text.rs` composites it beside the SDL2_ttf runs on the shared baseline.
//!
//! Runs of every other script stay on SDL2_ttf, byte for byte: a Latin, Cyrillic or CJK library
//! never loads this library for its labels, and a label's Latin words beside an Arabic one are
//! still Inter drawn by the television's own renderer.
//!
//! **Failure is the old picture, never a wrong one.** If the library is missing, refuses its ABI or
//! a face will not open, [`available`] is false and `text.rs` treats the Arabic and Hebrew links as
//! empty: those characters fall through to the base face's boxes, exactly as before this module.
//!
//! Main thread only, like every SDL2_ttf call in `text.rs`: the facade's FreeType library and
//! HarfBuzz faces are this module's own, separate from libass's on the subtitle worker.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::OnceLock;

/// `PlxTextRun` in `include/ass.h`.
#[repr(C)]
struct NativeRun {
    width: c_int,
    ascent: c_int,
    height: c_int,
    alpha: *const u8,
}

/// The facade ABI this file is written against (`PLX_ASS_ABI_VERSION`); any other is not used.
const NATIVE_ABI: u32 = 4;
/// `plx_ass_text_bidi`'s own bound, mirrored so an over-long line is refused here without a call.
const BIDI_MAX_CHARS: usize = 16_384;

plx_base::dynlib! {
    native_text: ["libass-plx-host.so.0", "libass-plx.so.0", "libass-plx.0.dylib"] {
        fn plx_ass_abi_version() -> u32;
        fn plx_ass_text_open(path: *const c_char, px: c_int) -> *mut c_void;
        fn plx_ass_text_close(text: *mut c_void);
        fn plx_ass_text_measure(text: *mut c_void, utf8: *const c_char, len: usize, rtl: c_int,
                                out: *mut NativeRun) -> c_int;
        fn plx_ass_text_render(text: *mut c_void, utf8: *const c_char, len: usize, rtl: c_int,
                               out: *mut NativeRun) -> c_int;
        fn plx_ass_text_bidi(chars: *const u32, n: c_int, levels: *mut u8, visual: *mut i32,
                             mirrored: *mut u32) -> c_int;
    }
}

/// Where the bundled library and faces live: beside the binary on the television and in the
/// simulators, `pkg/` for the host unit tests (the same split `plx_media`'s ASS renderer makes).
pub(crate) fn asset_dir() -> std::path::PathBuf {
    #[cfg(any(test, feature = "test-support"))]
    {
        std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg"))
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        plx_base::paths::app_dir().to_path_buf()
    }
}

/// Is the shaping library loaded at the expected ABI? Loads it on the first ask, once.
pub(crate) fn available() -> bool {
    static LOADED: OnceLock<bool> = OnceLock::new();
    *LOADED.get_or_init(|| {
        let ok = native_text::load(Some(&asset_dir())).ok()
            && unsafe { plx_ass_abi_version() } == NATIVE_ABI;
        plx_base::eventlog::log(&format!("rtl: shaping library {}", if ok { "loaded" } else { "unavailable" }));
        ok
    })
}

/// Can `cp` make any part of a line right-to-left? The cheap gate in front of [`bidi`]: true for
/// the strong right-to-left and Arabic-number ranges (Hebrew, Arabic, Syriac, Thaana, NKo and
/// their supplements and presentation forms) and the explicit right-to-left controls. A line
/// without one is entirely left to right by rules P2/P3 and W1-W7, so it never needs FriBidi.
pub(crate) fn may_be_rtl(cp: u32) -> bool {
    matches!(cp,
        0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF
        // RLM, RLE, RLO, RLI (ALM, U+061C, is inside the Arabic block)
        | 0x200F | 0x202B | 0x202E | 0x2067)
}

/// A line's bidirectional analysis, indexed by CHARACTER (not byte).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Bidi {
    /// Resolved embedding level per character, after rule L1. Odd is right to left.
    pub levels: Vec<u8>,
    /// `visual[i]` is the character drawn at visual position `i`, counted from the left.
    pub visual: Vec<usize>,
    /// The character to draw for each logical one: its mirrored bracket where its level is odd.
    pub mirrored: Vec<char>,
}

/// FriBidi over one line. `None` when the line needs no reordering at all (no character can be
/// right to left, see [`may_be_rtl`]), when the library is unavailable, or when it refuses.
pub(crate) fn bidi(s: &str) -> Option<Bidi> {
    if !s.chars().any(|c| may_be_rtl(c as u32)) || !available() {
        return None;
    }
    let chars: Vec<u32> = s.chars().map(|c| c as u32).collect();
    if chars.len() > BIDI_MAX_CHARS {
        return None;
    }
    let n = chars.len();
    let (mut levels, mut visual, mut mirrored) = (vec![0u8; n], vec![0i32; n], vec![0u32; n]);
    let r = unsafe {
        plx_ass_text_bidi(chars.as_ptr(), n as c_int, levels.as_mut_ptr(), visual.as_mut_ptr(),
                          mirrored.as_mut_ptr())
    };
    if r < 0 {
        return None;
    }
    let visual: Vec<usize> = visual.into_iter().map(|v| v as usize).collect();
    // A permutation of 0..n, or the reply is not one this module can use.
    let mut seen = vec![false; n];
    for &v in &visual {
        if v >= n || std::mem::replace(&mut seen[v], true) {
            return None;
        }
    }
    let mirrored = mirrored
        .iter()
        .zip(s.chars())
        .map(|(&m, c)| char::from_u32(m).unwrap_or(c))
        .collect();
    Some(Bidi { levels, visual, mirrored })
}

/// One shaped run's box: `alpha` is `w * h` coverage bytes (empty from [`Face::measure`]), the
/// baseline `ascent` rows below its top.
pub(crate) struct Shaped {
    pub w: i32,
    pub h: i32,
    pub ascent: i32,
    pub alpha: Vec<u8>,
}

/// One bundled face at one pixel size, opened by the shaping library.
pub(crate) struct Face(*mut c_void);

impl Face {
    /// `None` if the library is unavailable or the file will not open at `px`.
    pub(crate) fn open(path: &std::path::Path, px: i32) -> Option<Face> {
        if !available() {
            return None;
        }
        let c = CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
        let p = unsafe { plx_ass_text_open(c.as_ptr(), px) };
        (!p.is_null()).then_some(Face(p))
    }

    fn call(&self, s: &str, rtl: bool, render: bool) -> Option<Shaped> {
        let mut out = NativeRun { width: 0, ascent: 0, height: 0, alpha: std::ptr::null() };
        let f = if render { plx_ass_text_render } else { plx_ass_text_measure };
        let r = unsafe { f(self.0, s.as_ptr().cast(), s.len(), rtl as c_int, &mut out) };
        if r != 0 || out.width < 0 || out.height < 0 {
            return None;
        }
        let alpha = if render && out.width > 0 && out.height > 0 && !out.alpha.is_null() {
            unsafe { std::slice::from_raw_parts(out.alpha, (out.width * out.height) as usize) }.to_vec()
        } else {
            Vec::new()
        };
        Some(Shaped { w: out.width, h: out.height, ascent: out.ascent, alpha })
    }

    /// The box `s` (one directional run, logical order) would render into. It runs the same layout
    /// pass as [`Self::render`], rasterizing every glyph, because the box is the ink's extent; it
    /// stops before compositing the coverage buffer, so it returns no alpha.
    pub(crate) fn measure(&self, s: &str, rtl: bool) -> Option<Shaped> {
        self.call(s, rtl, false)
    }

    /// Shape and rasterize `s` (one directional run, logical order).
    pub(crate) fn render(&self, s: &str, rtl: bool) -> Option<Shaped> {
        self.call(s, rtl, true)
    }

    /// Hand the handle to a process-lifetime cache slot (`text.rs` keeps one per face and size,
    /// as it does every SDL2_ttf face).
    pub(crate) fn into_raw(self) -> *mut c_void {
        let p = self.0;
        std::mem::forget(self);
        p
    }

    /// Use a cached handle without taking it back.
    ///
    /// # Safety
    /// `p` must come from [`Face::into_raw`]; cached handles are never closed.
    pub(crate) unsafe fn borrow_raw(p: *mut c_void) -> std::mem::ManuallyDrop<Face> {
        std::mem::ManuallyDrop::new(Face(p))
    }
}

impl Drop for Face {
    fn drop(&mut self) {
        unsafe { plx_ass_text_close(self.0) };
    }
}

#[cfg(test)]
mod native_tests {
    //! Against the HOST build of the bundle (`make libass-host` stages `pkg/libass-plx-host.so.0`)
    //! and the shipped faces in `pkg/`. Ignored by default like `plx_media`'s native ASS tests;
    //! `make check-ass` runs them with `--include-ignored`.
    use super::*;

    const NEEDS: &str = "needs pkg/libass-plx-host.so.0 (make libass-host)";

    fn pkg(name: &str) -> std::path::PathBuf {
        asset_dir().join(name)
    }

    fn face(name: &str, px: i32) -> Face {
        assert!(available(), "the shaping library did not load from {}", asset_dir().display());
        Face::open(&pkg(name), px).unwrap_or_else(|| panic!("{name} did not open at {px} px"))
    }

    fn idx(s: &str, c: char) -> usize {
        s.chars().position(|x| x == c).unwrap()
    }

    /// Columns of `run` holding any ink, left to right.
    fn inked_columns(run: &Shaped) -> Vec<bool> {
        (0..run.w as usize)
            .map(|x| (0..run.h as usize).any(|y| run.alpha[y * run.w as usize + x] > 24))
            .collect()
    }

    /// The topmost inked row, or `h` for a blank box.
    fn ink_top(run: &Shaped) -> usize {
        (0..run.h as usize)
            .find(|&y| (0..run.w as usize).any(|x| run.alpha[y * run.w as usize + x] > 24))
            .unwrap_or(run.h as usize)
    }

    #[test]
    #[ignore = "needs pkg/libass-plx-host.so.0 (make libass-host)"]
    fn an_arabic_word_reads_right_to_left() {
        let _ = NEEDS;
        let b = bidi("سلام").expect("an Arabic word needs bidi");
        assert_eq!(b.levels, vec![1, 1, 1, 1]);
        assert_eq!(b.visual, vec![3, 2, 1, 0], "the first letter is drawn at the right");
    }

    /// UAX #9 by hand: the paragraph is left to right (first strong is `B`). W2 turns the `2`
    /// after Arabic into an Arabic number, N1 puts the space before it at the Arabic level, I1
    /// lifts the number to 2. So the line draws as `Breaking Bad: 2 ةقلحلا`. Read right to left,
    /// the Arabic says "الحلقة 2" with its number where an Arabic reader expects it.
    #[test]
    #[ignore = "needs pkg/libass-plx-host.so.0 (make libass-host)"]
    fn a_latin_line_keeps_its_order_and_reverses_only_the_arabic() {
        let s = "Breaking Bad: الحلقة 2";
        let b = bidi(s).unwrap();
        let two = idx(s, '2');
        let first_ar = idx(s, 'ا');
        assert!(b.levels[..first_ar].iter().all(|&l| l == 0), "{:?}", b.levels);
        assert_eq!(b.levels[two], 2);
        assert_eq!(&b.visual[..first_ar], &(0..first_ar).collect::<Vec<_>>()[..]);
        assert_eq!(b.visual[first_ar], two, "the number sits left of the Arabic word");
        assert_eq!(*b.visual.last().unwrap(), first_ar, "the word's first letter is rightmost");
    }

    #[test]
    #[ignore = "needs pkg/libass-plx-host.so.0 (make libass-host)"]
    fn an_arabic_line_takes_its_direction_from_its_first_letter() {
        let s = "الموسم 2";
        let b = bidi(s).unwrap();
        assert_eq!(b.visual[0], idx(s, '2'), "in a right-to-left line the trailing number is leftmost");
        assert_eq!(*b.visual.last().unwrap(), 0);
    }

    /// A right-to-left mark in front makes the line right to left even when a Latin word opens it.
    /// `TextView` keeps a wrapped line in its paragraph's direction this way.
    #[test]
    #[ignore = "needs pkg/libass-plx-host.so.0 (make libass-host)"]
    fn a_leading_rlm_makes_a_latin_led_line_right_to_left() {
        let s = "\u{200F}Netflix عرض";
        let b = bidi(s).unwrap();
        let n = idx(s, 'N');
        assert_eq!(b.levels[n], 2);
        assert_eq!(b.visual[0], idx(s, 'ض'), "the Arabic word is at the left");
        let latin: Vec<usize> = b.visual.iter().copied().filter(|&i| (n..n + 7).contains(&i)).collect();
        assert_eq!(latin, (n..n + 7).collect::<Vec<_>>(), "the Latin word still reads left to right");
        assert_eq!(*b.visual.last().unwrap(), 0, "the mark is at the far right, where the line starts");
    }

    #[test]
    #[ignore = "needs pkg/libass-plx-host.so.0 (make libass-host)"]
    fn brackets_mirror_inside_right_to_left_text() {
        let b = bidi("(سلام)").unwrap();
        assert_eq!(b.levels, vec![1; 6]);
        assert_eq!(b.mirrored[0], ')');
        assert_eq!(b.mirrored[5], '(');
        assert_eq!(b.visual, vec![5, 4, 3, 2, 1, 0]);
    }

    #[test]
    #[ignore = "needs pkg/libass-plx-host.so.0 (make libass-host)"]
    fn trailing_spaces_return_to_the_paragraph_level() {
        let b = bidi("abc سلام  ").unwrap();
        assert_eq!(&b.levels[8..], &[0, 0], "rule L1");
    }

    /// Needs no library: the gate in front of FriBidi answers it.
    #[test]
    fn a_line_with_nothing_right_to_left_skips_the_library() {
        assert_eq!(bidi("Breaking Bad (2008) — Ünïcödé, Кириллица, 日本語"), None);
    }

    /// Joined Arabic has no gap inside a word: the connecting strokes run from the first inked
    /// column to the last. Unshaped isolated forms side by side leave empty columns between letters.
    #[test]
    #[ignore = "needs pkg/libass-plx-host.so.0 (make libass-host)"]
    fn arabic_letters_join() {
        for bold in [false, true] {
            let f = face(if bold { "appfont-arabic-bold.ttf" } else { "appfont-arabic.ttf" }, 28);
            let run = f.render("بببب", true).unwrap();
            let cols = inked_columns(&run);
            let (a, z) = (cols.iter().position(|&c| c).unwrap(), cols.iter().rposition(|&c| c).unwrap());
            assert!(cols[a..=z].iter().all(|&c| c), "bold={bold}: a gap inside a joined word: {cols:?}");
        }
    }

    /// Right to left, the FIRST letter is drawn at the RIGHT: alef is the tallest letter, so the
    /// top of the ink of "اب" (alef then beh) is in its right half, and of "با" in its left.
    #[test]
    #[ignore = "needs pkg/libass-plx-host.so.0 (make libass-host)"]
    fn a_right_to_left_run_starts_at_the_right() {
        let f = face("appfont-arabic.ttf", 40);
        for (s, right) in [("اب", true), ("با", false)] {
            let run = f.render(s, true).unwrap();
            let top = ink_top(&run);
            let xs: Vec<usize> = (0..run.w as usize).filter(|&x| run.alpha[top * run.w as usize + x] > 24).collect();
            let mid = xs.iter().sum::<usize>() / xs.len();
            assert_eq!(mid > run.w as usize / 2, right, "{s}: alef's top at x={mid} of {}", run.w);
        }
    }

    /// Hebrew has no joining, so a right-to-left run is exactly the reversed left-to-right one.
    #[test]
    #[ignore = "needs pkg/libass-plx-host.so.0 (make libass-host)"]
    fn a_hebrew_run_is_its_letters_in_reverse() {
        let f = face("appfont-hebrew.ttf", 32);
        let rtl = f.render("שלום", true).unwrap();
        let ltr = f.render("םולש", false).unwrap();
        assert_eq!((rtl.w, rtl.h, rtl.ascent), (ltr.w, ltr.h, ltr.ascent));
        assert!(rtl.alpha == ltr.alpha, "RTL shaping is not the reversed LTR drawing");
    }

    /// Vowel marks are positioned by the font, not dropped: the vowelled word inks higher than the
    /// bare one, and the box grows to hold it rather than cutting it.
    #[test]
    #[ignore = "needs pkg/libass-plx-host.so.0 (make libass-host)"]
    fn vowel_marks_are_drawn_above_and_never_clipped() {
        let f = face("appfont-arabic.ttf", 32);
        let bare = f.render("محمد", true).unwrap();
        let marked = f.render("مُحَمَّد", true).unwrap();
        let rise = |r: &Shaped| r.ascent as i64 - ink_top(r) as i64;
        assert!(rise(&marked) > rise(&bare), "marks did not rise above the letters");
        assert_eq!(bare.w, marked.w, "marks advance nothing");
    }

    #[test]
    #[ignore = "needs pkg/libass-plx-host.so.0 (make libass-host)"]
    fn measure_and_render_agree_on_the_box() {
        for (name, s) in [
            ("appfont-arabic.ttf", "مسلسلات عربية ٢٠٢٤"),
            ("appfont-arabic-bold.ttf", "لا إله"),
            ("appfont-hebrew.ttf", "סדרות טלוויזיה"),
            ("appfont-hebrew-bold.ttf", "שָׁלוֹם"),
        ] {
            for px in [22, 28, 40, 72] {
                let f = face(name, px);
                let m = f.measure(s, true).unwrap();
                let r = f.render(s, true).unwrap();
                assert_eq!((m.w, m.h, m.ascent), (r.w, r.h, r.ascent), "{name} {px}px {s}");
                assert!(m.alpha.is_empty());
                assert_eq!(r.alpha.len(), (r.w * r.h) as usize);
            }
        }
    }

    #[test]
    #[ignore = "needs pkg/libass-plx-host.so.0 (make libass-host)"]
    fn the_box_is_the_ink_not_the_faces_line_box() {
        let px = 40;
        let f = face("appfont-arabic.ttf", px);
        let alef = f.render("ا", true).unwrap();
        // alef is 0.760 em tall and sits on the baseline: its box is its stroke, give or take a
        // row of anti-aliasing, nowhere near Noto Kufi's 1.282 em ascender or 0.615 em descender
        assert!((alef.ascent - (0.760 * px as f32) as i32).abs() <= 1, "alef ascent {}", alef.ascent);
        assert!(alef.h - alef.ascent <= 1, "alef reaches {} rows below", alef.h - alef.ascent);
        // a final yeh descends about 0.45 em, deeper than Inter's 0.241 em descender
        let yeh = f.render("في", true).unwrap();
        assert!(yeh.h - yeh.ascent >= (0.40 * px as f32) as i32, "yeh below {}", yeh.h - yeh.ascent);
        let spaces = f.render("   ", true).unwrap();
        assert!(spaces.w > 0, "spaces advance");
        assert_eq!(spaces.h, 0, "and draw nothing");
        assert!(spaces.alpha.is_empty());
    }

    #[test]
    #[ignore = "needs pkg/libass-plx-host.so.0 (make libass-host)"]
    fn the_facade_refuses_what_it_cannot_do() {
        assert!(available());
        assert!(Face::open(&pkg("appfont-arabic.ttf"), 0).is_none());
        assert!(Face::open(&pkg("appfont-arabic.ttf"), 513).is_none());
        assert!(Face::open(&pkg("no-such-face.ttf"), 28).is_none());
        let f = face("appfont-arabic.ttf", 28);
        assert!(f.measure(&"ب".repeat(2049), true).is_none(), "a run over 4096 bytes");
        let empty = f.measure("", true).unwrap();
        assert_eq!(empty.w, 0);
        let n = 0;
        let r = unsafe {
            plx_ass_text_bidi([0x0628u32].as_ptr(), n, [0u8].as_mut_ptr(), [0i32].as_mut_ptr(),
                              [0u32].as_mut_ptr())
        };
        assert_eq!(r, -1);
    }
}
