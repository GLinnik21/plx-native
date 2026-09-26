use super::*;

fn event(id: u32, start_ms: i64, duration_ms: i64) -> Event {
    Event {
        start_ms,
        duration_ms,
        payload: format!("{id},0,Default,,0,0,0,,hello").into_bytes().into(),
    }
}

fn source(events: Vec<Event>) -> Source {
    Source {
        id: 1,
        revision: 1,
        content: Content::Embedded {
            header: Arc::from(&b"[Script Info]\n"[..]),
            events: events.into(),
            fonts: Arc::from([]),
        },
    }
}

#[test]
fn window_retirement_keeps_native_fonts_and_retained_overlapping_events() {
    let long = event(0, 0, 20_000);
    let ended = event(1, 0, 1000);
    let old = source(vec![long.clone(), ended, event(2, 2000, 3000)]);
    let new = source(vec![long, event(2, 2000, 3000), event(3, 6000, 2000)]);
    assert_eq!(append_from(&old, &new), Some(2));
}

#[test]
fn append_preserves_readorder_but_replacement_cannot_append() {
    let old = source(vec![event(0, 0, 1000)]);
    let new = source(vec![event(0, 0, 1000), event(1, 0, 1000)]);
    assert_eq!(append_from(&old, &new), Some(1));
    let replacement = source(vec![event(2, 0, 1000)]);
    assert_eq!(append_from(&old, &replacement), None);
    let mut seek = new;
    seek.id = 2;
    assert_eq!(append_from(&old, &seek), None);
}

#[test]
fn rejects_unbounded_and_invalid_events_before_native_parsing() {
    let mut invalid = event(0, i64::MAX, 1);
    assert!(validate(&source(vec![invalid.clone()])).is_err());
    invalid.start_ms = 0;
    invalid.duration_ms = 0;
    assert!(validate(&source(vec![invalid.clone()])).is_err());
    invalid.duration_ms = 1000;
    invalid.payload = vec![b'a'; MAX_EVENT_BYTES + 1].into();
    assert!(validate(&source(vec![invalid])).is_err());
    assert!(validate(&source(vec![event(0, 0, 1000)])).is_ok());
}

#[test]
fn ids_do_not_alias_cancellation_or_each_other() {
    let a = next_source_id();
    let b = next_source_id();
    assert_ne!(a, 0);
    assert_ne!(a, b);
}

const HEADER: &str = "[Script Info]\nScriptType: v4.00+\nPlayResX: 320\nPlayResY: 180\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Inter,24,&H0000FF00,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,0,0,7,0,0,0,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";

fn script(lines: &str) -> Arc<Source> {
    Arc::new(Source {
        id: next_source_id(),
        revision: 1,
        content: Content::Script {
            bytes: format!("{HEADER}{lines}").into_bytes().into(),
            fonts: Arc::from([]),
        },
    })
}

fn render(engine: &mut Engine, source: &Arc<Source>, now_ms: i64) -> Arc<Frame> {
    let frame = engine.render(&Request {
        source: source.clone(),
        key: Key {
            epoch: 1,
            source_id: source.id,
            revision: source.revision,
            now_ms,
            width: 320,
            height: 180,
            storage_width: 320,
            storage_height: 180,
        },
    });
    assert_eq!(
        frame.error, None,
        "native fixture requires the built host library in pkg"
    );
    frame
}

fn pixel(frame: &Frame, x: i32, y: i32) -> [u8; 4] {
    let r = frame.rect.as_ref().expect("visible pixels");
    assert!(
        x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height,
        "({x},{y}) outside {:?}",
        (r.x, r.y, r.width, r.height)
    );
    let start = ((y - r.y) * r.width + x - r.x) as usize * 4;
    r.rgba[start..start + 4].try_into().unwrap()
}

/// An actual pixel contract against the same pinned library shipped to the TV.
/// Run after `make libass-host` with `--include-ignored`.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_pixels_preserve_position_layers_alpha_motion_karaoke_and_readorder() {
    let red = r"{\an7\pos(20,20)\c&H0000FF&\p1}m 0 0 l 40 0 40 40 0 40";
    let blue = r"{\an7\pos(40,30)\c&HFF0000&\alpha&H80&\p1}m 0 0 l 40 0 40 40 0 40";
    let source = script(&format!(
        "Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{red}\nDialogue: 1,0:00:00.00,0:00:02.00,Default,,0,0,0,,{blue}\n"));
    let mut engine = Engine::default();
    let first = render(&mut engine, &source, 100);
    assert_eq!(pixel(&first, 25, 25), [255, 0, 0, 255]);
    assert_eq!(pixel(&first, 45, 35), [128, 0, 127, 255]);
    assert_eq!(pixel(&first, 75, 35), [0, 0, 255, 127]);
    let coded = engine.render(&Request {
        source: source.clone(),
        key: Key {
            epoch: 1,
            source_id: source.id,
            revision: source.revision,
            now_ms: 100,
            width: 320,
            height: 180,
            storage_width: 720,
            storage_height: 480,
        },
    });
    assert_eq!(coded.error, None);
    assert_eq!(pixel(&coded, 25, 25), [255, 0, 0, 255]);
    assert_eq!(
        pixel(&coded, 45, 35),
        [128, 0, 127, 255],
        "authored positions remain in the output canvas when coded video differs"
    );
    assert_eq!(
        render(&mut engine, &source, 100).serial,
        first.serial,
        "paused clock must reuse pixels"
    );
    assert_eq!(
        render(&mut engine, &source, 200).serial,
        first.serial,
        "static dialogue must reuse pixels while playing"
    );
    assert!(
        render(&mut engine, &source, 2100).rect.is_none(),
        "expired output clears"
    );

    let embedded = Arc::new(Source {
        id: next_source_id(),
        revision: 1,
        content: Content::Embedded {
            header: HEADER.as_bytes().into(),
            fonts: Arc::from([]),
            events: vec![
                Event {
                    start_ms: 0,
                    duration_ms: 2000,
                    payload: format!("0,0,Default,,0,0,0,,{red}").into_bytes().into(),
                },
                Event {
                    start_ms: 0,
                    duration_ms: 2000,
                    payload: format!("1,1,Default,,0,0,0,,{blue}").into_bytes().into(),
                },
                // Duplicate ReadOrder must NOT blend a second translucent blue layer.
                Event {
                    start_ms: 0,
                    duration_ms: 2000,
                    payload: format!("1,1,Default,,0,0,0,,{blue}").into_bytes().into(),
                },
            ]
            .into(),
        },
    });
    let packet_frame = render(&mut engine, &embedded, 100);
    assert_eq!(
        packet_frame.rect.as_ref().unwrap().rgba,
        first.rect.as_ref().unwrap().rgba,
        "Matroska chunks and a standalone script must render identical composed pixels"
    );

    let moving = script("Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\an7\\move(20,80,160,80,0,1000)\\p1}m 0 0 l 20 0 20 20 0 20\n");
    let start = render(&mut engine, &moving, 0);
    let halfway = render(&mut engine, &moving, 500);
    assert_eq!(
        halfway.rect.as_ref().unwrap().x - start.rect.as_ref().unwrap().x,
        70,
        "motion uses subtitle time, including between native position callbacks"
    );
    assert_ne!(start.serial, halfway.serial);
    assert_eq!(render(&mut engine, &moving, 500).serial, halfway.serial);

    let karaoke = script(
        "Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\an7\\pos(20,100)}{\\kf100}AAAA\n",
    );
    let early = render(&mut engine, &karaoke, 100);
    let late = render(&mut engine, &karaoke, 900);
    let green = |f: &Frame| {
        f.rect
            .as_ref()
            .unwrap()
            .rgba
            .chunks_exact(4)
            .filter(|p| p[1] > 200 && p[0] < 20 && p[3] > 100)
            .count()
    };
    assert!(
        green(&late) > green(&early),
        "karaoke primary color sweeps over authored glyphs"
    );
    assert_ne!(early.serial, late.serial);
}

/// Give the shipped bold face a unique family in memory, so the test can prove
/// an attachment was selected instead of accidentally exercising fallback. The
/// SFNT name table size stays unchanged; repair its and the file's checksums.
fn fixture_font() -> Arc<[u8]> {
    let mut font = std::fs::read(asset_dir().join("appfont-bold.ttf")).unwrap();
    let u32_at = |bytes: &[u8], at: usize| {
        u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
    };
    let tables = u16::from_be_bytes(font[4..6].try_into().unwrap()) as usize;
    let record = |tag: &[u8]| {
        (0..tables)
            .map(|i| 12 + i * 16)
            .find(|&i| &font[i..i + 4] == tag)
            .unwrap()
    };
    let name_record = record(b"name");
    let head_record = record(b"head");
    let name_start = u32_at(&font, name_record + 8);
    let name_len = u32_at(&font, name_record + 12);
    let head_start = u32_at(&font, head_record + 8);
    let head_len = u32_at(&font, head_record + 12);
    let names = &mut font[name_start..name_start + name_len];
    for (from, to) in [
        (b"Inter".as_slice(), b"PLX73".as_slice()),
        (b"\0I\0n\0t\0e\0r".as_slice(), b"\0P\0L\0X\07\03".as_slice()),
    ] {
        for i in 0..=names.len() - from.len() {
            if &names[i..i + from.len()] == from {
                names[i..i + from.len()].copy_from_slice(to);
            }
        }
    }
    let checksum = |bytes: &[u8]| {
        bytes.chunks(4).fold(0u32, |sum, chunk| {
            let mut word = [0; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            sum.wrapping_add(u32::from_be_bytes(word))
        })
    };
    font[head_start + 8..head_start + 12].fill(0);
    for (record, start, len) in [
        (name_record, name_start, name_len),
        (head_record, head_start, head_len),
    ] {
        let sum = checksum(&font[start..start + len]);
        font[record + 4..record + 8].copy_from_slice(&sum.to_be_bytes());
    }
    let adjustment = 0xb1b0_afbau32.wrapping_sub(checksum(&font));
    font[head_start + 8..head_start + 12].copy_from_slice(&adjustment.to_be_bytes());
    font.into()
}

#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_script_font_revision_and_cjk_fallback_use_real_glyphs() {
    let plain = script(
        "Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\fnPLX73\\pos(20,40)}Attachment\n",
    );
    let mut engine = Engine::default();
    let fallback = render(&mut engine, &plain, 100);
    let Content::Script { bytes, .. } = &plain.content else {
        unreachable!()
    };
    let attached = Arc::new(Source {
        id: plain.id,
        revision: 2,
        content: Content::Script {
            bytes: bytes.clone(),
            fonts: vec![Font {
                name: "fixture.ttf".into(),
                data: fixture_font(),
            }]
            .into(),
        },
    });
    let rendered = render(&mut engine, &attached, 100);
    assert_ne!(
        rendered.rect.as_ref().unwrap().rgba,
        fallback.rect.as_ref().unwrap().rgba,
        "a late attached font replaces fallback even when the source identity is unchanged"
    );
    let korean = script("Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\pos(20,40)}한\n");
    let chinese = script("Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\pos(20,40)}中\n");
    let a = render(&mut engine, &korean, 100);
    let b = render(&mut engine, &chinese, 100);
    assert_ne!(
        a.rect.as_ref().unwrap().rgba,
        b.rect.as_ref().unwrap().rgba,
        "the bundled CJK fallback yields distinct glyphs, not identical missing-glyph boxes"
    );
}
