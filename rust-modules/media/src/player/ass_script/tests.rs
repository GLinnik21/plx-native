use super::*;

const HEADER: &str = "[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Inter,24\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";

fn stamp(ms: i64) -> String {
    format!("{}:{:02}:{:02}.{:02}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000 / 10)
}

/// `n` one-second lines, one every second, the text of each naming its index.
fn script(n: usize) -> Arc<[u8]> {
    let mut s = String::from(HEADER);
    for i in 0..n {
        let at = i as i64 * 1000;
        s.push_str(&format!("Dialogue: 0,{},{},Default,,0,0,0,,line{i}\n", stamp(at), stamp(at + 1000)));
    }
    s.into_bytes().into()
}

fn lines_of(w: &Window) -> Vec<String> {
    String::from_utf8_lossy(&w.text).lines().filter(|l| l.starts_with("Dialogue:")).map(String::from).collect()
}

fn has(w: &Window, i: usize) -> bool {
    lines_of(w).iter().any(|l| l.ends_with(&format!(",line{i}")))
}

#[test]
fn a_script_over_the_native_guard_is_fed_in_windows_around_the_playhead() {
    let index = ScriptIndex::parse(&script(30_000)).unwrap();
    assert_eq!(index.len(), 30_000);
    assert!(index.windowed());
    let w = index.window(1_000_000);
    assert!(lines_of(&w).len() <= WINDOW_EVENTS, "{} lines in a window", lines_of(&w).len());
    assert!(w.text.starts_with(HEADER.as_bytes()), "the header comes first, so libass reads the styles");
    assert!(w.covers(1_000_000));
    assert!(has(&w, 1000), "the line on screen is in the window");
    assert!(has(&w, 995), "a short step back stays inside it");
    assert!(!has(&w, 0) && !has(&w, 29_999), "and the far ends are not held");
}

#[test]
fn a_backward_seek_and_the_far_end_are_fed_again() {
    let index = ScriptIndex::parse(&script(30_000)).unwrap();
    let first = index.window(20_000_000);
    assert!(has(&first, 20_000));
    assert!(!first.covers(5_000_000), "the playhead left the window: it is fed again");
    let back = index.window(5_000_000);
    assert!(back.covers(5_000_000) && has(&back, 5000) && !has(&back, 20_000));
    let end = index.window(29_999_000);
    assert!(end.covers(29_999_500) && has(&end, 29_999));
    assert_eq!(end.hi_ms, i64::MAX, "nothing follows the last window");
    assert!(lines_of(&end).len() <= WINDOW_EVENTS);
}

#[test]
fn walking_the_whole_script_visits_every_line_in_a_window_within_the_bound() {
    let index = ScriptIndex::parse(&script(30_000)).unwrap();
    let mut seen = vec![false; 30_000];
    let (mut now, mut windows) = (0, 0);
    loop {
        let w = index.window(now);
        windows += 1;
        assert!(lines_of(&w).len() <= WINDOW_EVENTS);
        for l in lines_of(&w) {
            let i: usize = l.rsplit("line").next().unwrap().trim().parse().unwrap();
            seen[i] = true;
        }
        if w.hi_ms == i64::MAX {
            break;
        }
        now = w.hi_ms;
    }
    assert!(seen.iter().all(|s| *s), "every line was in some window");
    assert!(windows <= 5, "{windows} windows for 30 000 lines");
}

#[test]
fn a_line_that_started_long_ago_and_is_still_up_stays_in_every_later_window() {
    let mut s = String::from_utf8(script(30_000).to_vec()).unwrap();
    s.push_str("Dialogue: 0,0:00:00.00,9:00:00.00,Default,,0,0,0,,sign\n");
    let index = ScriptIndex::parse(&Arc::from(s.into_bytes())).unwrap();
    for now in [0, 15_000_000, 25_000_000] {
        let w = index.window(now);
        assert!(String::from_utf8_lossy(&w.text).contains(",sign\n"), "the sign is up at {now} ms");
    }
}

#[test]
fn a_small_script_is_loaded_whole_and_lines_stay_in_file_order() {
    let index = ScriptIndex::parse(&script(100)).unwrap();
    assert!(!index.windowed());
    let w = index.window(0);
    assert_eq!((0..100).filter(|&i| has(&w, i)).count(), 100);
    // overlapping events stack by file order: a window keeps it even when starts are out of order
    let s = format!("{HEADER}Dialogue: 0,0:00:05.00,0:00:06.00,Default,,0,0,0,,b\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,a\n");
    let w = ScriptIndex::parse(&Arc::from(s.into_bytes())).unwrap().window(0);
    let t = String::from_utf8_lossy(&w.text).into_owned();
    assert!(t.find(",b\n").unwrap() < t.find(",a\n").unwrap());
}

#[test]
fn a_line_with_unreadable_times_is_never_alive_as_for_libass() {
    // libass reads a stamp it cannot parse as 0: Start 0, End 0, Duration 0, so it never shows.
    let s = format!("{HEADER}Dialogue: 0,garbage,garbage,Default,,0,0,0,,odd\n");
    let index = ScriptIndex::parse(&Arc::from(s.into_bytes())).unwrap();
    assert!(!String::from_utf8_lossy(&index.window(123_456).text).contains(",odd\n"));
    // ... but a line whose columns are missing altogether is kept (libass decides)
    let s = format!("{HEADER}Dialogue: 0\n");
    let w = ScriptIndex::parse(&Arc::from(s.into_bytes())).unwrap().window(123_456);
    assert!(String::from_utf8_lossy(&w.text).contains("Dialogue: 0\n"));
}

#[test]
fn window_boundaries_use_libass_times() {
    // `0:00:01.5` is 1 s + 5 cs for libass, not 1.5 s: the line is alive from 1050 ms
    let s = format!("{HEADER}Dialogue: 0,0:00:01.5,0:00:02.5,Default,,0,0,0,,x\n");
    let index = ScriptIndex::parse(&Arc::from(s.into_bytes())).unwrap();
    let spans: Vec<_> = index.spans().collect();
    assert_eq!((spans[0].start_ms, spans[0].end_ms), (1050, 2050));
}

#[test]
fn non_windowed_script_has_timings() {
    let index = ScriptIndex::parse(&script(100)).unwrap();
    assert!(!index.windowed());
    let spans: Vec<_> = index.spans().collect();
    assert_eq!(spans.len(), 100);
    assert_eq!((spans[3].start_ms, spans[3].end_ms), (3000, 4000));
}

#[test]
fn spans_know_which_lines_animate() {
    let s = format!(
        "{HEADER}Dialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,{{\\kf50}}a\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,Scroll up;0;10;1,b\nDialogue: 0,0:00:02.00,0:00:03.00,Default,,0,0,0,,c\n"
    );
    let index = ScriptIndex::parse(&Arc::from(s.into_bytes())).unwrap();
    let flags: Vec<bool> = index.spans().map(|s| matches!(s.motion, Motion::Known(true))).collect();
    assert_eq!(flags, [true, true, false]);
}
