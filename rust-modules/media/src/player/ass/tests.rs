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
        origin: Origin::Authored,
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

fn request_key(source: &Source) -> Key {
    Key {
        epoch: 1,
        source_id: source.id,
        revision: source.revision,
        width: 320,
        height: 180,
        storage_width: 320,
        storage_height: 180,
    }
}

#[test]
fn off_retires_a_racing_publication_and_pending_source_outside_the_mailbox_lock() {
    let runtime = Runtime::new();
    let source = Arc::new(source(vec![event(0, 0, 2000)]));
    let key = request_key(&source);
    let frame = error_frame(key, Fault::RenderFailed);
    let source_witness = Arc::downgrade(&source);
    let frame_witness = Arc::downgrade(&frame);
    let mut engine = Engine {
        source: Some(source.clone()),
        frame: Some(frame.clone()),
        ..Engine::default()
    };
    runtime.source_id.store(source.id, Ordering::Release);
    {
        let mut mailbox = runtime.mailbox.lock().unwrap();
        mailbox.request(key, 100, &source);
        // The worker owns the lock and has already accepted this epoch. Cancel
        // must return while the lock is held; the old publication then races in.
        clear_runtime(&runtime);
        mailbox.publish_error(key, frame.clone());
    }
    drop(source);
    drop(frame);
    let retired = {
        let mut mailbox = runtime.mailbox.lock().unwrap();
        retire_off(&runtime, &mut mailbox, &mut engine).expect("Off has resources to retire")
    };
    assert!(
        runtime.mailbox.try_lock().is_ok(),
        "destruction must happen after unlocking"
    );
    assert!(engine.is_empty());
    {
        let mailbox = runtime.mailbox.lock().unwrap();
        assert!(
            mailbox.is_empty(),
            "retire stale work, requests and publications before considering them for rendering"
        );
    }
    assert!(source_witness.upgrade().is_some());
    assert!(frame_witness.upgrade().is_some());
    drop(retired);
    assert!(source_witness.upgrade().is_none());
    assert!(frame_witness.upgrade().is_none());
    assert!(
        retire_off(&runtime, &mut runtime.mailbox.lock().unwrap(), &mut engine).is_none(),
        "an empty Off worker can stay parked without periodic cleanup"
    );
}

#[test]
fn cancellation_before_worker_parks_is_remembered_and_releases_resources() {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let runtime = Arc::new(Runtime::new());
    let source = Arc::new(source(vec![event(0, 0, 2000)]));
    let key = request_key(&source);
    let frame = error_frame(key, Fault::RenderFailed);
    let witness = Arc::downgrade(&frame);
    runtime.source_id.store(source.id, Ordering::Release);
    runtime.mailbox.lock().unwrap().publish_error(key, frame.clone());
    let observed_idle = Arc::new(AtomicBool::new(false));
    let may_park = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = mpsc::channel();
    let worker = {
        let runtime = runtime.clone();
        let observed_idle = observed_idle.clone();
        let may_park = may_park.clone();
        plx_base::task::spawn("ASS cancellation test", move || {
            runtime.worker.set(std::thread::current()).unwrap();
            let mut engine = Engine {
                source: Some(source),
                frame: Some(frame),
                ..Engine::default()
            };
            {
                let mailbox = runtime.mailbox.lock().unwrap();
                assert!(mailbox.is_empty() || mailbox.published.is_some());
                assert_ne!(runtime.source_id.load(Ordering::Acquire), 0);
            }
            // Main will cancel only AFTER the idle observation and BEFORE park.
            // Spin on an atomic gate so no other blocking primitive can consume
            // this thread's park permit during the forced interleaving.
            observed_idle.store(true, Ordering::Release);
            while !may_park.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            std::thread::park();
            let retired = {
                let mut mailbox = runtime.mailbox.lock().unwrap();
                retire_off(&runtime, &mut mailbox, &mut engine)
                    .expect("remembered Off wakes cleanup")
            };
            drop(retired);
            done_tx.send(()).unwrap();
        })
        .expect("test worker")
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while !observed_idle.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "worker must reach the controlled idle boundary"
        );
        std::thread::yield_now();
    }
    clear_runtime(&runtime);
    may_park.store(true, Ordering::Release);
    let completed = done_rx.recv_timeout(Duration::from_secs(2));
    // If the wake was lost, release the test thread before reporting failure.
    // The timeout observes completion; it never chooses the race interleaving.
    if completed.is_err() {
        worker.thread().unpark();
    }
    worker.join().unwrap();
    completed.expect("cancel's wake permit must survive until park");
    assert!(
        witness.upgrade().is_none(),
        "Off retains neither published nor worker-owned frame"
    );
}

const HEADER: &str = "[Script Info]\nScriptType: v4.00+\nPlayResX: 320\nPlayResY: 180\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Inter,24,&H0000FF00,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,0,0,7,0,0,0,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";

fn script(lines: &str) -> Arc<Source> {
    Arc::new(Source {
        id: next_source_id(),
        revision: 1,
        origin: Origin::Authored,
        content: Content::Script {
            bytes: format!("{HEADER}{lines}").into_bytes().into(),
            fonts: Arc::from([]),
        },
    })
}

fn job(source: &Arc<Source>, now_ms: i64, width: i32, height: i32, storage_width: i32, storage_height: i32) -> Job {
    Job {
        kind: Kind::Normal,
        key: Key {
            epoch: 1,
            source_id: source.id,
            revision: source.revision,
            width,
            height,
            storage_width,
            storage_height,
        },
        now_ms,
        seq: 1,
        source: source.clone(),
        queued: None,
    }
}

fn render(engine: &mut Engine, source: &Arc<Source>, now_ms: i64) -> Arc<Frame> {
    let frame = engine.render(&job(source, now_ms, 320, 180, 320, 180)).frame;
    assert_eq!(
        frame.error, None,
        "native fixture requires the built host library in pkg"
    );
    frame
}

fn pixel(frame: &Frame, x: i32, y: i32) -> [u8; 4] {
    let Some(r) = frame
        .rects
        .iter()
        .find(|r| x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height)
    else {
        return [0; 4];
    };
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
    let coded = engine.render(&job(&source, 100, 320, 180, 720, 480)).frame;
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
        render(&mut engine, &source, 2100).rects.is_empty(),
        "expired output clears"
    );

    let embedded = Arc::new(Source {
        id: next_source_id(),
        revision: 1,
        origin: Origin::Authored,
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
        packet_frame.rects, first.rects,
        "Matroska chunks and a standalone script must render identical composed pixels"
    );

    let moving = script("Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\an7\\move(20,80,160,80,0,1000)\\p1}m 0 0 l 20 0 20 20 0 20\n");
    let start = render(&mut engine, &moving, 0);
    let halfway = render(&mut engine, &moving, 500);
    assert_eq!(
        halfway.rects[0].x - start.rects[0].x,
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
        f.rects
            .iter()
            .flat_map(|r| r.rgba.chunks_exact(4))
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
        origin: Origin::Authored,
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
        rendered.rects, fallback.rects,
        "a late attached font replaces fallback even when the source identity is unchanged"
    );
    let korean = script("Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\pos(20,40)}한\n");
    let chinese = script("Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\pos(20,40)}中\n");
    let a = render(&mut engine, &korean, 100);
    let b = render(&mut engine, &chinese, 100);
    assert_ne!(
        a.rects, b.rects,
        "the bundled CJK fallback yields distinct glyphs, not identical missing-glyph boxes"
    );
}

/// The CJK face is the renderer's default path, not an `ass_add_font` memory font, so a line whose
/// style names a family nothing carries must still reach it and draw real glyphs.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_cjk_line_in_an_unknown_family_style_still_renders() {
    let mut engine = Engine::default();
    let line = |text: &str| {
        script(&format!(
            "Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{{\\fnNoSuchFamily\\pos(20,40)}}{text}\n"
        ))
    };
    let korean = render(&mut engine, &line("한"), 100);
    let chinese = render(&mut engine, &line("中"), 100);
    assert!(!korean.rects.is_empty(), "an unknown family still falls back to the CJK face");
    assert!(!chinese.rects.is_empty());
    assert_ne!(
        korean.rects, chinese.rects,
        "distinct glyphs, not identical missing-glyph boxes"
    );
}

/// The TV stress fixture regressed from 60 to 52 UI FPS when distant captions
/// were uploaded as one mostly transparent rectangle. Bound the useful pixels
/// independently of the empty distance between authored signs.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_sparse_signs_do_not_publish_the_transparent_canvas_between_them() {
    let source = script(concat!(
        "Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\an7\\pos(20,20)\\c&H0000FF&\\p1}m 0 0 l 20 0 20 20 0 20\n",
        "Dialogue: 1,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\an7\\pos(280,140)\\c&HFF0000&\\p1}m 0 0 l 20 0 20 20 0 20\n",
        "Dialogue: 2,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\an7\\move(120,80,200,80,0,1000)\\p1}m 0 0 l 20 0 20 20 0 20\n",
    ));
    let mut engine = Engine::default();
    let first = render(&mut engine, &source, 100);
    assert_eq!(pixel(&first, 25, 25), [255, 0, 0, 255]);
    assert_eq!(pixel(&first, 285, 145), [0, 0, 255, 255]);
    let bytes: usize = first.rects.iter().map(|r| r.rgba.len()).sum();
    assert!(
        bytes < 320 * 180,
        "sparse signs published {bytes} RGBA bytes"
    );
    assert_eq!(first.rects.len(), 3);
    let later = render(&mut engine, &source, 500);
    assert_ne!(first.serial, later.serial);
    assert_eq!(later.rects.len(), 3);
    for region in &later.rects {
        assert!(
            first
                .rects
                .iter()
                .any(|old| Arc::ptr_eq(&old.rgba, &region.rgba)),
            "unchanged signs and a pure translation retain their pixel allocations"
        );
    }
}

#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_region_unions_preserve_layers_and_transparent_holes() {
    let source = script(concat!(
        "Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\an7\\pos(20,90)\\c&HFF0000&\\p1}m 0 0 l 10 0 10 10 0 10\n",
        "Dialogue: 1,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\an7\\pos(20,20)\\c&H0000FF&\\p1}m 0 0 l 100 0 100 10 0 10\n",
        "Dialogue: 2,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\an7\\pos(110,20)\\c&H00FF00&\\p1}m 0 0 l 10 0 10 100 0 100\n",
    ));
    let frame = render(&mut Engine::default(), &source, 100);
    assert_eq!(
        frame.rects.len(),
        1,
        "the L-shaped union must absorb the earlier isolated sign"
    );
    assert_eq!(pixel(&frame, 25, 95), [0, 0, 255, 255]);
    assert_eq!(pixel(&frame, 25, 25), [255, 0, 0, 255]);
    assert_eq!(pixel(&frame, 115, 25), [0, 255, 0, 255]);
    assert_eq!(pixel(&frame, 50, 60), [0; 4]);
}

#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_fragmented_script_stays_bounded_without_dropping_signs() {
    let mut lines = String::new();
    for i in 0..70 {
        let (x, y) = (8 + (i % 18) * 16, 8 + (i / 18) * 36);
        lines.push_str(&format!(
            "Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{{\\an7\\pos({x},{y})\\p1}}m 0 0 l 4 0 4 4 0 4\n"));
    }
    let source = script(&lines);
    let frame = Engine::default().render(&job(&source, 100, 1920, 1080, 1920, 1080)).frame;
    assert_eq!(frame.error, None);
    assert!(frame.rects.len() <= MAX_REGIONS);
    for i in 0..70 {
        let (x, y) = ((10 + (i % 18) * 16) * 6, (10 + (i / 18) * 36) * 6);
        assert_eq!(
            pixel(&frame, x, y)[3],
            255,
            "sign {i} must survive region compaction"
        );
    }
}

#[test]
fn a_track_is_never_refused_for_the_count_or_size_of_its_fonts() {
    let font = |i: usize, len: usize| Font {
        name: format!("f{i}.ttf"),
        data: Arc::from(vec![1u8; len]),
    };
    let mut fonts: Vec<Font> = (0..200).map(|i| font(i, 2048)).collect();
    fonts.push(font(200, MAX_FONT_BYTES + 1));
    let mut s = source(vec![event(1, 0, 1000)]);
    let Content::Embedded { fonts: slot, .. } = &mut s.content else { panic!() };
    *slot = fonts.into();
    assert_eq!(validate(&s), Ok(()), "the renderer skips unusable fonts, the text still shows");
}

#[test]
fn validation_puts_no_count_on_the_events_of_a_track() {
    // only `validate`: the native guard (20 000) is not reached because the windows that feed it
    // (`ass_source`, `ass_script`) stay far under it, which their own tests prove
    let events = (0..30_000).map(|i| event(i, i as i64, 1000)).collect();
    assert_eq!(validate(&source(events)), Ok(()));
}

#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_a_thirty_thousand_event_sidecar_renders_around_the_playhead_and_after_a_backward_seek() {
    let t = |ms: i64| format!("{}:{:02}:{:02}.{:02}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000 / 10);
    let mut lines = String::new();
    for i in 0..30_000i64 {
        lines.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{{\\an7\\pos(10,10)\\p1}}m 0 0 l 50 0 50 50 0 50\n",
            t(i * 1000), t(i * 1000 + 900)));
    }
    let source = script(&lines);
    let mut engine = Engine::default();
    for now in [20_000_100, 5_000_100, 29_999_100] {
        let frame = render(&mut engine, &source, now);
        assert_eq!(pixel(&frame, 20, 20)[3], 255, "the sign is up at {now} ms");
    }
    assert_eq!(pixel(&render(&mut engine, &source, 20_000_950), 20, 20), [0; 4], "and gone between lines");
}

// ---- facade ABI 3: features, cache limits, line spacing, replace_track, grown font faces ----

/// A renderer holding `fonts` and the script `text`, opened the way `Engine::open` opens one.
fn native_with(fonts: &[Font], text: &str) -> Native {
    Native::open_data(fonts, text.as_bytes(), 1)
        .expect("native fixture requires the built host library in pkg")
}

fn native_key() -> Key {
    Key {
        epoch: 1,
        source_id: 1,
        revision: 1,
        width: 320,
        height: 180,
        storage_width: 320,
        storage_height: 180,
    }
}

/// The frame at `now_ms`; `None` is libass's "identical to the last frame".
fn native_frame(native: &mut Native, now_ms: i64) -> Option<Vec<Rect>> {
    native
        .render(native_key(), now_ms, &[])
        .expect("render of a valid track")
}

/// The pixel rows holding any ink, as `(first, last)` bands of consecutive rows, and the columns
/// the ink spans.
struct Ink {
    bands: Vec<(i32, i32)>,
    left: i32,
    right: i32,
}

fn ink(rects: &[Rect]) -> Ink {
    let mut rows = std::collections::BTreeSet::new();
    let (mut left, mut right) = (i32::MAX, i32::MIN);
    for rect in rects {
        for y in 0..rect.height {
            for x in 0..rect.width {
                if rect.rgba[((y * rect.width + x) * 4 + 3) as usize] > 8 {
                    rows.insert(rect.y + y);
                    left = left.min(rect.x + x);
                    right = right.max(rect.x + x + 1);
                }
            }
        }
    }
    let mut bands: Vec<(i32, i32)> = Vec::new();
    for y in rows {
        match bands.last_mut() {
            Some((_, last)) if *last + 1 == y => *last = y,
            _ => bands.push((y, y)),
        }
    }
    Ink { bands, left, right }
}

const FACE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/ass-faces");

/// The 14 committed one-glyph faces of the family `PlxFaces` (`tests/fixtures/make_ass_faces.py`).
fn fixture_faces(count: usize) -> Vec<Font> {
    (0..count)
        .map(|i| Font {
            name: format!("face-{i:02}.ttf"),
            data: std::fs::read(format!("{FACE_DIR}/face-{i:02}.ttf"))
                .expect("committed fixture face")
                .into(),
        })
        .collect()
}

/// A script in the `PlxFaces` family: a 320x180 canvas, 100 px type, top-left, no margins.
fn faces_script(lines: &str) -> String {
    HEADER.replace("Style: Default,Inter,24", "Style: Default,PlxFaces,100") + lines
}

#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_the_facade_is_abi_four() {
    assert!(native_ass::load(Some(asset_dir())).ok());
    assert_eq!(unsafe { plx_ass_abi_version() }, 4);
    assert_eq!(NATIVE_ABI, 4);
}

/// The bundled libass was built without libunibreak until the ABI 3 bump, so a CJK line with no
/// space could not wrap at all: it ran off the edge of the frame.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_a_long_cjk_line_wraps_inside_the_margins_with_unicode_line_breaking() {
    let cue = format!(
        "Dialogue: 0,0:00:00.00,0:00:02.00,Default,,20,20,0,,{{\\an7\\pos(20,10)}}{}\n",
        "中".repeat(30)
    );
    let text = format!("{HEADER}{cue}");

    let mut plain = native_with(&[], &text);
    let unwrapped = ink(&native_frame(&mut plain, 100).expect("first frame"));
    assert_eq!(unwrapped.bands.len(), 1, "without the rules one CJK line stays one line");
    assert!(unwrapped.right > 300, "and runs past the right margin: {}", unwrapped.right);

    let mut wrapping = native_with(&[], &text);
    assert!(wrapping.set_wrap_unicode(true), "the bundled libass has libunibreak");
    let wrapped = ink(&native_frame(&mut wrapping, 100).expect("first frame"));
    assert!(wrapped.bands.len() >= 2, "a 30-glyph line wraps: {:?}", wrapped.bands);
    assert!(wrapped.left >= 20 - 1, "inside the left margin: {}", wrapped.left);
    assert!(wrapped.right <= 300 + 1, "inside the right margin: {}", wrapped.right);
}

#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_line_spacing_changes_the_line_pitch_by_exactly_its_value() {
    let cue = "Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\an7\\pos(10,10)}\u{e000}\\N\u{e000}\n";
    let text = faces_script(cue);
    let pitch = |spacing: f64| {
        let mut native = native_with(&fixture_faces(1), &text);
        assert!(native.set_line_spacing(spacing));
        let bands = ink(&native_frame(&mut native, 100).expect("first frame")).bands;
        assert_eq!(bands.len(), 2, "two lines at spacing {spacing}: {bands:?}");
        bands[1].0 - bands[0].0
    };
    let base = pitch(0.0);
    assert_eq!(base, 100, "one line is one em when nothing is added");
    assert_eq!(pitch(-3.5) - base, -4, "-3.5 px tightens the pitch (rounded to the grid)");
    assert_eq!(pitch(10.0) - base, 10);
    let mut native = native_with(&[], &text);
    assert!(!native.set_line_spacing(f64::NAN), "a non-finite spacing is refused");
    assert!(!native.set_line_spacing(1e6), "so is one beyond the bound");
}

#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_cache_limits_are_validated() {
    let native = native_with(&[], &script_text("Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,x\n"));
    unsafe {
        assert_eq!(plx_ass_set_cache_limits(native.0, 400, 12), 0);
        assert_eq!(plx_ass_set_cache_limits(native.0, 0, 12), -1);
        assert_eq!(plx_ass_set_cache_limits(native.0, 400, 0), -1);
        assert_eq!(plx_ass_set_cache_limits(std::ptr::null_mut(), 400, 12), -1);
    }
}

fn script_text(lines: &str) -> String {
    format!("{HEADER}{lines}")
}

/// A track replaced in place keeps every face the renderer opened: `ass_set_fonts` (which reopens
/// every font in memory) runs once per renderer, and the frame after a replace is a full frame
/// even when libass would call it unchanged.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_replace_track_keeps_the_fonts_and_the_next_frame_is_never_unchanged() {
    let one = faces_script("Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\an7\\pos(10,10)}\u{e000}\n");
    let mut native = native_with(&fixture_faces(2), &one);
    let before = native.stats();
    assert_eq!((before.font_scans, before.tracks), (1, 1));
    assert_eq!(
        before.library_fonts,
        bundled_faces() + 2,
        "the bundled faces (the CJK face is the default path) and the two fixtures"
    );
    let first = native_frame(&mut native, 100).expect("first frame");
    assert_eq!(ink(&first).bands.len(), 1);
    assert!(native_frame(&mut native, 100).is_none(), "the same instant is unchanged");

    // The same text replaced in place: the frame is new, never "unchanged".
    native.replace_track(one.as_bytes(), 1).expect("replace");
    let again = native_frame(&mut native, 100).expect("a replaced track starts with a full frame");
    assert_eq!(again, first, "and it is the same picture");
    // A different track at the same instant shows its own events.
    let other = faces_script("Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\an7\\pos(10,10)}\u{e001}\n");
    native.replace_track(other.as_bytes(), 1).expect("replace");
    let second = native_frame(&mut native, 100).expect("full frame");
    assert_ne!(second, first, "the second face's square is bigger");
    // An empty replaced track clears the picture rather than leaving the old one.
    let none = faces_script("Dialogue: 0,0:00:10.00,0:00:12.00,Default,,0,0,0,,x\n");
    native.replace_track(none.as_bytes(), 1).expect("replace");
    assert_eq!(native_frame(&mut native, 100), Some(Vec::new()));

    let after = native.stats();
    assert_eq!(after.font_scans, 1, "no face was reopened by a replace");
    assert_eq!(after.tracks, 4);
    assert_eq!(after.library_fonts, before.library_fonts);
    assert!(native.replace_track(b"not a script", 1).is_err());
}

/// ASS `[Fonts]` encoding: 3 bytes to 4 characters of `6 bits + 33`, 80 characters a line.
fn ass_font_section(name: &str, data: &[u8]) -> String {
    let mut encoded = String::new();
    for chunk in data.chunks(3) {
        let mut word = [0u8; 3];
        word[..chunk.len()].copy_from_slice(chunk);
        let six = [
            word[0] >> 2,
            (word[0] & 3) << 4 | word[1] >> 4,
            (word[1] & 15) << 2 | word[2] >> 6,
            word[2] & 63,
        ];
        encoded.extend(six[..chunk.len() + 1].iter().map(|&v| char::from(v + 33)));
    }
    let lines: Vec<&str> = encoded
        .as_bytes()
        .chunks(80)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();
    format!("[Fonts]\nfontname: {name}\n{}\n\n", lines.join("\n"))
}

/// A sidecar script with an embedded font and `events` one-second signs, each a different glyph's
/// worth of text at its own second.
fn font_script(font_name: &str, face: usize, events: usize) -> Arc<Source> {
    let data = std::fs::read(format!("{FACE_DIR}/face-{face:02}.ttf")).unwrap();
    let (styles, events_section) = HEADER.split_once("[Events]").unwrap();
    let mut text = styles.replace("Style: Default,Inter,24", "Style: Default,PlxFaces,100");
    text.push_str(&ass_font_section(font_name, &data));
    text.push_str("[Events]");
    text.push_str(events_section);
    let t = |ms: i64| format!("{}:{:02}:{:02}.{:02}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000 / 10);
    for i in 0..events as i64 {
        text.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{{\\an7\\pos(10,10)}}{}\n",
            t(i * 1000),
            t(i * 1000 + 900),
            char::from_u32(0xE000 + face as u32).unwrap()
        ));
    }
    Arc::new(Source {
        id: next_source_id(),
        revision: 1,
        origin: Origin::Authored,
        content: Content::Script { bytes: text.into_bytes().into(), fonts: Arc::from([]) },
    })
}

/// Every window of a long script repeats the script's `[Fonts]`. On the reused renderer each slide
/// would add another copy of the font to the library for nothing, because the faces are only read
/// when a renderer first loads.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_fifty_slides_of_a_script_with_embedded_fonts_keep_the_font_count_flat() {
    let source = font_script("slide.ttf", 3, 20_000);
    let mut engine = Engine::default();
    let first = render(&mut engine, &source, 100);
    assert_eq!(ink(&first.rects).bands.len(), 1, "the embedded face draws its square");
    let opened = engine.native.as_ref().unwrap().stats();
    assert_eq!(opened.library_fonts, bundled_faces() + 1, "the bundled faces and the one from [Fonts]");
    for i in 0..50 {
        // Far apart, so every render leaves the window the last one fed.
        let now = if i % 2 == 0 { 19_000_100 } else { 100 };
        let frame = render(&mut engine, &source, now);
        assert_eq!(ink(&frame.rects).bands.len(), 1, "slide {i}: the font still renders");
    }
    let slid = engine.native.as_ref().unwrap().stats();
    assert_eq!(slid.library_fonts, opened.library_fonts, "no font was added by a slide");
    assert_eq!(slid.font_scans, 1, "and no face was reopened");
    assert!(slid.tracks > 40, "the slides did replace the track: {}", slid.tracks);
}

/// A switch between two authored scripts, each with its own `[Fonts]`, is a new renderer, and the
/// second script's font renders.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_a_switch_between_authored_scripts_with_different_fonts_opens_one_new_renderer() {
    let a = font_script("a.ttf", 5, 4);
    let b = font_script("b.ttf", 9, 4);
    let mut engine = Engine::default();
    let frame = render(&mut engine, &a, 100);
    let a_ink = ink(&frame.rects);
    assert_eq!(a_ink.bands.len(), 1);
    let opened = engine.native.as_ref().unwrap().stats();
    let frame = render(&mut engine, &b, 100);
    let b_ink = ink(&frame.rects);
    assert_eq!(b_ink.bands.len(), 1, "script B's own face renders");
    assert_ne!(
        b_ink.bands[0].1 - b_ink.bands[0].0,
        a_ink.bands[0].1 - a_ink.bands[0].0,
        "and it is B's face (a bigger square), not A's"
    );
    let switched = engine.native.as_ref().unwrap().stats();
    assert_eq!(
        (switched.font_scans, switched.tracks, switched.library_fonts),
        (1, 1, opened.library_fonts),
        "the new renderer scanned its fonts once, for B alone"
    );
}

/// libass fixes one font at 10 faces upstream; the bundled build grows the arrays. 14 faces of one
/// family, one glyph each, on one renderer, one cue per glyph: every glyph must draw. On unpatched
/// 0.17.5 the 11th glyph onward found no room for its face and drew `.notdef`, which is empty here.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_fourteen_faces_of_one_family_all_render() {
    let mut cues = String::new();
    for i in 0..14u32 {
        cues.push_str(&format!(
            "Dialogue: 0,0:00:{:02}.00,0:00:{:02}.00,Default,,0,0,0,,{{\\an7\\pos(10,10)}}{}\n",
            i,
            i + 1,
            char::from_u32(0xE000 + i).unwrap()
        ));
    }
    let mut native = native_with(&fixture_faces(14), &faces_script(&cues));
    for i in 0..14i32 {
        let rects = native_frame(&mut native, i64::from(i) * 1000 + 500).expect("a new cue");
        let drawn = ink(&rects);
        assert_eq!(drawn.bands.len(), 1, "glyph {i} drew nothing (.notdef)");
        let side = (400 + 20 * i) * 100 / 1000;
        let (top, bottom) = drawn.bands[0];
        assert!(
            (bottom - top + 1 - side).abs() <= 2 && (drawn.right - drawn.left - side).abs() <= 2,
            "glyph {i}: the square of face {i} ({side} px), got {}x{}",
            drawn.right - drawn.left,
            bottom - top + 1
        );
    }
    // Back to the first: the faces stay for the renderer's lifetime.
    assert_eq!(ink(&native_frame(&mut native, 500).unwrap()).bands.len(), 1);
}

// ---- Source.origin, the restyle branch and the alignment classes (plan §2.4, §2.5, §4.6 case 3) ----

use crate::player::text_ass::{self, AlignClass};
use crate::player::text_style::{self, TextStyle};
use plx_plex::plex::session::{SubtitlePosition, SubtitleSize};

/// A Text-origin embedded track with the synthesized header for `style` and the `(start, end,
/// Style, text)` cues, as a Store holds one. A restyle keeps `id` and bumps `revision`.
fn text_source(id: u64, revision: u64, style: &TextStyle, cues: &[(i64, i64, &str, &str)]) -> Arc<Source> {
    let events: Vec<Event> = cues
        .iter()
        .enumerate()
        .map(|(n, (start, end, style_name, body))| Event {
            start_ms: *start,
            duration_ms: end - start,
            payload: format!("{n},0,{style_name},,0,0,0,,{body}").into_bytes().into(),
        })
        .collect();
    Arc::new(Source {
        id,
        revision,
        origin: Origin::Text(*style),
        content: Content::Embedded {
            header: text_ass::header(style).into_bytes().into(),
            events: events.into(),
            fonts: Arc::from([]),
        },
    })
}

// ---- the bundled subtitle faces (plan D6): scripts reached by family name ----
//
// What a missing glyph looks like: a character no face covers is drawn from the CJK default face
// without a coverage check, as that face's `.notdef` box. So "this text rendered" is never `ink`
// alone (the box has ink); it is "this text does NOT render as the boxes a codepoint nobody has
// would". U+10FFFD is such a codepoint, and the same number of them is the reference.

fn ass_time(ms: i64) -> String {
    format!("0:{:02}:{:02}.{:02}", ms / 60_000, ms / 1000 % 60, ms % 1000 / 10)
}

/// A 320x180 script whose default style is `family` at `size`, with one top-left cue per entry.
fn sub_script(family: &str, size: u32, cues: &[(i64, i64, &str)]) -> String {
    let mut text = HEADER.replace("Style: Default,Inter,24", &format!("Style: Default,{family},{size}"));
    for (start, end, body) in cues {
        text.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{{\\an7\\pos(10,10)}}{body}\n",
            ass_time(*start),
            ass_time(*end)
        ));
    }
    text
}

/// The same number of codepoints no face carries.
fn nothing_like(text: &str) -> String {
    "\u{10FFFD}".repeat(text.chars().count())
}

/// One cue rendered by a renderer of its own.
fn drawn(family: &str, size: u32, body: &str) -> Vec<Rect> {
    let mut native = native_with(&[], &sub_script(family, size, &[(0, 2000, body)]));
    native_frame(&mut native, 100).expect("the first frame")
}

fn width(rects: &[Rect]) -> i32 {
    let ink = ink(rects);
    ink.right - ink.left
}

/// How many faces every renderer starts with: Inter, Inter Bold and the manifest's Noto faces.
fn bundled_faces() -> u32 {
    fonts::bundled(asset_dir()).expect("the bundled faces").len() as u32
}

fn manifest_value() -> serde_json::Value {
    let path = asset_dir().join("fonts/subs/manifest.json");
    serde_json::from_slice(&std::fs::read(path).expect("pkg/fonts/subs/manifest.json")).expect("a JSON manifest")
}

/// Authored ASS that names a family nothing carries falls to "Inter" and, for Thai, used to fall to
/// the CJK default's `.notdef` boxes. RED without the Noto faces: both renderings are the boxes.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_thai_in_an_unknown_family_renders_thai_not_tofu() {
    let thai = "กขคงจ";
    let unknown = drawn("NoSuchFamily", 40, thai);
    assert!(!ink(&unknown).bands.is_empty());
    assert_ne!(
        unknown,
        drawn("NoSuchFamily", 40, &nothing_like(thai)),
        "Thai in an unknown family is drawn from a Thai face, not as missing-glyph boxes"
    );
    assert_eq!(
        unknown,
        drawn("PlxSubs", 40, thai),
        "the unknown family falls to Inter, whose Noto aliases answer for Thai: the same glyphs as PlxSubs"
    );
    assert_eq!(unknown, drawn("Inter", 40, thai), "and so does the UI family itself");
}

/// Thai vowel and tone marks sit on the consonant: shaping, not a row of boxes.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_thai_marks_attach_to_the_consonant() {
    let base = ink(&drawn("PlxSubs", 80, "ก"));
    let marked = ink(&drawn("PlxSubs", 80, "กั"));
    assert!(!base.bands.is_empty());
    assert!(
        marked.bands[0].0 < base.bands[0].0,
        "the mark rises above the consonant: {:?} vs {:?}",
        marked.bands,
        base.bands
    );
    assert!((marked.right - base.right).abs() <= 10, "and adds no advance of its own (a mark may overhang; a letter would add ~40): {} vs {}", marked.right, base.right);
    let toned = ink(&drawn("PlxSubs", 80, "ก่"));
    assert!(toned.bands[0].0 < base.bands[0].0, "the tone mark rises: {:?} vs {:?}", toned.bands, base.bands);
    assert!((toned.right - base.right).abs() <= 10, "and adds no advance: {} vs {}", toned.right, base.right);
}

/// क्ष is one conjunct. A ZWNJ between its letters asks for the visible virama form instead, so the
/// two spellings differ, and the conjunct is the narrower one.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_devanagari_conjunct_shapes_and_a_zwnj_breaks_it() {
    let conjunct = drawn("PlxSubs", 80, "क्ष");
    let broken = drawn("PlxSubs", 80, "क्\u{200c}ष");
    assert_ne!(conjunct, broken);
    assert!(width(&conjunct) < width(&broken), "{} vs {}", width(&conjunct), width(&broken));
    assert_ne!(conjunct, drawn("PlxSubs", 80, &nothing_like("क्ष")));
}

/// The joiner is resolved by libass per glyph run, within the faces the font has opened so far, so
/// which script a track met first must not change a later Devanagari cue.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_a_zwnj_cue_draws_the_same_after_any_other_script() {
    let dev = "क्\u{200c}ष क्ष";
    let arabic = "بـب مرحبا";
    let alone = {
        let mut native = native_with(&[], &sub_script("PlxSubs", 60, &[(0, 1000, dev)]));
        native_frame(&mut native, 500).expect("alone")
    };
    let arabic_first = {
        let mut native =
            native_with(&[], &sub_script("PlxSubs", 60, &[(0, 1000, arabic), (2000, 3000, dev)]));
        assert!(!ink(&native_frame(&mut native, 500).expect("arabic")).bands.is_empty());
        native_frame(&mut native, 2500).expect("devanagari after arabic")
    };
    let arabic_second = {
        let mut native =
            native_with(&[], &sub_script("PlxSubs", 60, &[(0, 1000, dev), (2000, 3000, arabic)]));
        let first = native_frame(&mut native, 500).expect("devanagari first");
        assert!(!ink(&native_frame(&mut native, 2500).expect("arabic")).bands.is_empty());
        first
    };
    assert_eq!(alone, arabic_first, "Devanagari after Arabic is the Devanagari of a fresh renderer");
    assert_eq!(alone, arabic_second, "and Arabic after it changes nothing about it");
}

/// U+0640 is in the Arabic face only (Inter has none): a kashida lengthens the join.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_arabic_tatweel_lengthens_the_join() {
    let joined = drawn("PlxSubs", 80, "بب");
    let stretched = drawn("PlxSubs", 80, "ب\u{640}ب");
    assert!(width(&stretched) >= width(&joined) + 4, "{} vs {}", width(&stretched), width(&joined));
    assert_ne!(stretched, drawn("PlxSubs", 80, &nothing_like("ب\u{640}ب")));
}

/// libass sizes a face from its win ascent plus descent. The cut rewrites every Noto face to
/// Inter Bold's metrics (usWinAscent 2269 + usWinDescent 660 per 2048 em, 1.430 em), so a Thai and a
/// Devanagari base letter come out at their design height in THAT em, whatever the face's own
/// metrics were (Thai's were 1.511 em). The expectation uses Inter Bold's constants, never the
/// face's own win values, so a face that kept its own metrics fails it.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_noto_glyphs_are_as_tall_as_their_design_in_the_same_em_as_inter() {
    const INTER_BOLD_WIN_EM: f64 = (2269.0 + 660.0) / 2048.0;
    let manifest = manifest_value();
    let size = 100.0;
    let mut probed = 0;
    for face in manifest["faces"].as_array().unwrap() {
        let probe = &face["probe"];
        if probe.is_null() {
            continue;
        }
        probed += 1;
        let design_em = (probe["y_max"].as_f64().unwrap() - probe["y_min"].as_f64().unwrap())
            / probe["upem"].as_f64().unwrap();
        let expected = design_em / INTER_BOLD_WIN_EM * size;
        let text = probe["char"].as_str().unwrap();
        let bands = ink(&drawn("PlxSubs", 100, text)).bands;
        let (top, bottom) = (bands.first().unwrap().0, bands.last().unwrap().1);
        let got = f64::from(bottom - top + 1);
        assert!(
            (got - expected).abs() <= 1.0,
            "{}: {text:?} is {got} px tall at {size} px, Inter Bold's em says {expected:.1}",
            face["script"],
        );
    }
    assert!(probed >= 2, "Thai and Devanagari are probed");
}

/// D13, shipped: one renderer, one track, consecutive cues in Latin, CJK and every shipped script,
/// ONE CODEPOINT PER CUE. A cue of several letters would pass with one tofu among them, so each
/// is compared with a lone U+10FFFD box (what any uncovered codepoint draws as).
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_every_shipped_script_latin_and_cjk_render_on_one_renderer() {
    let manifest = manifest_value();
    let mut samples: Vec<(String, String)> = vec![
        ("Latin".into(), "Hello, World ♪«¿?»".into()),
        ("Hangul".into(), "한국어".into()),
        ("Han".into(), "中文".into()),
        ("Kana".into(), "ひらがなカタカナ".into()),
    ];
    for face in manifest["faces"].as_array().unwrap() {
        samples.push((face["script"].as_str().unwrap().into(), face["sample"].as_str().unwrap().into()));
    }
    assert!(samples.len() >= 24);
    // Each distinct codepoint once: a repeat ("ll") is an unchanged frame, not a new cue.
    let mut distinct = std::collections::HashSet::new();
    let letters: Vec<(&str, String)> = samples
        .iter()
        .flat_map(|(script, text)| text.chars().map(move |c| (script.as_str(), c)))
        .filter(|(_, c)| !c.is_whitespace() && distinct.insert(*c))
        .map(|(script, c)| (script, c.to_string()))
        .collect();
    let rows: Vec<(i64, i64, &str)> = letters
        .iter()
        .enumerate()
        .map(|(i, (_, s))| (i as i64 * 1000, i as i64 * 1000 + 900, s.as_str()))
        .collect();
    let mut real = native_with(&[], &sub_script("PlxSubs", 40, &rows));
    let tofu = drawn("PlxSubs", 40, "\u{10FFFD}");
    for (i, (script, text)) in letters.iter().enumerate() {
        let now = i as i64 * 1000 + 500;
        let a = native_frame(&mut real, now).expect("a new cue");
        assert!(!ink(&a).bands.is_empty(), "{script}: {text:?} drew nothing");
        assert_ne!(a, tofu, "{script}: {text:?} (U+{:04X}) drew as a missing-glyph box", text.chars().next().unwrap() as u32);
    }
    assert_eq!(
        real.stats().library_fonts,
        bundled_faces(),
        "drawing adds no face: the renderer holds the bundled set (Inter, Inter Bold, the Noto faces)"
    );
}

/// libass skips U+2060 (and the other default-ignorables, `ass_shaper_skip_characters`) before it
/// looks a glyph up, so a word joiner inside Thai text is neither a box nor a change to the line,
/// though no shipped face carries it. This holds the claim in `tools/cut-noto-subs.py`.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_a_word_joiner_in_thai_is_skipped_not_drawn_as_a_box() {
    assert_eq!(drawn("PlxSubs", 60, "กข\u{2060}ค"), drawn("PlxSubs", 60, "กขค"));
}

/// Plan 3.4: Latin, Thai and Han in ONE cue. Each run takes its face (Inter Bold, Noto Thai, the
/// CJK default) and none falls to boxes, so the cue is wider than any two of its parts.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_latin_thai_and_han_share_one_cue() {
    let mixed = "Hi กขค 中文";
    let drawn_mixed = drawn("PlxSubs", 50, mixed);
    assert_ne!(drawn_mixed, drawn("PlxSubs", 50, &nothing_like(mixed)));
    for part in ["Hi กขค", "กขค 中文", "Hi 中文"] {
        assert!(width(&drawn_mixed) > width(&drawn("PlxSubs", 50, part)), "{part:?}");
    }
}

fn open_calls() -> u32 {
    OPEN_DATA_CALLS.with(|c| c.get())
}

const RED: &str = r"{\an7\pos(20,20)\c&H0000FF&\p1}m 0 0 l 40 0 40 40 0 40";
const BLUE: &str = r"{\an7\pos(100,100)\c&HFF0000&\p1}m 0 0 l 40 0 40 40 0 40";

/// An embedded track with `header`, one event `(start, duration, drawing)` per entry.
fn track(header: &str, events: &[(i64, i64, &str)]) -> Arc<Source> {
    Arc::new(Source {
        id: next_source_id(),
        revision: 1,
        origin: Origin::Authored,
        content: Content::Embedded {
            header: header.as_bytes().into(),
            fonts: Arc::from([]),
            events: events
                .iter()
                .enumerate()
                .map(|(n, (start_ms, duration_ms, text))| Event {
                    start_ms: *start_ms,
                    duration_ms: *duration_ms,
                    payload: format!("{n},0,Default,,0,0,0,,{text}").into_bytes().into(),
                })
                .collect::<Vec<_>>()
                .into(),
        },
    })
}

fn style(size: SubtitleSize, position: SubtitlePosition) -> TextStyle {
    TextStyle { size, position }
}

/// Render a Text-origin source into the full logical screen, as the screen asks for it.
fn text_render(engine: &mut Engine, source: &Arc<Source>, now_ms: i64) -> Arc<Frame> {
    let frame = engine
        .render(&job(source, now_ms, 1920, 1080, 1920, 1080))
        .frame;
    assert_eq!(frame.error, None, "native fixture requires the built host library in pkg");
    frame
}

fn no_cues_style() -> TextStyle {
    style(SubtitleSize::Medium, SubtitlePosition::Low)
}

#[test]
fn restyle_applies_to_text_origin_tracks_of_equal_fonts_and_a_different_header_only() {
    let medium = text_source(1, 1, &no_cues_style(), &[]);
    let large = text_source(1, 2, &style(SubtitleSize::Large, SubtitlePosition::Low), &[]);
    assert!(restyles(&medium, &large), "a new size is a restyle");
    assert!(!restyles(&medium, &medium), "an equal header is the flush path, not a restyle");
    // Another seek's id does not matter: the renderer is kept by resources, not identity.
    let seek = text_source(7, 1, &style(SubtitleSize::Large, SubtitlePosition::High), &[]);
    assert!(restyles(&medium, &seek));

    let authored = |header: &'static [u8]| Source {
        id: 1,
        revision: 1,
        origin: Origin::Authored,
        content: Content::Embedded { header: Arc::from(header), events: Arc::from([]), fonts: Arc::from([]) },
    };
    assert!(!restyles(&authored(b"a"), &authored(b"b")), "an authored header change is a new renderer");
    assert!(!restyles(&medium, &authored(b"b")) && !restyles(&authored(b"b"), &medium), "origins must both be Text");

    let font = |data: &'static [u8]| -> Arc<[Font]> { Arc::from([Font { name: "f.ttf".into(), data: Arc::from(data) }]) };
    let mut other_fonts = (*large).clone();
    other_fonts.content = match &large.content {
        Content::Embedded { header, events, .. } => {
            Content::Embedded { header: header.clone(), events: events.clone(), fonts: font(b"x") }
        }
        script => script.clone(),
    };
    assert!(!restyles(&medium, &other_fonts), "different fonts need their own renderer");
    let script = |bytes: &'static [u8]| Source {
        id: 1,
        revision: 1,
        origin: Origin::Text(no_cues_style()),
        content: Content::Script { bytes: Arc::from(bytes), fonts: Arc::from([]) },
    };
    assert!(restyles(&script(b"a"), &script(b"b")));
    assert!(!restyles(&medium, &script(b"b")), "an embedded track and a script are different kinds");
}

#[test]
fn aligns_are_read_from_the_style_name_of_each_active_embedded_event() {
    let cues = [
        (0, 1000, "Default", "bottom"),
        (0, 3000, "An8", "top"),
        (2000, 3000, "An5", "middle"),
        (2000, 3000, "An1", "bottom left"),
    ];
    let source = text_source(1, 1, &no_cues_style(), &cues);
    let mut engine = Engine::default();
    let at = |engine: &mut Engine, now| engine.aligns(&source, now).unwrap();
    assert_eq!(at(&mut engine, 500), AlignSet { bottom: true, middle: false, top: true });
    assert_eq!(at(&mut engine, 1500), AlignSet { bottom: false, middle: false, top: true }, "the bottom cue ended");
    assert_eq!(at(&mut engine, 2500), AlignSet { bottom: true, middle: true, top: true });
    assert_eq!(at(&mut engine, 3000), AlignSet::default(), "end is exclusive, as libass reads it");
    assert!(at(&mut engine, 3000).is_empty());
    assert!(at(&mut engine, 2500).contains(AlignClass::Middle));
}

#[test]
fn aligns_are_read_from_the_style_name_of_each_active_script_line() {
    let lines = |n: usize| -> Arc<Source> {
        let mut text = text_ass::header(&no_cues_style());
        text.push_str("Dialogue: 0,0:00:00.00,0:00:02.00,An7,,0,0,0,,top\n");
        text.push_str("Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,low\n");
        text.push_str("Dialogue: 0,0:00:02.50,0:00:04.00,An4,,0,0,0,,mid\n");
        Arc::new(Source {
            id: n as u64,
            revision: 1,
            origin: Origin::Text(no_cues_style()),
            content: Content::Script { bytes: text.into_bytes().into(), fonts: Arc::from([]) },
        })
    };
    let source = lines(1);
    let mut engine = Engine::default();
    assert_eq!(engine.aligns(&source, 500), Some(AlignSet { bottom: false, middle: false, top: true }));
    assert_eq!(engine.aligns(&source, 1500), Some(AlignSet { bottom: true, middle: false, top: true }));
    assert_eq!(engine.aligns(&source, 2700), Some(AlignSet { bottom: true, middle: true, top: false }));
    assert_eq!(engine.aligns(&source, 4000), Some(AlignSet::default()));
    // A new revision is parsed again, not answered from the previous parse.
    let mut next = (*source).clone();
    next.revision = 2;
    next.content = match &lines(1).content {
        Content::Script { bytes, fonts } => {
            let mut b = bytes.to_vec();
            b.extend_from_slice(b"Dialogue: 0,0:00:03.50,0:00:05.00,An9,,0,0,0,,more top\n");
            Content::Script { bytes: b.into(), fonts: fonts.clone() }
        }
        other => other.clone(),
    };
    assert_eq!(engine.aligns(&Arc::new(next), 4200), Some(AlignSet { bottom: false, middle: false, top: true }));
}

#[test]
fn renderer_settings_follow_the_origin_and_the_picks_it_carries() {
    assert_eq!(Origin::Authored.renderer_settings(), None);
    for size in [SubtitleSize::Medium, SubtitleSize::Large] {
        let picks = style(size, SubtitlePosition::Low);
        assert_eq!(Origin::Text(picks).renderer_settings(), Some((text_style::line_spacing(picks.scale()), true)));
    }
    let spacing = |size| Origin::Text(style(size, SubtitlePosition::Low)).renderer_settings().unwrap().0;
    assert_ne!(spacing(SubtitleSize::Large), spacing(SubtitleSize::Medium));
}

#[test]
fn an_authored_source_publishes_no_aligns() {
    let mut engine = Engine::default();
    assert_eq!(engine.aligns(&Arc::new(source(vec![event(0, 0, 1000)])), 500), None);
    assert_eq!(engine.aligns(&script("Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,x\n"), 500), None);
}

/// One restyle is no new renderer: the held one keeps its faces, takes the new size at once, and
/// takes the new size's line spacing.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_one_restyle_opens_no_new_renderer_and_renders_the_new_size_and_spacing() {
    let _guard = plx_base::testlock::serial();
    let one = [(0, 60_000, "Default", "HELLO")];
    let two = [(0, 60_000, "Default", "HELLO\\NHELLO")];
    let pick = |size, position| {
        crate::route::restore_subtitle_size(size);
        crate::route::restore_subtitle_position(position);
        style(size, position)
    };
    let mut engine = Engine::default();
    let medium = pick(SubtitleSize::Medium, SubtitlePosition::Low);
    let a = text_render(&mut engine, &text_source(1, 1, &medium, &one), 500);
    let a_ink = ink(&a.rects);
    assert_eq!(a_ink.bands.len(), 1);
    let opened = engine.native.as_ref().unwrap().stats();
    assert_eq!((opened.font_scans, opened.tracks), (1, 1));

    // Larger: the same renderer, a replaced track, a bigger letter.
    let large = pick(SubtitleSize::Large, SubtitlePosition::Low);
    let b = text_render(&mut engine, &text_source(1, 2, &large, &one), 500);
    let b_ink = ink(&b.rects);
    let (a_h, b_h) = (a_ink.bands[0].1 - a_ink.bands[0].0 + 1, b_ink.bands[0].1 - b_ink.bands[0].0 + 1);
    assert!(
        (f64::from(b_h) / f64::from(a_h) - 1.25).abs() < 0.08,
        "Large draws 1.25x the letter height of Medium: {a_h} -> {b_h}"
    );
    let restyled = engine.native.as_ref().unwrap().stats();
    assert_eq!(
        (restyled.font_scans, restyled.library_fonts, restyled.tracks),
        (1, opened.library_fonts, 2),
        "the held renderer's track was replaced; no renderer was created and no face reopened"
    );

    // Higher: the baseline moves up by the Position's lift (320 px), nothing else changes.
    let high = pick(SubtitleSize::Large, SubtitlePosition::High);
    let c = text_render(&mut engine, &text_source(1, 3, &high, &one), 500);
    let c_ink = ink(&c.rects);
    assert!(
        (b_ink.bands[0].1 - c_ink.bands[0].1 - 320).abs() <= 2,
        "High lifts the baseline 320 px above Low: {} -> {}",
        b_ink.bands[0].1,
        c_ink.bands[0].1
    );

    // The line spacing follows the size: a two-line cue's pitch is 48 px x scale (Large: 60 px).
    let d = text_render(&mut engine, &text_source(1, 4, &large, &two), 500);
    let d_ink = ink(&d.rects);
    assert_eq!(d_ink.bands.len(), 2, "two lines");
    let pitch = d_ink.bands[1].0 - d_ink.bands[0].0;
    assert!((pitch - 60).abs() <= 1, "Large keeps the 48 x 1.25 line pitch, got {pitch}");
    let last = engine.native.as_ref().unwrap().stats();
    assert_eq!((last.font_scans, last.tracks), (1, 4), "four tracks on one renderer");

    // Back to Medium: 48 px.
    let again = pick(SubtitleSize::Medium, SubtitlePosition::Low);
    let e = text_render(&mut engine, &text_source(1, 5, &again, &two), 500);
    let e_ink = ink(&e.rects);
    assert_eq!(e_ink.bands.len(), 2);
    let pitch = e_ink.bands[1].0 - e_ink.bands[0].0;
    assert!((pitch - 48).abs() <= 1, "Medium keeps the 48 px pitch, got {pitch}");
    assert_eq!(engine.native.as_ref().unwrap().stats().font_scans, 1);
}

/// A renderer reached by a fresh open takes the same settings as one reached by a restyle (plan
/// §4.6): the line pitch of the size its header was built for, never the viewer's picks now.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_a_fresh_open_of_a_text_track_has_the_line_spacing_of_its_own_size() {
    let _guard = plx_base::testlock::serial();
    let two = [(0, 60_000, "Default", "HELLO\\NHELLO")];
    let pitch_of = |engine: &mut Engine, picked: &TextStyle, built_for: &TextStyle| {
        crate::route::restore_subtitle_size(picked.size);
        crate::route::restore_subtitle_position(picked.position);
        let frame = text_render(engine, &text_source(next_source_id(), 1, built_for, &two), 500);
        let bands = ink(&frame.rects).bands;
        assert_eq!(bands.len(), 2, "two lines");
        bands[1].0 - bands[0].0
    };
    let medium = style(SubtitleSize::Medium, SubtitlePosition::Low);
    let large = style(SubtitleSize::Large, SubtitlePosition::Low);
    let pitch = pitch_of(&mut Engine::default(), &medium, &medium);
    assert!((pitch - 48).abs() <= 1, "Medium opens at the 48 px pitch, got {pitch}");
    let pitch = pitch_of(&mut Engine::default(), &large, &large);
    assert!((pitch - 60).abs() <= 1, "Large opens at the 60 px pitch, got {pitch}");
    // The picks moved on after the header was built: the header's own size still decides.
    let pitch = pitch_of(&mut Engine::default(), &medium, &large);
    assert!((pitch - 60).abs() <= 1, "a Large header keeps 60 px under Medium picks, got {pitch}");
    let mut engine = Engine::default();
    pitch_of(&mut engine, &large, &medium);
    let pitch = pitch_of(&mut engine, &medium, &large);
    assert!((pitch - 60).abs() <= 1, "and so does a restyle to it, got {pitch}");
}

/// The worker publishes the classes beside the frame, and `published_aligns` reads them back for
/// that frame only, for a Text-origin source only.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_the_worker_publishes_aligns_beside_a_text_frame_and_none_for_authored() {
    let _guard = plx_base::testlock::serial();
    let cues = [(0, 4000, "An8", "TOP"), (2000, 4000, "Default", "LOW")];
    let text = text_source(next_source_id(), 1, &no_cues_style(), &cues);
    // Ask until the worker has answered for `now`: the aligns it published for the frame it
    // returned (asked for by that frame's serial) are what `expect` names (`None`: nothing published for it).
    let until = |source: &Arc<Source>, now: i64, expect: Option<AlignSet>| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let frame = request(source.clone(), now, 1920, 1080, 1920, 1080);
            let got = frame.as_ref().and_then(|f| published_aligns(f.serial));
            if frame.is_some() && got == expect {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "worker never published {expect:?}, last {got:?}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    until(&text, 1000, Some(AlignSet { bottom: false, middle: false, top: true }));
    until(&text, 3000, Some(AlignSet { bottom: true, middle: false, top: true }));
    let authored = script("Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,x\n");
    until(&authored, 500, None);
    clear();
}

/// D12 part 1: a seek (new id, same header and fonts) and a switch between tracks that share a
/// header reuse the renderer; only a different header or font set makes a new one.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_reuse_counts_renderers_for_seek_switch_and_authored_change() {
    let mut engine = Engine::default();
    let a = track(HEADER, &[(0, 2000, RED)]);
    assert_eq!(pixel(&render(&mut engine, &a, 100), 25, 25), [255, 0, 0, 255]);
    assert_eq!(open_calls(), 1, "the first track opens a renderer");

    let seek = track(HEADER, &[(0, 2000, RED), (2000, 1000, BLUE)]);
    let frame = render(&mut engine, &seek, 100);
    assert_eq!(frame.source_id, seek.id);
    assert_eq!(pixel(&frame, 25, 25), [255, 0, 0, 255]);
    assert_eq!(open_calls(), 1, "a seek (new id, same header) opens no renderer");

    let other = track(HEADER, &[(0, 2000, BLUE)]);
    let frame = render(&mut engine, &other, 100);
    assert_eq!(frame.source_id, other.id);
    assert_eq!(pixel(&frame, 25, 25), [0; 4], "only the new track's events are shown");
    assert_eq!(pixel(&frame, 105, 105), [0, 0, 255, 255]);
    assert_eq!(open_calls(), 1, "a switch between identical headers opens no renderer");

    let styled = HEADER.replace("&H0000FF00", "&H000000FF");
    let authored = track(&styled, &[(0, 2000, RED)]);
    assert_eq!(render(&mut engine, &authored, 100).source_id, authored.id);
    assert_eq!(open_calls(), 2, "a different authored header opens a new renderer");
}

/// libass says "unchanged" when the renderer keeps showing what it showed before a reuse. The
/// frame it answers with must name the source it now answers for, or `request` never returns it.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_unchanged_picture_after_a_switch_carries_the_new_source_id() {
    // both blank at 5 s
    let mut engine = Engine::default();
    let a = track(HEADER, &[(0, 1000, RED)]);
    let b = track(HEADER, &[(0, 1000, BLUE)]);
    render(&mut engine, &a, 5000);
    let frame = render(&mut engine, &b, 5000);
    assert_eq!(frame.source_id, b.id, "blank to blank");
    assert!(frame.rects.is_empty());
    // the same picture on both
    let c = track(HEADER, &[(0, 9000, RED)]);
    let d = track(HEADER, &[(0, 9000, RED)]);
    let shown = render(&mut engine, &c, 100);
    let frame = render(&mut engine, &d, 100);
    assert_eq!(frame.source_id, d.id, "same picture");
    assert_eq!(frame.rects, shown.rects);
    assert_eq!(open_calls(), 1);
}

fn validity_at(engine: &mut Engine, source: &Arc<Source>, now_ms: i64) -> (Arc<Frame>, Validity) {
    let out = engine.render(&job(source, now_ms, 320, 180, 320, 180));
    assert_eq!(out.frame.error, None, "native fixture requires the built host library in pkg");
    (out.frame, out.validity)
}

/// The window must agree with what libass really draws, for the odd stamps `string2timecode`
/// reads its own way (a fraction is centiseconds; no fraction is 0).
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_validity_boundaries_are_where_libass_draws() {
    let line = |start: &str, end: &str| {
        format!("Dialogue: 0,{start},{end},Default,,0,0,0,,{{\\an7\\pos(20,20)\\p1}}m 0 0 l 40 0 40 40 0 40\n")
    };
    // 1 s + 5 cs .. 2 s + 50 cs; the second line has no fractions at all: libass reads 0..0
    let source = script(&format!("{}{}", line("0:00:01.5", "0:00:02.50"), line("0:00:03", "0:00:05")));
    let mut engine = Engine::default();
    let (frame, v) = validity_at(&mut engine, &source, 500);
    assert!(frame.rects.is_empty());
    assert_eq!((v.from_ms, v.until_ms), (i64::MIN, 1050));
    // rendering exactly at each computed boundary shows the change
    assert!(validity_at(&mut engine, &source, 1049).0.rects.is_empty());
    let (frame, v) = validity_at(&mut engine, &source, 1050);
    assert_eq!(pixel(&frame, 25, 25), [0, 255, 0, 255], "visible from 1050 ms");
    assert_eq!((v.from_ms, v.until_ms), (1050, 2500));
    assert_eq!(pixel(&validity_at(&mut engine, &source, 2499).0, 25, 25)[3], 255);
    let (frame, v) = validity_at(&mut engine, &source, 2500);
    assert!(frame.rects.is_empty(), "gone at 2500 ms");
    assert_eq!((v.from_ms, v.until_ms), (2500, i64::MAX));
    assert!(validity_at(&mut engine, &source, 4000).0.rects.is_empty(), "the stamp-less line never shows");
}

#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_karaoke_and_motion_are_animated_only_while_active() {
    let source = script(concat!(
        "Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\an7\\pos(20,100)}{\\kf100}AAAA\n",
        "Dialogue: 0,0:00:03.00,0:00:04.00,Default,,0,0,0,,{\\an7\\pos(20,20)\\p1}m 0 0 l 4 0 4 4 0 4\n",
    ));
    let mut engine = Engine::default();
    assert!(validity_at(&mut engine, &source, 500).1.animated);
    let (frame, v) = validity_at(&mut engine, &source, 3500);
    assert!(!v.animated && !frame.rects.is_empty());
}

#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_script_window_clamps_validity_and_non_windowed_scripts_have_timings() {
    // 30 000 lines a second apart, with a two-minute hole after the 15 000th
    let t = |ms: i64| format!("{}:{:02}:{:02}.{:02}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000 / 10);
    let mut lines = String::new();
    for i in 0..30_000i64 {
        let at = i * 1000 + if i >= 15_000 { 120_000 } else { 0 };
        lines.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{{\\an7\\pos(10,10)\\p1}}m 0 0 l 50 0 50 50 0 50\n",
            t(at), t(at + 900)));
    }
    let source = script(&lines);
    let mut engine = Engine::default();
    let now = 15_060_000;
    let (frame, v) = validity_at(&mut engine, &source, now);
    assert!(frame.rects.is_empty(), "in the hole");
    let (lo, hi) = engine.script.as_ref().unwrap().window.unwrap();
    assert_eq!(lo, now - 10_000);
    assert_eq!(v.from_ms, lo, "the raw boundary is older than the window, which only reaches back to lo");
    assert_eq!(v.until_ms, 15_120_000);
    assert!(v.until_ms <= hi);

    // a script that is loaded whole keeps its timings too
    let small = script("Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\an7\\pos(10,10)\\p1}m 0 0 l 5 0 5 5 0 5\n");
    let (_, v) = validity_at(&mut engine, &small, 500);
    assert_eq!((v.from_ms, v.until_ms), (i64::MIN, 1000));
    assert!(engine.script.as_ref().unwrap().window.is_none());
}

/// A Text-origin sidecar script past the 8 MiB a script loaded whole may be (twelve events of 900 KiB:
/// a few lines, many bytes) is not refused: it is fed in windows within the bound, and each cue is drawn
/// when the playhead reaches it, including after a seek back.
#[test]
#[ignore = "requires the pinned native host libass artifact and packaged fonts"]
fn native_a_text_script_past_the_whole_script_bound_renders_through_its_windows() {
    let style = TextStyle::default();
    let mut text = text_ass::header(&style);
    let pad = "x".repeat(900 * 1024);
    for k in 0..12i64 {
        text.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{{\\an7\\pos(10,10)\\p1{pad}}}m 0 0 l 50 0 50 50 0 50\n",
            ass_time(k * 2000), ass_time(k * 2000 + 1000)));
    }
    assert!(text.len() > MAX_SCRIPT_BYTES);
    let source = Arc::new(Source {
        id: next_source_id(),
        revision: 1,
        origin: Origin::Text(style),
        content: Content::Script { bytes: text.into_bytes().into(), fonts: Arc::from([]) },
    });
    assert_eq!(validate(&source), Ok(()), "a windowed script is bounded by its source");
    let mut engine = Engine::default();
    for now in [500, 10_500, 22_500, 4_500] {
        let (frame, _) = validity_at(&mut engine, &source, now);
        assert!(!frame.rects.is_empty(), "the cue alive at {now} ms is drawn");
        let (lo, hi) = engine.script.as_ref().unwrap().window.expect("fed in windows");
        assert!((lo..hi).contains(&now));
    }
    let (frame, _) = validity_at(&mut engine, &source, 1_500);
    assert!(frame.rects.is_empty(), "and nothing between two cues");
}

mod synth;

/// A paused playhead whose subtitle offset moves both ways (2 s cues back to back: 20-22, 22-24,
/// 24-26): every new clock gets the frame for ITS cue, whatever the worker was doing (an
/// ahead render armed by the previous frame, or nothing) when the offset changed.
#[test]
fn a_changed_offset_on_a_paused_playhead_publishes_the_frame_of_the_new_clock() {
    use super::mailbox::{Mailbox, Rendered};
    let source = Arc::new(source(vec![event(0, 20_000, 2000), event(1, 22_000, 2000), event(2, 24_000, 2000)]));
    let key = request_key(&source);
    let cue_of = |now: i64| (now / 2000) * 2000;
    let validity_of = |now: i64| ass_timing::Validity { from_ms: cue_of(now), until_ms: cue_of(now) + 2000, animated: false };
    let mut mailbox = Mailbox::new();
    // The worker drains its mailbox: the frame is `error_frame` (any frame), the validity the cue's.
    let drain = |mailbox: &mut Mailbox| {
        while let Some(job) = mailbox.take() {
            let rendered = Rendered { frame: error_frame(job.key, Fault::RenderFailed), validity: validity_of(job.now_ms), aligns: None };
            mailbox.finish(&job, Some(rendered));
        }
    };
    // Paused at 23.0 s, the offset 0, then +2.5 s, then -2.5 s, then back, each one a frame step.
    for clock in [23_000, 20_500, 25_500, 20_500, 25_500, 23_000] {
        mailbox.request(key, clock, &source);
        drain(&mut mailbox);
        mailbox.request(key, clock, &source);
        let shown = mailbox.view(&key).expect("a frame");
        let published = mailbox.published.as_ref().map(|p| p.validity).expect("published");
        assert!(published.contains(clock), "clock {clock}: the frame on screen covers {published:?} ({:?})", shown.serial);
    }
}
