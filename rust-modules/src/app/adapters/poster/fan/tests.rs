use super::*;

fn solid(w: u32, h: u32, c: [u8; 3]) -> Rgba {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        px.extend_from_slice(&[c[0], c[1], c[2], 255]);
    }
    Rgba { w, h, px }
}

fn at(img: &Rgba, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * img.w + x) * 4) as usize;
    [img.px[i], img.px[i + 1], img.px[i + 2]]
}

fn member(c: [u8; 3], blur: Option<[[f32; 3]; 4]>) -> Member {
    Member {
        poster: solid(20, 30, c),
        blur,
    }
}

fn ring(c: [f32; 3]) -> Option<[[f32; 3]; 4]> {
    Some([c; 4])
}

fn close(a: [u8; 3], b: [u8; 3], tol: i32) -> bool {
    (0..3).all(|k| (a[k] as i32 - b[k] as i32).abs() <= tol)
}

/// The owner's layout: top-left from the LEFT poster's colours, top-right from the RIGHT one's,
/// both bottom corners from the FRONT one's; the front poster covers the centre on top of the
/// others; the output is exactly the bake size and opaque.
#[test]
fn corners_come_from_their_poster_and_the_front_poster_is_on_top() {
    let front = member([200, 200, 200], ring([0.0, 0.0, 1.0]));
    let left = member([10, 200, 10], ring([1.0, 0.0, 0.0]));
    let right = member([10, 10, 200], ring([0.0, 1.0, 0.0]));
    let out = compose(&front, Some(&left), Some(&right));
    assert_eq!((out.w, out.h), (FAN_W, FAN_H));
    assert_eq!(out.px.len(), (FAN_W * FAN_H * 4) as usize);
    assert!(out.px.chunks(4).all(|p| p[3] == 255), "the bake is opaque");

    assert!(
        close(at(&out, 0, 0), [255, 0, 0], 2),
        "top-left = left: {:?}",
        at(&out, 0, 0)
    );
    assert!(
        close(at(&out, FAN_W - 1, 0), [0, 255, 0], 2),
        "top-right = right"
    );
    // Bottom corners are the front's colour under the scrim: blue, darkened, never red/green.
    for x in [0, FAN_W - 1] {
        let c = at(&out, x, FAN_H - 1);
        assert!(
            c[2] > 40 && c[0] < 5 && c[1] < 5,
            "bottom corner = front, darkened: {c:?}"
        );
    }
    let centre = at(&out, FAN_W / 2, (0.32 * FAN_H as f32) as u32);
    assert!(
        close(centre, [200, 200, 200], 2),
        "front poster on top: {centre:?}"
    );
    // The side posters show beside the front one, unshaded (the mock dims neither).
    let (hw, hh) = member_half();
    let (lx, ly) = pixel_of(&BACK_LEFT, -0.88 * hw, -0.3 * hh);
    assert!(close(at(&out, lx, ly), [10, 200, 10], 4), "left poster behind: {:?}", at(&out, lx, ly));
    let (rx, ry) = pixel_of(&BACK_RIGHT, 0.88 * hw, -0.3 * hh);
    assert!(close(at(&out, rx, ry), [10, 10, 200], 4), "right poster behind: {:?}", at(&out, rx, ry));
}

/// The output pixel containing the point `(lx, ly)` of a member's own frame, for a member placed
/// by `p` (the same rotation [`draw`] inverts), and that member's half-extents.
fn pixel_of(p: &FanMember, lx: f32, ly: f32) -> (u32, u32) {
    let m = crate::ui::collection_tile::fan_member(p, FAN_W as f32, FAN_H as f32);
    let (dx, dy) = (lx * m.cosine() - ly * m.sin, lx * m.sin + ly * m.cosine());
    ((m.cx + dx).floor() as u32, (m.cy + dy).floor() as u32)
}

fn member_half() -> (f32, f32) {
    let m = crate::ui::collection_tile::fan_member(&FRONT, FAN_W as f32, FAN_H as f32);
    (m.hw, m.hh)
}

/// Every poster in the app has rounded corners, and so do the members baked into a fan: a pixel
/// just inside a member's corner square but outside its arc is the ground (or a shadow over it),
/// never the poster, while the member's centre and edge midpoints are the poster. Checked on the
/// front member and on a TILTED back member, whose corner is only round if the mask rotates with
/// it.
#[test]
fn baked_members_have_rounded_corners() {
    let ground = ring([1.0, 0.0, 0.0]);
    let front = member([200, 200, 200], ground);
    let left = member([10, 200, 10], ground);
    let right = member([10, 10, 200], ground);
    let out = compose(&front, Some(&left), Some(&right));
    let (hw, hh) = member_half();
    let not_poster = |p: (u32, u32), poster: [u8; 3], what: &str| {
        let c = at(&out, p.0, p.1);
        assert!(!close(c, poster, 40), "{what} at {p:?} is poster colour {c:?}");
        assert!(c[0] > 60 && c[1] < 40 && c[2] < 40, "{what} at {p:?} is not ground/shadow: {c:?}");
    };
    let is_poster = |p: (u32, u32), poster: [u8; 3], what: &str| {
        let c = at(&out, p.0, p.1);
        assert!(close(c, poster, 3), "{what} at {p:?} is {c:?}, not poster {poster:?}");
    };

    // Probes sit 3 px inside an edge (clear of the 1-px anti-aliasing and the rim), and 1 px
    // inside BOTH edges at a corner — inside the corner square, outside a 7.2-px arc.
    let grey = [200, 200, 200];
    is_poster(pixel_of(&FRONT, 0.0, 0.0), grey, "front centre");
    is_poster(pixel_of(&FRONT, 0.0, -(hh - 3.0)), grey, "front top edge");
    is_poster(pixel_of(&FRONT, -(hw - 3.0), 0.0), grey, "front left edge");
    is_poster(pixel_of(&FRONT, hw - 3.0, 0.0), grey, "front right edge");
    not_poster(pixel_of(&FRONT, hw - 1.0, -(hh - 1.0)), grey, "front top-right corner");
    not_poster(pixel_of(&FRONT, -(hw - 1.0), -(hh - 1.0)), grey, "front top-left corner");

    let blue = [10, 10, 200];
    is_poster(pixel_of(&BACK_RIGHT, hw - 3.0, 0.0), blue, "right outer edge");
    is_poster(pixel_of(&BACK_RIGHT, hw - 3.0, hh * 0.5), blue, "right outer edge, low");
    not_poster(pixel_of(&BACK_RIGHT, hw - 1.0, -(hh - 1.0)), blue, "right top-right corner");
}

/// Without UltraBlurColors a corner is the averaged pixels of that corner of the poster.
#[test]
fn missing_ultrablur_falls_back_to_the_posters_own_corner_pixels() {
    let mut poster = solid(20, 30, [0, 0, 0]);
    for y in 0..7 {
        for x in 0..5 {
            let i = ((y * 20 + x) * 4) as usize;
            poster.px[i..i + 3].copy_from_slice(&[240, 120, 0]);
        }
    }
    let left = Member { poster, blur: None };
    let front = member([50, 50, 50], ring([0.0, 0.0, 0.5]));
    let out = compose(&front, Some(&left), None);
    assert!(
        close(at(&out, 0, 0), [240, 120, 0], 2),
        "{:?}",
        at(&out, 0, 0)
    );
    // No right member: the front poster supplies top-right too.
    assert!(
        close(at(&out, FAN_W - 1, 0), [0, 0, 128], 2),
        "{:?}",
        at(&out, FAN_W - 1, 0)
    );
}

/// The scrim leaves the top alone and darkens toward the bottom.
#[test]
fn the_scrim_darkens_only_the_bottom() {
    let grey = ring([0.5, 0.5, 0.5]);
    let front = Member {
        poster: solid(20, 30, [128, 128, 128]),
        blur: grey,
    };
    let out = compose(&front, None, None);
    let top = at(&out, 2, 2)[0];
    let mid = at(&out, 2, FAN_H / 2)[0];
    let bottom = at(&out, 2, FAN_H - 1)[0];
    assert!(close(at(&out, 2, 2), [128, 128, 128], 1));
    assert_eq!(top, mid, "above the scrim nothing changes");
    // `.scr`: linear to rgba(0,0,0,.55) at the bottom edge, so 128 keeps 45%.
    assert!(
        (bottom as i32 - 58).abs() <= 2,
        "the bottom edge is darkened for the live title: {bottom}"
    );
}

#[derive(Default)]
struct Mock {
    cached: Option<Vec<u8>>,
    members: Option<Got<Members>>,
    posters: Vec<Got<Rgba>>,
    member_calls: usize,
    poster_calls: usize,
    discarded: bool,
    persisted: Option<Vec<u8>>,
}

impl FanIo for Mock {
    fn cached(&mut self) -> Option<Vec<u8>> {
        self.cached.take()
    }
    fn discard(&mut self) {
        self.discarded = true;
    }
    fn members(&mut self) -> Got<Members> {
        self.member_calls += 1;
        self.members.take().unwrap_or(Got::Transient)
    }
    fn poster(&mut self, _: &str) -> Got<Rgba> {
        self.poster_calls += 1;
        if self.posters.is_empty() {
            Got::Final
        } else {
            self.posters.remove(0)
        }
    }
    fn persist(&mut self, png: &[u8]) {
        self.persisted = Some(png.to_vec());
    }
}

fn listed(n: usize) -> Got<Members> {
    Got::Ok(
        (0..n)
            .map(|i| (format!("/library/metadata/{i}/thumb/1"), None))
            .collect(),
    )
}

#[test]
fn a_cold_bake_fetches_three_members_persists_and_a_warm_hit_skips_them() {
    let mut cold = Mock {
        members: Some(listed(3)),
        posters: vec![
            Got::Ok(solid(20, 30, [200, 0, 0])),
            Got::Ok(solid(20, 30, [0, 200, 0])),
            Got::Ok(solid(20, 30, [0, 0, 200])),
        ],
        ..Mock::default()
    };
    let Got::Ok(first) = bake(&mut cold) else {
        panic!("cold bake must produce art")
    };
    assert_eq!((cold.member_calls, cold.poster_calls), (1, 3));
    let png = cold.persisted.expect("a complete bake is persisted");
    // IHDR colour type (byte 25): 2 = truecolour RGB. The bake is opaque, so no alpha is stored.
    assert_eq!(png[25], 2, "the baked fan is persisted as RGB, not RGBA");

    let mut warm = Mock {
        cached: Some(png),
        ..Mock::default()
    };
    let Got::Ok(again) = bake(&mut warm) else {
        panic!("warm hit must produce art")
    };
    assert_eq!(
        (warm.member_calls, warm.poster_calls),
        (0, 0),
        "a disk hit fetches no member"
    );
    assert!(warm.persisted.is_none());
    assert_eq!((again.w, again.h), (first.w, first.h));
    assert_eq!(again.px, first.px, "PNG round-trips the bake losslessly");
}

#[test]
fn an_undecodable_disk_entry_is_discarded_and_rebaked() {
    let mut io = Mock {
        cached: Some(b"not a png".to_vec()),
        members: Some(listed(1)),
        posters: vec![Got::Ok(solid(20, 30, [9, 9, 9]))],
        ..Mock::default()
    };
    assert!(matches!(bake(&mut io), Got::Ok(_)));
    assert!(io.discarded && io.persisted.is_some());
}

#[test]
fn empty_denied_or_artless_collections_have_no_art_and_never_persist() {
    for (members, posters) in [
        (Got::Ok(Vec::new()), vec![]),
        (Got::Final, vec![]),
        (Got::Ok(vec![(String::new(), None)]), vec![]),
        (listed(2), vec![Got::Final, Got::Final]),
    ] {
        let mut io = Mock {
            members: Some(members),
            posters,
            ..Mock::default()
        };
        assert!(matches!(bake(&mut io), Got::Final));
        assert!(io.persisted.is_none());
    }
}

#[test]
fn transient_failures_retry_and_a_degraded_bake_is_not_persisted() {
    let mut listing = Mock {
        members: Some(Got::Transient),
        ..Mock::default()
    };
    assert!(matches!(bake(&mut listing), Got::Transient));

    let mut all_down = Mock {
        members: Some(listed(2)),
        posters: vec![Got::Transient, Got::Transient],
        ..Mock::default()
    };
    assert!(matches!(bake(&mut all_down), Got::Transient));

    let mut partial = Mock {
        members: Some(listed(2)),
        posters: vec![Got::Ok(solid(20, 30, [1, 2, 3])), Got::Transient],
        ..Mock::default()
    };
    assert!(
        matches!(bake(&mut partial), Got::Ok(_)),
        "show what arrived"
    );
    assert!(
        partial.persisted.is_none(),
        "but do not freeze a degraded fan on disk"
    );
}

#[test]
fn only_a_composite_becomes_a_fan_key() {
    assert_eq!(
        fan_key("/library/collections/7/composite/123?width=1").as_deref(),
        Some("/plx/fan/7/123")
    );
    assert_eq!(fan_key("/library/metadata/7/thumb/123"), None);
    assert_eq!(fan_key(""), None);
    assert_eq!(parse_fan_key("/plx/fan/7/123"), Some(("7", "123")));
    for bad in [
        "/plx/fan/7",
        "/plx/fan//1",
        "/plx/fan/7/",
        "/plx/fan/7/1/2",
        "/photo/:/transcode?x",
    ] {
        assert_eq!(parse_fan_key(bad), None, "{bad}");
    }
}
