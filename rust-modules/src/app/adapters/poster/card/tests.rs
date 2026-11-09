use super::*;
use plx_ui::hero_logo::LogoRung;
use plx_ui::theme;

const RED: [u8; 4] = [220, 20, 20, 255];
const GREEN: [u8; 4] = [20, 220, 20, 255];

fn solid(w: u32, h: u32, c: [u8; 4]) -> Rgba {
    Rgba { w, h, px: (0..w * h).flat_map(|_| c).collect() }
}
fn spec() -> CardSpec {
    CardSpec::deck(1)
}

/// What a fake serves for one part, and what it answers a refetch with.
struct Fake {
    cached: Option<Cached>,
    bg: Option<Got<Source>>,
    logo: Option<Got<Source>>,
    fresh_bg: Option<Rgba>,
    fresh_logo: Option<Rgba>,
    loads: Vec<Part>,
    refetches: Vec<Part>,
    discarded: bool,
    persisted: Option<Vec<u8>>,
}

impl Fake {
    /// A black 1920x1080 background and a 1200x240 logo of `logo`, both fresh.
    fn new(logo: [u8; 4]) -> Fake {
        Fake {
            cached: None,
            bg: Some(Got::Ok(Source { img: solid(1920, 1080, [0, 0, 0, 255]), stale: false })),
            logo: Some(Got::Ok(Source { img: solid(1200, 240, logo), stale: false })),
            fresh_bg: None,
            fresh_logo: None,
            loads: Vec::new(),
            refetches: Vec::new(),
            discarded: false,
            persisted: None,
        }
    }
}

impl CardIo for Fake {
    fn cached(&mut self) -> Option<Cached> {
        self.cached.take()
    }
    fn discard(&mut self) {
        self.discarded = true;
    }
    fn load(&mut self, part: Part) -> Got<Source> {
        self.loads.push(part);
        match part {
            Part::Background => self.bg.take(),
            Part::Logo => self.logo.take(),
        }
        .expect("a part loaded twice")
    }
    fn refetch(&mut self, part: Part) -> Option<Rgba> {
        self.refetches.push(part);
        match part {
            Part::Background => self.fresh_bg.take(),
            Part::Logo => self.fresh_logo.take(),
        }
    }
    fn persist(&mut self, png: &[u8]) {
        self.persisted = Some(png.to_vec());
    }
}

fn pixel(img: &Rgba, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * img.w + x) * 4) as usize;
    [img.px[i], img.px[i + 1], img.px[i + 2]]
}

/// The pixel at the middle of the logo as the spec places it: centred, bottom edge on `logo_bottom`.
fn logo_middle(img: &Rgba) -> [u8; 3] {
    let s = spec();
    let (_, lh) = s.logo_size(1200, 240).expect("a sized logo");
    pixel(img, s.w / 2, s.logo_bottom - lh / 2)
}

fn is_final(b: &Bake) -> bool {
    matches!(b.got, Got::Final)
}
fn image(b: Bake) -> Rgba {
    match b.got {
        Got::Ok(img) => img,
        Got::Final => panic!("expected an image, got Final"),
        Got::Transient => panic!("expected an image, got Transient"),
    }
}

#[test]
fn a_cold_bake_composes_one_opaque_card_and_persists_its_png() {
    let mut io = Fake::new(GREEN);
    let out = image(bake(&mut io, &spec()));
    assert_eq!((out.w, out.h), (420, 236));
    assert!(out.px.chunks(4).all(|p| p[3] == 255), "opaque");
    assert_eq!(logo_middle(&out), [GREEN[0], GREEN[1], GREEN[2]], "the logo is in the card");
    assert_eq!(io.loads, [Part::Background, Part::Logo]);
    assert!(io.refetches.is_empty(), "fresh sources are not refetched");
    let png = io.persisted.expect("a whole bake is persisted");
    let (w, h, _) = plx_gfx::img::img_decode_owned(&png).expect("a PNG");
    assert_eq!((w, h), (420, 236));
}

#[test]
fn a_warm_hit_decodes_the_baked_png_and_loads_nothing() {
    let mut cold = Fake::new(GREEN);
    let _ = bake(&mut cold, &spec());
    let png = cold.persisted.unwrap();

    let mut io = Fake::new(GREEN);
    io.cached = Some(Cached { bytes: png.clone(), stale: true });
    let b = bake(&mut io, &spec());
    assert!(b.stale, "a stale hit is served and flagged for the refresh lane");
    assert_eq!(image(b).w, 420);
    assert!(io.loads.is_empty() && io.persisted.is_none() && !io.discarded);
}

#[test]
fn a_cached_card_that_is_not_the_specs_size_or_does_not_decode_is_rebaked() {
    let other = plx_gfx::img::img_encode_png(300, 169, &solid(300, 169, RED).px).unwrap();
    for bytes in [other, b"garbage".to_vec()] {
        let mut io = Fake::new(GREEN);
        io.cached = Some(Cached { bytes, stale: false });
        assert!(!is_final(&bake(&mut io, &spec())));
        assert!(io.discarded);
        assert!(io.persisted.is_some());
    }
}

#[test]
fn a_part_that_settles_or_fails_makes_the_whole_card_a_miss_and_persists_nothing() {
    let mut io = Fake::new(GREEN);
    io.bg = Some(Got::Final);
    assert!(is_final(&bake(&mut io, &spec())) && io.persisted.is_none());

    let mut io = Fake::new(GREEN);
    io.logo = Some(Got::Final);
    assert!(is_final(&bake(&mut io, &spec())) && io.persisted.is_none(), "a 404 logo: the Type rung");

    let mut io = Fake::new(GREEN);
    io.bg = Some(Got::Transient);
    assert!(matches!(bake(&mut io, &spec()).got, Got::Transient), "an unreachable server retries");
    assert!(io.persisted.is_none());
}

// ── stale sources ───────────────────────────────────────────────────────────────────────────────

/// `load_art` serves a stale logo as a hit; the composite persists for a day or more, so the first
/// bake fetches the logo from the server and composes from THAT.
#[test]
fn a_stale_logo_is_refetched_and_the_composite_is_made_from_the_fresh_one() {
    let mut io = Fake::new(RED);
    io.logo = Some(Got::Ok(Source { img: solid(1200, 240, RED), stale: true }));
    io.fresh_logo = Some(solid(1200, 240, GREEN));
    let b = bake(&mut io, &spec());
    assert!(!b.stale);
    let out = image(b);
    assert_eq!(io.refetches, [Part::Logo], "only the stale part is refetched");
    assert_eq!(logo_middle(&out), [GREEN[0], GREEN[1], GREEN[2]], "the fresh logo, not the stale one");
    assert!(io.persisted.is_some(), "a whole bake is persisted");
}

/// If the server cannot be reached the card is still delivered, from the stale bytes, but it is NOT
/// persisted: the next demand bakes again rather than freezing a degraded card on disk.
#[test]
fn a_stale_logo_that_cannot_be_refetched_composes_from_the_stale_bytes_and_persists_nothing() {
    let mut io = Fake::new(RED);
    io.logo = Some(Got::Ok(Source { img: solid(1200, 240, RED), stale: true }));
    io.fresh_logo = None;
    let out = image(bake(&mut io, &spec()));
    assert_eq!(logo_middle(&out), [RED[0], RED[1], RED[2]]);
    assert_eq!(io.refetches, [Part::Logo]);
    assert!(io.persisted.is_none(), "a degraded composite is shown, never filed");

    let mut io = Fake::new(RED);
    io.bg = Some(Got::Ok(Source { img: solid(1920, 1080, [0, 0, 0, 255]), stale: true }));
    let _ = image(bake(&mut io, &spec()));
    assert_eq!(io.refetches, [Part::Background]);
    assert!(io.persisted.is_none(), "the same for a stale background");
}

#[test]
fn a_rebake_refetches_both_sources_and_files_a_composite_with_the_new_logo() {
    let mut io = Fake::new(RED);
    io.fresh_bg = Some(solid(1920, 1080, [0, 0, 0, 255]));
    io.fresh_logo = Some(solid(1200, 240, GREEN));
    rebake(&mut io, &spec());
    assert_eq!(io.refetches, [Part::Background, Part::Logo]);
    assert!(io.loads.is_empty(), "the cached logo is never read");
    let png = io.persisted.expect("filed");
    let (w, h, px) = plx_gfx::img::img_decode_owned(&png).unwrap();
    assert_eq!((w, h), (420, 236));
    let out = Rgba { w, h, px };
    assert_eq!(logo_middle(&out), [GREEN[0], GREEN[1], GREEN[2]], "the NEW logo is in the output");
}

#[test]
fn a_rebake_whose_server_cannot_be_reached_writes_nothing() {
    let mut io = Fake::new(RED);
    io.fresh_bg = None;
    io.fresh_logo = Some(solid(1200, 240, GREEN));
    rebake(&mut io, &spec());
    assert!(io.persisted.is_none());

    let mut io = Fake::new(RED);
    io.fresh_bg = Some(solid(1920, 1080, [0, 0, 0, 255]));
    io.fresh_logo = None;
    rebake(&mut io, &spec());
    assert!(io.persisted.is_none(), "a half-fresh composite is not filed");
}

// ── the pin ─────────────────────────────────────────────────────────────────────────────────────

/// What moves the composite's pixels: the kind that keys the persisted PNG, the spec, and the
/// sizing tokens `compose` reads. A change to any of them must change [`CARD_KIND`] in the same
/// commit, or every persisted card keeps showing the old look until its art path moves.
#[test]
fn the_card_kind_is_pinned_with_what_moves_pixels() {
    assert_eq!(CARD_KIND, "card.1");
    assert_eq!(spec(), CardSpec { w: 420, h: 236, logo_bottom: 188, rung: LogoRung::Card });
    assert_eq!(
        (theme::logo::CARD_AREA, theme::logo::CARD_H_MAX, theme::logo::CARD_W_MAX, theme::logo::CARD_BOTTOM_INSET),
        (9_600.0, 56.0, 270.0, 48.0)
    );
}

/// The identity names the server's machine, the logo's rating key and the WHOLE art path, and the
/// kind: no profile, and two art paths under one rating key are two cards.
#[test]
fn two_art_paths_under_one_rating_key_are_two_baked_identities() {
    let key = |art: &str| plx_platform::imgcache::classify_baked("machine:abc", CARD_KIND, "605", art, 420, 236);
    assert_ne!(key("/library/metadata/605/art/1"), key("/library/metadata/605/art/2"));
    assert_eq!(key("/library/metadata/605/art/1"), key("/library/metadata/605/art/1"));
    assert_ne!(
        key("/library/metadata/605/art/1"),
        plx_platform::imgcache::classify_baked("machine:abc", "card.0", "605", "/library/metadata/605/art/1", 420, 236)
    );
}
