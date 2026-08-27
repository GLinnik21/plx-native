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

/// **Seconds-scale duration** — "4.5 s" / "12 s". The two formatters above are minute-scale and
/// round a fifteen-second interval to "0m", which is a read-out saying nothing; this is for the
/// short spans the diagnostics panel reports (an Original shortfall's elapsed time, N13).
///
/// One decimal below ten seconds and none above, because the interesting distinctions are at the
/// bottom of the range — the difference between 2 s and 4.5 s of shortfall is a decision, and the
/// difference between 40 s and 41 s is not.
pub(crate) fn secs_short(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 10_000 {
        format!("{}.{} s", ms / 1_000, (ms % 1_000) / 100)
    } else {
        format!("{} s", ms / 1_000)
    }
}

#[cfg(test)]
#[test]
fn seconds_scale_durations_keep_the_distinctions_that_matter() {
    assert_eq!(secs_short(0), "0.0 s");
    assert_eq!(secs_short(4_500), "4.5 s");
    assert_eq!(secs_short(9_999), "9.9 s");
    assert_eq!(secs_short(12_000), "12 s");
    // Negative is not a duration; it reads as none rather than as a minus sign on screen.
    assert_eq!(secs_short(-1), "0.0 s");
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

/// An episode's ADDRESS inside its show — `"S2, E3"`, with no title. The detail page's hero meta
/// line draws it bare (the title band above it already IS the episode's name, so repeating it there
/// would be the one line on the page that says nothing new), and [`episode_kicker`] is this plus the
/// title. It lives here rather than as a `format!` beside its one caller because the two spellings
/// have to stay one vocabulary: a page that said "S2, E3" while the HUD said "S2E3" for the same
/// leaf is exactly the drift this module exists to prevent, and nothing but shared code enforces it.
pub(crate) fn episode_ordinal(season: i64, index: i64) -> String {
    format!("S{season}, E{index}")
}

/// The source attribution — `"Shared by friend"` — or `None` when the item is the signed-in
/// account's own.
///
/// `None` rather than an empty `String`, because ABSENCE is what the design specifies for an owned
/// item: no separator, no empty run, no draw call. Both call sites were hand-rolling that guard.
///
/// ONE formatter for the same reason [`episode_kicker`] is one: this phrase is drawn by the hero's
/// meta line, the detail page's facts row and (with a sentence after it) the Library's failure
/// read-out. It was written twice with two different empty-handle behaviours and interpolated a
/// third time inline — exactly the drift this module exists to prevent.
pub(crate) fn shared_by(handle: &str) -> Option<String> {
    (!handle.is_empty()).then(|| format!("Shared by {handle}"))
}

/// The episode kicker — `"S2, E3 · Laura"`, the [`episode_ordinal`] with the episode's title after
/// it. ONE formatter, because this string is drawn by the transport HUD (the now-playing item), the
/// route's pre-roll ctx line and the Up Next caption, and the whole point of the last two is that
/// they read identically to the first. It was three separate literals before, with nothing keeping
/// them in step.
pub(crate) fn episode_kicker(season: i64, index: i64, title: &str) -> String {
    format!("{} \u{b7} {title}", episode_ordinal(season, index))
}

/// The media badge for a video version — `"4K"` / `"1080p"` / `"SD"`, or None when the item has no
/// video (a show container, a music item, an unparsed response).
///
/// `res` is PMS's `Media.videoResolution` and is PREFERRED over the frame size, because the frame
/// size is the STORED one: a 2.35:1 1080p film is 1918x802 on the dev server, and a height rule
/// would badge it 720p. Verified values are `"4k"`, `"1080"`, `"720"`, `"576"`, `"sd"` — and PMS
/// sends every one of them as a numeric-looking STRING, which is exactly the shape this maps.
/// `width`/`height` are only the fallback for a version that omits the class.
pub(crate) fn resolution(res: &str, width: i64, height: i64) -> Option<String> {
    let r = res.trim().to_ascii_lowercase();
    if !r.is_empty() {
        // "4k"/"8k" are already the badge; a bare number is a scan-line count ("1080" → "1080p");
        // anything else (e.g. "sd") is a class name and reads as itself, upper-cased.
        return Some(match r.as_str() {
            _ if r.chars().all(|c| c.is_ascii_digit()) => format!("{r}p"),
            _ => r.to_ascii_uppercase(),
        });
    }
    // No class: fall back to the frame WIDTH, which (unlike height) is constant across aspect
    // ratios. Thresholds sit below each nominal width (3840/2560/1920/1280/1024) so a cropped or
    // anamorphic frame still lands in its own class, and the buckets are the SAME vocabulary the
    // class path emits — the two paths must not disagree about the same file.
    // saturating: these are wire values via the lenient `de_i64`, so a garbage 19-digit height
    // must not overflow (a debug-build panic, a silently wrapped badge in release)
    let w = if width > 0 { width } else { height.saturating_mul(16) / 9 };
    Some(match w {
        w if w <= 0 => return None,
        w if w >= 3000 => "4K".to_string(),
        w if w >= 2400 => "1440p".to_string(),
        w if w >= 1700 => "1080p".to_string(),
        w if w >= 1100 => "720p".to_string(),
        w if w >= 900 => "576p".to_string(),
        _ => "SD".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::resolution;

    /// The mapping is fed PMS's own values, and PMS string-encodes them — including the ones that
    /// look like integers ("1080"), which is the case that must come out as "1080p" and not as a
    /// number or an empty badge.
    #[test]
    fn resolution_labels_pms_string_encoded_classes() {
        // verified live on the dev server: every videoResolution is a JSON string
        assert_eq!(resolution("4k", 3840, 2160).as_deref(), Some("4K"));
        assert_eq!(resolution("1080", 1920, 1080).as_deref(), Some("1080p"));
        assert_eq!(resolution("720", 1280, 720).as_deref(), Some("720p"));
        assert_eq!(resolution("576", 1024, 576).as_deref(), Some("576p"));
        assert_eq!(resolution("sd", 826, 452).as_deref(), Some("SD"));
        // whitespace/casing from the wire must not produce a second spelling of a badge
        assert_eq!(resolution(" 4K ", 0, 0).as_deref(), Some("4K"));
        // the class WINS over the frame size: a 2.35:1 1080p film is 1918x802 (a height rule
        // would call it 720p, a width rule 1080p — either way the server's own answer decides)
        assert_eq!(resolution("1080", 1918, 802).as_deref(), Some("1080p"));
    }

    #[test]
    fn resolution_falls_back_to_frame_width() {
        assert_eq!(resolution("", 3840, 2160).as_deref(), Some("4K"));
        assert_eq!(resolution("", 1918, 802).as_deref(), Some("1080p")); // scope 1080p, by WIDTH
        assert_eq!(resolution("", 1280, 720).as_deref(), Some("720p"));
        assert_eq!(resolution("", 720, 480).as_deref(), Some("SD"));
        // the fallback speaks the class path's vocabulary: the same file must not badge "576p"
        // with a videoResolution and "SD" without one
        assert_eq!(resolution("", 1024, 576).as_deref(), resolution("576", 1024, 576).as_deref());
        assert_eq!(resolution("", 0, 1080).as_deref(), Some("1080p")); // width absent → from height
        assert_eq!(resolution("", 0, 0), None); // no video at all (a show container) → no badge
    }
}

/// A calendar date from a Plex `YYYY-MM-DD` — `"1 Feb 1921"`. ONE spelling, because the same value
/// is now drawn by two screens: detail's episode air dates and the person page's Born/Died line.
/// `year` is the fallback when the item carries only a year (an episode with no `originallyAvailableAt`);
/// pass 0 when there is none, and an unparseable date yields the empty string rather than a
/// half-formatted one — every caller draws a date line only when it is non-empty.
pub(crate) fn pretty_date(iso: &str, year: i64) -> String {
    let parts: Vec<&str> = iso.split('-').collect();
    if parts.len() == 3 {
        if let (Ok(y), Ok(mo), Ok(da)) =
            (parts[0].parse::<i64>(), parts[1].parse::<usize>(), parts[2].parse::<i64>())
        {
            const MON: [&str; 12] =
                ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            if (1..=12).contains(&mo) {
                return format!("{da} {} {y}", MON[mo - 1]);
            }
        }
    }
    if year > 0 {
        year.to_string()
    } else {
        String::new()
    }
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

/// The unit that trails a score, set a rung down in tertiary ink — see `widgets::RatingCell`.
///
/// Only IMDb has one. A percentage carries its `%` inside [`rating_score`] because there the sign
/// is part of the number, whereas "/10" is a note about the SCALE: it is what stops an 8.1 being
/// read against the 69% beside it, and it is the reason the two can share a row at all.
pub(crate) fn rating_suffix(art: crate::metadata::RatingArt) -> &'static str {
    match art {
        crate::metadata::RatingArt::Imdb => "/10",
        _ => "",
    }
}
