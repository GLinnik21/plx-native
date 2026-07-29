//! Tiny shared display formatters — ONE home for the duration/clock strings so the same value
//! can't render differently across screens (the "2 hr 15 min" vs "2h 15m" vs "0 hr 45 min"
//! drift this replaces).

/// Compact duration for meta lines — "2h 15m" / "45m" (Info card tags, player HUD context).
pub(crate) fn dur_short(ms: i64) -> String {
    let mins = (ms / 60_000).max(0);
    let (h, m) = (mins / 60, mins % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// Spelled-out duration — "2 hr 15 min" / "45 min" (detail hero + About "Run Time").
pub(crate) fn dur_long(ms: i64) -> String {
    let mins = (ms / 60_000).max(0);
    let (h, m) = (mins / 60, mins % 60);
    if h > 0 {
        format!("{h} hr {m} min")
    } else {
        format!("{m} min")
    }
}

/// Time-remaining for a Continue-Watching item — "8 min left" / "1 hr 2 min left" (rounded up to
/// the next whole minute, floored at 1). `remaining_ms` is duration − viewOffset.
pub(crate) fn time_left(remaining_ms: i64) -> String {
    let mins = ((remaining_ms + 59_999) / 60_000).max(1);
    let (h, m) = (mins / 60, mins % 60);
    if h > 0 {
        format!("{h} hr {m} min left")
    } else {
        format!("{m} min left")
    }
}

/// Playback clock — "1:23:45" past the hour, else "3:45" (scrubber clocks, chapter stamps).
pub(crate) fn clock(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// The episode kicker — `"S2, E3 · Laura"`. ONE formatter, because this string is drawn by the
/// transport HUD (the now-playing item), the route's pre-roll ctx line and the Up Next caption, and
/// the whole point of the last two is that they read identically to the first. It was three
/// separate literals before, with nothing keeping them in step.
pub(crate) fn episode_kicker(season: i64, index: i64, title: &str) -> String {
    format!("S{season}, E{index} \u{b7} {title}")
}

/// A review score, in the shape its own provider publishes it. PMS normalises every provider onto
/// one 0–10 scale, which is not how any of them are quoted: Rotten Tomatoes and TMDB are
/// PERCENTAGES (9.1 → "91%") and IMDb is out of ten ("7.4"). Printing the raw 9.1 beside a tomato
/// would read as a 9.1% score, so the badge's number is put back into the provider's own units here.
pub(crate) fn rating_score(art: crate::metadata::RatingArt, value: f64) -> String {
    match art {
        crate::metadata::RatingArt::Imdb => format!("{value:.1}"),
        _ => format!("{}%", (value * 10.0).round().clamp(0.0, 100.0) as i64),
    }
}
