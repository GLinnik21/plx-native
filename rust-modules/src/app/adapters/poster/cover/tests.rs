use super::*;
use plx_plex::plex::watchlist::WatchItem;
use std::collections::HashMap;

const MIN: (u32, u32) = (420, 236);
const URL: &str = "https://metadata-static.plex.tv/a/cover/0123456789abcdef.jpg";

fn png(w: u32, h: u32) -> Vec<u8> {
    let px: Vec<u8> = (0..w * h).flat_map(|_| [30u8, 60, 90, 255]).collect();
    plx_gfx::img::img_encode_png(w, h, &px).expect("encodes")
}

fn item(cover: Option<&str>) -> CatalogItem {
    CatalogItem { item: WatchItem::default(), cover_art: cover.map(str::to_owned), ..Default::default() }
}

/// A fake that behaves like the disk tier for the marker (an EMPTY body is refused, as
/// `imgcache::write_locked` refuses it) and counts every call that would touch the world.
struct Fake {
    cached: Option<Cached>,
    tier: HashMap<&'static str, Vec<u8>>,
    marker_stale: bool,
    identified: bool,
    unreachable: bool,
    read: Option<Result<CatalogItem, WatchlistError>>,
    fetch: Option<ArtFetch>,
    reads: usize,
    fetches: Vec<String>,
    discarded: bool,
    persisted: Vec<Vec<u8>>,
}

impl Fake {
    fn new() -> Fake {
        Fake {
            cached: None,
            tier: HashMap::new(),
            marker_stale: false,
            identified: true,
            unreachable: false,
            read: None,
            fetch: None,
            reads: 0,
            fetches: Vec::new(),
            discarded: false,
            persisted: Vec::new(),
        }
    }
    fn answering(read: Result<CatalogItem, WatchlistError>, fetch: Option<ArtFetch>) -> Fake {
        Fake { read: Some(read), fetch, ..Fake::new() }
    }
    fn markers(&self) -> usize {
        usize::from(self.tier.contains_key("marker"))
    }
    /// No read, no fetch, nothing persisted of either kind: the bake sent and wrote nothing.
    fn untouched(&self) -> bool {
        self.reads == 0 && self.fetches.is_empty() && self.persisted.is_empty() && self.markers() == 0
    }
}

impl CoverIo for Fake {
    fn cached(&mut self) -> Option<Cached> {
        self.cached.take()
    }
    fn discard(&mut self) {
        self.discarded = true;
    }
    fn marker(&mut self) -> Option<bool> {
        (self.tier.get("marker").map(Vec::as_slice) == Some(NO_COVER_MARKER)).then_some(self.marker_stale)
    }
    fn identified(&mut self) -> bool {
        self.identified
    }
    fn discover_unreachable(&mut self) -> bool {
        self.unreachable
    }
    fn read(&mut self) -> Result<CatalogItem, WatchlistError> {
        self.reads += 1;
        self.read.take().expect("an unexpected Discover read")
    }
    fn fetch(&mut self, url: &str) -> ArtFetch {
        self.fetches.push(url.to_owned());
        self.fetch.take().expect("an unexpected image fetch")
    }
    fn persist(&mut self, bytes: &[u8]) {
        self.persisted.push(bytes.to_vec());
    }
    fn persist_marker(&mut self, body: &[u8]) {
        if !body.is_empty() {
            self.tier.insert("marker", body.to_vec());
        }
    }
}

fn is_final(b: &Bake) -> bool {
    matches!(b.got, Got::Final)
}
/// A `Final` the miss set keeps for the whole identity (a verdict), as against one that expires
/// with the Discover memo (a read that did not answer).
fn is_lasting_final(b: &Bake) -> bool {
    is_final(b) && b.lasting
}
fn image(b: Bake) -> Rgba {
    match b.got {
        Got::Ok(img) => img,
        Got::Final => panic!("expected an image, got Final"),
        Got::Transient => panic!("expected an image, got Transient"),
    }
}

// ── disk first ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn a_cached_cover_is_a_hit_that_reads_and_fetches_nothing() {
    let mut io = Fake::new();
    io.cached = Some(Cached { bytes: png(420, 236), stale: false });
    let b = bake(&mut io, MIN);
    assert!(!b.stale);
    let img = image(b);
    assert_eq!((img.w, img.h), (420, 236));
    assert!(io.untouched() && !io.discarded);

    let mut io = Fake::new();
    io.cached = Some(Cached { bytes: png(420, 236), stale: true });
    assert!(bake(&mut io, MIN).stale, "a stale hit is served, and flagged for the refresh lane");
    assert!(io.untouched());
}

/// `minSize=1` covers: the answer is at least the box. A cached cover LARGER than the box is
/// accepted (the draw cover-crops it); one SMALLER in either axis is dropped and fetched again.
#[test]
fn a_cached_cover_larger_than_the_box_is_accepted_and_a_smaller_one_discarded() {
    for (w, h) in [(420, 236), (421, 236), (840, 472), (1920, 1080)] {
        let mut io = Fake::new();
        io.cached = Some(Cached { bytes: png(w, h), stale: false });
        let img = image(bake(&mut io, MIN));
        assert_eq!((img.w, img.h), (w, h));
        assert!(!io.discarded && io.untouched(), "{w}x{h}");
    }
    for (w, h) in [(419, 236), (420, 235), (300, 169), (840, 100)] {
        let mut io = Fake::answering(Ok(item(Some(URL))), Some(ArtFetch::Bytes(png(500, 300))));
        io.cached = Some(Cached { bytes: png(w, h), stale: false });
        let img = image(bake(&mut io, MIN));
        assert!(io.discarded, "{w}x{h} is below the box");
        assert_eq!(io.fetches, [URL], "and the cover is fetched again");
        assert_eq!((img.w, img.h), (500, 300));
    }
}

#[test]
fn an_undecodable_cached_cover_is_discarded_and_fetched_again() {
    let mut io = Fake::answering(Ok(item(Some(URL))), Some(ArtFetch::Bytes(png(420, 236))));
    io.cached = Some(Cached { bytes: b"not an image".to_vec(), stale: false });
    assert!(!is_final(&bake(&mut io, MIN)));
    assert!(io.discarded);
}

/// A fresh marker is a settled miss with no network at all; a stale one answers the same and is
/// flagged so the refresh lane re-reads Discover.
#[test]
fn a_marker_hit_is_final_with_no_read_and_no_fetch() {
    let mut io = Fake::new();
    io.tier.insert("marker", NO_COVER_MARKER.to_vec());
    let b = bake(&mut io, MIN);
    assert!(is_lasting_final(&b) && !b.stale, "the marker is a verdict, fresh or not");
    assert!(io.reads == 0 && io.fetches.is_empty() && io.persisted.is_empty());

    let mut io = Fake::new();
    io.tier.insert("marker", NO_COVER_MARKER.to_vec());
    io.marker_stale = true;
    let b = bake(&mut io, MIN);
    assert!(is_lasting_final(&b) && b.stale);
    assert_eq!(io.reads, 0);
}

/// The marker's body is what the tier will accept: it refuses an empty one, so a marker of nothing
/// would never have been stored and the boot would read Discover for the same title again.
#[test]
fn the_marker_round_trips_through_a_tier_that_refuses_an_empty_body() {
    assert!(!NO_COVER_MARKER.is_empty());
    let mut tier = Fake::new();
    tier.persist_marker(b"");
    assert_eq!(tier.markers(), 0, "the fake refuses an empty body exactly as imgcache does");

    // First boot: Discover says "no cover" and the marker is written.
    let mut first = Fake::answering(Ok(item(None)), None);
    assert!(is_final(&bake(&mut first, MIN)));
    assert_eq!((first.reads, first.markers()), (1, 1));
    // Next boot, same tier: a hit that reads nothing.
    let mut second = Fake::new();
    second.tier = first.tier;
    assert!(is_final(&bake(&mut second, MIN)));
    assert_eq!(second.reads, 0);
    assert!(second.fetches.is_empty());
}

// ── Discover, row by row (the plan's D7) ────────────────────────────────────────────────────────

#[test]
fn a_title_with_a_cover_is_fetched_persisted_once_and_delivered() {
    let body = png(640, 360);
    let mut io = Fake::answering(Ok(item(Some(URL))), Some(ArtFetch::Bytes(body.clone())));
    let img = image(bake(&mut io, MIN));
    assert_eq!((img.w, img.h), (640, 360));
    assert_eq!(io.reads, 1);
    assert_eq!(io.fetches, [URL], "the absolute URL goes to the item's server's transcoder");
    assert_eq!(io.persisted, [body], "the transcoder's bytes are filed as they are, once");
    assert_eq!(io.markers(), 0);
}

#[test]
fn every_discover_answer_maps_to_the_row_the_plan_names() {
    use WatchlistError as E;
    // (answer, writes the marker?) A marker is a verdict, so it is also the lasting miss; every
    // other answer is a read that did not answer, whose miss expires with the Discover memo.
    let rows: Vec<(&str, Result<CatalogItem, WatchlistError>, bool)> = vec![
        ("200 without coverArt", Ok(item(None)), true),
        ("NotFound", Err(E::NotFound), true),
        ("Refused (dev gate, nothing was sent)", Err(E::Refused), false),
        ("BadRequest", Err(E::BadRequest), false),
        ("Unauthorized", Err(E::Unauthorized), false),
        ("Transport", Err(E::Transport), false),
        ("Other(503)", Err(E::Other(503)), false),
        ("Other(500)", Err(E::Other(500)), false),
        ("Unreadable", Err(E::Unreadable), false),
        ("NotModified", Err(E::NotModified), false),
    ];
    for (what, answer, marker) in rows {
        let mut io = Fake::answering(answer, None);
        let b = bake(&mut io, MIN);
        assert!(is_final(&b), "{what}: a miss for this session");
        assert_eq!(b.lasting, marker, "{what}: only a definitive 'no cover' is a lasting miss");
        assert_eq!(io.reads, 1, "{what}");
        assert!(io.fetches.is_empty(), "{what}: no image is fetched without a cover URL");
        assert!(io.persisted.is_empty(), "{what}: no image bytes");
        assert_eq!(io.markers(), usize::from(marker), "{what}: marker only for a definitive 'no cover'");
    }
}

#[test]
fn every_transcoder_answer_maps_to_the_row_the_plan_names() {
    // (answer, writes the marker?, lasting miss?) A picture that cannot be used is the same picture
    // next time, so it is a verdict; a refused grant, a timeout, a 5xx and a silent line are not.
    let rows: Vec<(&str, ArtFetch, bool, bool)> = vec![
        ("404", ArtFetch::Status(404), true, true),
        ("400", ArtFetch::Status(400), true, true),
        ("410", ArtFetch::Status(410), true, true),
        ("401 (a refused grant)", ArtFetch::Status(401), false, false),
        ("403", ArtFetch::Status(403), false, false),
        ("408", ArtFetch::Status(408), false, false),
        ("429", ArtFetch::Status(429), false, false),
        ("500", ArtFetch::Status(500), false, false),
        ("502", ArtFetch::Status(502), false, false),
        ("no response", ArtFetch::NoResponse, false, false),
        ("an empty body", ArtFetch::Bytes(Vec::new()), false, false),
        ("bytes that do not decode", ArtFetch::Bytes(b"<html>".to_vec()), false, true),
        ("a picture smaller than the box", ArtFetch::Bytes(png(419, 236)), false, true),
    ];
    for (what, answer, marker, lasting) in rows {
        let mut io = Fake::answering(Ok(item(Some(URL))), Some(answer));
        let b = bake(&mut io, MIN);
        assert!(is_final(&b), "{what}: a miss for this session");
        assert_eq!(b.lasting, lasting, "{what}: lasting only when the answer is a verdict");
        assert!(io.persisted.is_empty(), "{what}: no image bytes");
        assert_eq!(io.markers(), usize::from(marker), "{what}");
    }
}

#[test]
fn verdict_and_the_status_rule_are_pure() {
    assert_eq!(verdict(Ok(item(Some(URL)))), Verdict::Fetch(URL.to_owned()));
    assert_eq!(verdict(Ok(item(None))), Verdict::NoCover);
    assert_eq!(verdict(Err(WatchlistError::NotFound)), Verdict::NoCover);
    assert_eq!(verdict(Err(WatchlistError::Transport)), Verdict::Skip);
    assert!(marks_no_cover(404) && !marks_no_cover(200) && !marks_no_cover(503) && !marks_no_cover(429));
}

// ── before dialling ─────────────────────────────────────────────────────────────────────────────

/// No client identifier is no identity to read as: nothing is sent and nothing is written.
#[test]
fn without_an_identity_nothing_is_read_or_written() {
    let mut io = Fake::new();
    io.identified = false;
    let b = bake(&mut io, MIN);
    assert!(is_final(&b) && !b.lasting, "nothing was asked, so nothing is known about the title");
    assert!(io.untouched());
}

/// Discover failed to answer moments ago: the cover settles WITHOUT dialling, and writes nothing
/// (a marker would claim the title has no cover, which is not what happened).
#[test]
fn a_recently_unreachable_discover_settles_a_cover_without_a_dial() {
    let mut io = Fake::new();
    io.unreachable = true;
    let b = bake(&mut io, MIN);
    assert!(is_final(&b) && !b.lasting, "an unreachable Discover says nothing about the title: the miss expires with the memo");
    assert!(io.untouched());
}

/// The memo does not hide what is already on disk: a cached cover still serves with Discover down.
#[test]
fn a_cached_cover_serves_while_discover_is_unreachable() {
    let mut io = Fake::new();
    io.unreachable = true;
    io.cached = Some(Cached { bytes: png(420, 236), stale: true });
    assert!(!is_final(&bake(&mut io, MIN)));
}

// ── the refresh lane's re-bake ──────────────────────────────────────────────────────────────────

#[test]
fn a_rebake_rereads_discover_whatever_is_on_disk_and_writes_what_came_back() {
    let body = png(800, 450);
    let mut io = Fake::answering(Ok(item(Some(URL))), Some(ArtFetch::Bytes(body.clone())));
    io.cached = Some(Cached { bytes: png(420, 236), stale: true });
    io.tier.insert("marker", NO_COVER_MARKER.to_vec());
    rebake(&mut io, MIN);
    assert_eq!((io.reads, io.fetches.len()), (1, 1), "a cached cover or marker does not stop it");
    assert_eq!(io.persisted, [body]);
}

#[test]
fn a_rebake_that_cannot_reach_discover_writes_nothing() {
    for answer in [Err(WatchlistError::Transport), Err(WatchlistError::Unauthorized), Err(WatchlistError::Other(502))] {
        let mut io = Fake::answering(answer, None);
        rebake(&mut io, MIN);
        assert!(io.persisted.is_empty() && io.markers() == 0 && !io.discarded);
    }
    let mut io = Fake::new();
    io.unreachable = true;
    rebake(&mut io, MIN);
    assert!(io.untouched() && !io.discarded, "no dial, and the cached cover stands");
}

/// A title that lost its cover: the cached one goes and the marker is written, so the next boot
/// does not serve a cover Plex no longer has.
#[test]
fn a_rebake_that_finds_no_cover_replaces_the_cover_with_the_marker() {
    let mut io = Fake::answering(Ok(item(None)), None);
    rebake(&mut io, MIN);
    assert!(io.discarded);
    assert_eq!(io.markers(), 1);
}

// ── identity ────────────────────────────────────────────────────────────────────────────────────

/// A localized title has a different `coverArt`, so the language is part of the baked identity (and
/// the marker's): no server, no profile.
#[test]
fn the_cover_identity_carries_the_language_and_neither_a_server_nor_a_profile() {
    let key = |lang: &str, kind: &str| {
        plx_platform::imgcache::classify_baked(&namespace(lang), kind, "5d776824999c4a001f9a1b2c", COVER_STAMP, 420, 236)
    };
    assert!(key("en", COVER_KIND).is_some());
    assert_eq!(key("en", COVER_KIND), key("en", COVER_KIND));
    assert_ne!(key("en", COVER_KIND), key("fr", COVER_KIND), "a language change is a new key");
    assert_ne!(key("en", COVER_KIND), key("en", COVER_NONE_KIND), "the marker is a sibling identity");
    assert_eq!(namespace("fr"), "plextv|lang:fr");
    assert!(!namespace("fr").contains("machine:") && !namespace("fr").contains("profile"));
}

/// The kinds are versioned strings, pinned: a change in what is stored under them must bump them.
#[test]
fn the_kinds_and_the_marker_are_pinned() {
    assert_eq!((COVER_KIND, COVER_NONE_KIND), ("cover.1", "cover-none.1"));
    assert_eq!(NO_COVER_MARKER, b"plx-no-cover-1");
}
