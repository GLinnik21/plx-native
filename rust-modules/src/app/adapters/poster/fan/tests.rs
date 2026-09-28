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
    let centre = at(&out, FAN_W / 2, (0.47 * FAN_H as f32) as u32);
    assert!(
        close(centre, [200, 200, 200], 2),
        "front poster on top: {centre:?}"
    );
    // The side posters show beside the front one.
    assert!(
        close(at(&out, 40, 190), [8, 164, 8], 4),
        "left poster behind: {:?}",
        at(&out, 40, 190)
    );
    assert!(
        close(at(&out, 260, 190), [8, 8, 164], 4),
        "right poster behind: {:?}",
        at(&out, 260, 190)
    );
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
    assert!(
        bottom < 50,
        "the bottom edge is darkened for the live title: {bottom}"
    );
}

#[derive(Default)]
struct Mock {
    cached: Option<Vec<u8>>,
    members: Option<Members>,
    posters: Vec<Art>,
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
    fn members(&mut self) -> Members {
        self.member_calls += 1;
        self.members.take().unwrap_or(Members::Transient)
    }
    fn poster(&mut self, _: &str) -> Art {
        self.poster_calls += 1;
        if self.posters.is_empty() {
            Art::Final
        } else {
            self.posters.remove(0)
        }
    }
    fn persist(&mut self, png: &[u8]) {
        self.persisted = Some(png.to_vec());
    }
}

fn listed(n: usize) -> Members {
    Members::Listed(
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
            Art::Decoded(solid(20, 30, [200, 0, 0])),
            Art::Decoded(solid(20, 30, [0, 200, 0])),
            Art::Decoded(solid(20, 30, [0, 0, 200])),
        ],
        ..Mock::default()
    };
    let FanOutcome::Baked(first) = bake(&mut cold) else {
        panic!("cold bake must produce art")
    };
    assert_eq!((cold.member_calls, cold.poster_calls), (1, 3));
    let png = cold.persisted.expect("a complete bake is persisted");

    let mut warm = Mock {
        cached: Some(png),
        ..Mock::default()
    };
    let FanOutcome::Baked(again) = bake(&mut warm) else {
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
        posters: vec![Art::Decoded(solid(20, 30, [9, 9, 9]))],
        ..Mock::default()
    };
    assert!(matches!(bake(&mut io), FanOutcome::Baked(_)));
    assert!(io.discarded && io.persisted.is_some());
}

#[test]
fn empty_denied_or_artless_collections_have_no_art_and_never_persist() {
    for (members, posters) in [
        (Members::Listed(Vec::new()), vec![]),
        (Members::Final, vec![]),
        (Members::Listed(vec![(String::new(), None)]), vec![]),
        (listed(2), vec![Art::Final, Art::Final]),
    ] {
        let mut io = Mock {
            members: Some(members),
            posters,
            ..Mock::default()
        };
        assert!(matches!(bake(&mut io), FanOutcome::NoArt));
        assert!(io.persisted.is_none());
    }
}

#[test]
fn transient_failures_retry_and_a_degraded_bake_is_not_persisted() {
    let mut listing = Mock {
        members: Some(Members::Transient),
        ..Mock::default()
    };
    assert!(matches!(bake(&mut listing), FanOutcome::Transient));

    let mut all_down = Mock {
        members: Some(listed(2)),
        posters: vec![Art::Transient, Art::Transient],
        ..Mock::default()
    };
    assert!(matches!(bake(&mut all_down), FanOutcome::Transient));

    let mut partial = Mock {
        members: Some(listed(2)),
        posters: vec![Art::Decoded(solid(20, 30, [1, 2, 3])), Art::Transient],
        ..Mock::default()
    };
    assert!(
        matches!(bake(&mut partial), FanOutcome::Baked(_)),
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
