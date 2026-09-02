//! Strict parser and refresh state for the fixed PMS HLS shape.
//!
//! This deliberately is not a general HLS implementation. It accepts one master variant and
//! growing MPEG-TS media playlists, and rejects every feature that would change how bytes must be
//! assembled before they reach the demuxer (encryption, byte ranges, fMP4 maps and
//! discontinuities). URI resolution is also part of the parser: every child stays on the source
//! PMS origin, so a playlist cannot turn the media worker into an arbitrary URL fetcher.
use crate::abr::MediaTimeMs;
use crate::plex::{origin, Origin};
use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Resource {
    pub(crate) origin: Origin,
    /// Absolute path plus optional query. Fragments are never accepted.
    pub(crate) path: String,
}

/// Authentication captured from the route-owned master URL and applied only at the transport
/// boundary. HLS URI resolution does not inherit a query, but PMS requires the Plex token on the
/// master, child manifest and every segment request. Keeping it out of [`Resource`] means parsed
/// playlist values and their `Debug` output can never become a credential store.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct InheritedAuth {
    token_pair: String,
}

fn is_token_pair(pair: &str) -> bool {
    pair.split_once('=')
        .is_some_and(|(name, _)| name.eq_ignore_ascii_case("X-Plex-Token"))
}

impl fmt::Debug for InheritedAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InheritedAuth(<redacted>)")
    }
}

impl InheritedAuth {
    pub(crate) fn capture(master: &Resource) -> Result<Self, Error> {
        let query = master
            .path
            .split_once('?')
            .map(|(_, query)| query)
            .ok_or(Error::MissingCredential)?;
        let mut found = query.split('&').filter(|pair| is_token_pair(pair));
        let token_pair = found
            .next()
            .filter(|pair| pair.len() > "X-Plex-Token=".len())
            .ok_or(Error::MissingCredential)?;
        if found.next().is_some() {
            return Err(Error::MultipleCredentials);
        }
        Ok(Self {
            token_pair: token_pair.to_owned(),
        })
    }

    /// A request path for the same-origin transport. A playlist may repeat the route credential,
    /// but it may not replace it with another value.
    pub(crate) fn request_path(&self, resource: &Resource) -> Result<String, Error> {
        if let Some((_, query)) = resource.path.split_once('?') {
            let mut found = query.split('&').filter(|pair| is_token_pair(pair));
            if let Some(pair) = found.next() {
                if pair != self.token_pair {
                    return Err(Error::CredentialChanged);
                }
                if found.next().is_some() {
                    return Err(Error::MultipleCredentials);
                }
                return Ok(resource.path.clone());
            }
        }
        let separator = if resource.path.contains('?') {
            '&'
        } else {
            '?'
        };
        Ok(format!("{}{separator}{}", resource.path, self.token_pair))
    }
}

/// Converts every fresh MPEG-TS demux context's local timestamps into one integer-millisecond
/// content timeline. Segment boundaries advance by EXTINF duration, not by the last packet seen;
/// that prevents encoder lead-in and time-base resets from accumulating between contexts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SegmentTimeline {
    next_start_ns: i64,
}

impl SegmentTimeline {
    pub(crate) fn begin(self, duration: Duration) -> Result<SegmentClock, Error> {
        let duration_ns =
            i64::try_from(duration.as_nanos()).map_err(|_| Error::DurationOverflow)?;
        let next_start_ns = self
            .next_start_ns
            .checked_add(duration_ns)
            .ok_or(Error::DurationOverflow)?;
        Ok(SegmentClock {
            base_ns: self.next_start_ns,
            next_start_ns,
            video_origin_ns: None,
            audio_origin_ns: None,
        })
    }

    pub(crate) fn commit(&mut self, clock: SegmentClock) {
        self.next_start_ns = clock.next_start_ns;
    }

    #[cfg(test)]
    pub(crate) fn end(&self) -> MediaTimeMs {
        MediaTimeMs(self.next_start_ns / 1_000_000)
    }

    /// Exact committed boundary for a replacement encoder request. Display telemetry is in
    /// milliseconds, but collapsing a 2.002 s HLS boundary to whole seconds repeats media.
    pub(crate) fn end_ns(&self) -> i64 {
        self.next_start_ns
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SegmentClock {
    base_ns: i64,
    next_start_ns: i64,
    video_origin_ns: Option<i64>,
    audio_origin_ns: Option<i64>,
}

impl SegmentClock {
    pub(crate) fn end(self) -> MediaTimeMs {
        MediaTimeMs(self.next_start_ns / 1_000_000)
    }

    fn normalize_from(base_ns: i64, origin: &mut Option<i64>, raw_ns: i64) -> MediaTimeMs {
        let origin = *origin.get_or_insert(raw_ns);
        let relative = raw_ns.saturating_sub(origin).max(0);
        MediaTimeMs(base_ns.saturating_add(relative) / 1_000_000)
    }

    /// Normalize video and audio independently to the segment boundary. MPEG-TS packets from the
    /// two streams may be physically interleaved in either order, so anchoring both lanes to the
    /// first packet observed makes the later-discovered lane inherit an arbitrary packet-order
    /// offset. The controller consumes only this content timeline, never raw FFmpeg PTS.
    pub(crate) fn normalize_video(&mut self, raw_ns: i64) -> MediaTimeMs {
        Self::normalize_from(self.base_ns, &mut self.video_origin_ns, raw_ns)
    }

    pub(crate) fn normalize_audio(&mut self, raw_ns: i64) -> MediaTimeMs {
        Self::normalize_from(self.base_ns, &mut self.audio_origin_ns, raw_ns)
    }
}

impl fmt::Debug for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The initial master URL normally carries X-Plex-Token in its query. Parsed values are
        // useful in assertions and diagnostics, but Debug must not become a second credential
        // logging surface.
        let path = match self.path.split_once('?') {
            Some((path, _)) => format!("{path}?<query>"),
            None => self.path.clone(),
        };
        f.debug_struct("Resource")
            .field("origin", &self.origin)
            .field("path", &path)
            .finish()
    }
}

impl Resource {
    pub(crate) fn new(origin: Origin, path: impl Into<String>) -> Result<Self, Error> {
        let path = normalize_absolute_path(&path.into())?;
        Ok(Self { origin, path })
    }

    /// Resolve one playlist URI. Relative and root-relative references inherit the base origin;
    /// an absolute HTTP(S) reference is accepted only when its parsed origin is exactly equal.
    pub(crate) fn resolve(&self, reference: &str) -> Result<Self, Error> {
        validate_uri_text(reference)?;
        if reference.is_empty() || reference.starts_with('?') {
            return Err(Error::InvalidUri("URI has no path"));
        }
        if reference.starts_with("//") {
            return Err(Error::CrossOrigin);
        }

        if has_scheme(reference) {
            if !(reference.starts_with("http://") || reference.starts_with("https://")) {
                return Err(Error::InvalidUri("unsupported URI scheme"));
            }
            let child_origin =
                Origin::parse(reference).ok_or(Error::InvalidUri("malformed absolute URI"))?;
            if child_origin != self.origin {
                return Err(Error::CrossOrigin);
            }
            let (_, path) = origin::split(reference);
            return Resource::new(child_origin, if path.is_empty() { "/" } else { path });
        }

        if reference.starts_with('/') {
            return Resource::new(self.origin.clone(), reference);
        }

        let base_path = self
            .path
            .split_once('?')
            .map_or(self.path.as_str(), |(path, _)| path);
        let slash = base_path
            .rfind('/')
            .ok_or(Error::InvalidUri("base URI has no directory"))?;
        Resource::new(
            self.origin.clone(),
            format!("{}{}", &base_path[..=slash], reference),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MasterPlaylist {
    pub(crate) source: Resource,
    pub(crate) version: Option<u32>,
    pub(crate) variant: Variant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Variant {
    pub(crate) resource: Resource,
    pub(crate) bandwidth: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaylistType {
    Event,
    Vod,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MediaPlaylist {
    pub(crate) source: Resource,
    pub(crate) version: Option<u32>,
    pub(crate) target_duration_secs: u64,
    /// PMS repeats the requested content offset in EXT-X-START. This is validated and retained
    /// as playlist metadata, but it is not added to the demux clock: the route's display base
    /// already places segment-local timestamps on the content timeline.
    pub(crate) start_offset_micros: Option<i64>,
    pub(crate) media_sequence: u64,
    pub(crate) playlist_type: Option<PlaylistType>,
    pub(crate) segments: Vec<Segment>,
    pub(crate) end_list: bool,
}

impl MediaPlaylist {
    pub(crate) fn total_duration(&self) -> Result<Duration, Error> {
        self.segments
            .iter()
            .try_fold(Duration::ZERO, |total, segment| {
                total
                    .checked_add(segment.duration)
                    .ok_or(Error::DurationOverflow)
            })
    }

    /// Segment containing EXT-X-START's preferred content time. PMS emits the whole VOD list but
    /// produces only the suffix at/after this point for an offset encoder; requesting segment zero
    /// from such a session returns 404 forever. Positive offsets count from the head, negative
    /// offsets from the tail, per the HLS tag's wire semantics.
    pub(crate) fn preferred_start_index(&self) -> Result<usize, Error> {
        let Some(offset_micros) = self.start_offset_micros else {
            return Ok(0);
        };
        let total_ns = self.total_duration()?.as_nanos();
        let magnitude_ns = u128::from(offset_micros.unsigned_abs()).saturating_mul(1_000);
        let target_ns = if offset_micros < 0 {
            total_ns.saturating_sub(magnitude_ns)
        } else {
            magnitude_ns.min(total_ns)
        };
        let mut elapsed_ns = 0u128;
        for (index, segment) in self.segments.iter().enumerate() {
            elapsed_ns = elapsed_ns
                .checked_add(segment.duration.as_nanos())
                .ok_or(Error::DurationOverflow)?;
            if elapsed_ns > target_ns {
                return Ok(index);
            }
        }
        Ok(self.segments.len())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    pub(crate) sequence: u64,
    pub(crate) duration: Duration,
    pub(crate) resource: Resource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refresh {
    /// Segments not returned by any successful earlier refresh, in sequence order.
    pub(crate) new_segments: Vec<Segment>,
    /// Sticky: once a valid ENDLIST is observed, a later stale response cannot clear it.
    pub(crate) end_list: bool,
}

#[derive(Debug, Default)]
pub(crate) struct MediaTracker {
    source: Option<Resource>,
    next_sequence: Option<u64>,
    seen: BTreeMap<u64, Segment>,
    ended: bool,
}

impl MediaTracker {
    pub(crate) fn apply(&mut self, playlist: &MediaPlaylist) -> Result<Refresh, Error> {
        if self
            .source
            .as_ref()
            .is_some_and(|source| source != &playlist.source)
        {
            return Err(Error::RefreshSourceChanged);
        }

        let mut expected = self.next_sequence.unwrap_or(playlist.media_sequence);
        if playlist.media_sequence > expected {
            return Err(Error::SequenceGap {
                expected,
                found: playlist.media_sequence,
            });
        }

        // Validate against local state first. No failing response is allowed to partially advance
        // the cursor or poison the replay comparison used by a later valid response.
        let mut additions = Vec::new();
        for segment in &playlist.segments {
            if segment.sequence < expected {
                match self.seen.get(&segment.sequence) {
                    Some(previous) if previous == segment => continue,
                    Some(_) => return Err(Error::SegmentChanged(segment.sequence)),
                    None => return Err(Error::UnknownOldSegment(segment.sequence)),
                }
            }
            if segment.sequence > expected {
                return Err(Error::SequenceGap {
                    expected,
                    found: segment.sequence,
                });
            }
            if self.ended {
                return Err(Error::SegmentAfterEndList(segment.sequence));
            }
            additions.push(segment.clone());
            expected = expected.checked_add(1).ok_or(Error::SequenceOverflow)?;
        }

        let playlist_next = playlist
            .media_sequence
            .checked_add(playlist.segments.len() as u64)
            .ok_or(Error::SequenceOverflow)?;
        if playlist.end_list && playlist_next < expected {
            return Err(Error::StaleEndList);
        }

        for segment in &additions {
            self.seen.insert(segment.sequence, segment.clone());
        }
        self.source.get_or_insert_with(|| playlist.source.clone());
        self.next_sequence = Some(expected);
        self.ended |= playlist.end_list;
        Ok(Refresh {
            new_segments: additions,
            end_list: self.ended,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    MissingHeader,
    Malformed { line: usize, reason: &'static str },
    InvalidUri(&'static str),
    CrossOrigin,
    UnsupportedTag(String),
    UnsupportedFeature(&'static str),
    MissingVariant,
    MultipleVariants,
    MissingTargetDuration,
    MissingMediaSequence,
    ChildIsNotPlaylist,
    SegmentIsNotMpegTs,
    SequenceOverflow,
    RefreshSourceChanged,
    SequenceGap { expected: u64, found: u64 },
    SegmentChanged(u64),
    UnknownOldSegment(u64),
    SegmentAfterEndList(u64),
    StaleEndList,
    MissingCredential,
    MultipleCredentials,
    CredentialChanged,
    DurationOverflow,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHeader => f.write_str("playlist does not start with #EXTM3U"),
            Self::Malformed { line, reason } => {
                write!(f, "malformed playlist at line {line}: {reason}")
            }
            Self::InvalidUri(reason) => write!(f, "invalid playlist URI: {reason}"),
            Self::CrossOrigin => f.write_str("playlist URI changes origin"),
            Self::UnsupportedTag(tag) => write!(f, "unsupported HLS tag {tag}"),
            Self::UnsupportedFeature(feature) => write!(f, "unsupported HLS feature: {feature}"),
            Self::MissingVariant => f.write_str("master playlist has no variant"),
            Self::MultipleVariants => f.write_str("master playlist has more than one variant"),
            Self::MissingTargetDuration => f.write_str("media playlist has no target duration"),
            Self::MissingMediaSequence => f.write_str("media playlist has no media sequence"),
            Self::ChildIsNotPlaylist => f.write_str("master child is not an m3u8 playlist"),
            Self::SegmentIsNotMpegTs => f.write_str("media segment is not MPEG-TS"),
            Self::SequenceOverflow => f.write_str("media sequence overflows u64"),
            Self::RefreshSourceChanged => f.write_str("refresh belongs to another media playlist"),
            Self::SequenceGap { expected, found } => {
                write!(f, "media sequence gap: expected {expected}, found {found}")
            }
            Self::SegmentChanged(sequence) => {
                write!(f, "segment {sequence} changed across refreshes")
            }
            Self::UnknownOldSegment(sequence) => {
                write!(f, "refresh introduced unseen old segment {sequence}")
            }
            Self::SegmentAfterEndList(sequence) => {
                write!(f, "segment {sequence} appeared after ENDLIST")
            }
            Self::StaleEndList => f.write_str("ENDLIST came from a playlist older than the cursor"),
            Self::MissingCredential => f.write_str("HLS master URL has no Plex credential"),
            Self::MultipleCredentials => {
                f.write_str("HLS master URL has multiple Plex credentials")
            }
            Self::CredentialChanged => f.write_str("playlist URI replaces the route credential"),
            Self::DurationOverflow => f.write_str("HLS timeline duration overflows"),
        }
    }
}

impl std::error::Error for Error {}

pub(crate) fn parse_master(source: &Resource, text: &str) -> Result<MasterPlaylist, Error> {
    let lines = playlist_lines(text)?;
    let mut version = None;
    let mut independent_segments = false;
    let mut pending_bandwidth = None;
    let mut variant = None;

    for (line_no, line) in lines.into_iter().skip(1) {
        if line.is_empty() {
            if pending_bandwidth.is_some() {
                return malformed(
                    line_no,
                    "variant URI must immediately follow EXT-X-STREAM-INF",
                );
            }
            continue;
        }

        if line.starts_with('#') {
            if pending_bandwidth.is_some() {
                return malformed(
                    line_no,
                    "variant URI must immediately follow EXT-X-STREAM-INF",
                );
            }
            if let Some(value) = line.strip_prefix("#EXT-X-STREAM-INF:") {
                if variant.is_some() {
                    return Err(Error::MultipleVariants);
                }
                pending_bandwidth = Some(parse_variant_bandwidth(value, line_no)?);
            } else if let Some(value) = line.strip_prefix("#EXT-X-VERSION:") {
                set_once(
                    &mut version,
                    parse_positive_u32(value, line_no)?,
                    line_no,
                    "duplicate version",
                )?;
            } else if line == "#EXT-X-INDEPENDENT-SEGMENTS" {
                if std::mem::replace(&mut independent_segments, true) {
                    return malformed(line_no, "duplicate independent-segments tag");
                }
            } else if let Some(value) = line.strip_prefix("#EXT-X-ALLOW-CACHE:") {
                if !matches!(value, "YES" | "NO") {
                    return malformed(line_no, "invalid allow-cache value");
                }
            } else if line == "#EXTM3U" {
                return malformed(line_no, "duplicate header");
            } else if let Some(feature) = forbidden_feature(line) {
                return Err(Error::UnsupportedFeature(feature));
            } else if line.starts_with("#EXT") {
                return Err(Error::UnsupportedTag(tag_name(line).to_owned()));
            }
            continue;
        }

        let bandwidth = pending_bandwidth.take().ok_or(Error::Malformed {
            line: line_no,
            reason: "URI has no EXT-X-STREAM-INF",
        })?;
        if variant.is_some() {
            return Err(Error::MultipleVariants);
        }
        let resource = source.resolve(line)?;
        if !path_without_query(&resource.path).ends_with(".m3u8") {
            return Err(Error::ChildIsNotPlaylist);
        }
        variant = Some(Variant {
            resource,
            bandwidth,
        });
    }

    if pending_bandwidth.is_some() {
        return Err(Error::Malformed {
            line: 0,
            reason: "EXT-X-STREAM-INF has no URI",
        });
    }
    Ok(MasterPlaylist {
        source: source.clone(),
        version,
        variant: variant.ok_or(Error::MissingVariant)?,
    })
}

pub(crate) fn parse_media(source: &Resource, text: &str) -> Result<MediaPlaylist, Error> {
    let lines = playlist_lines(text)?;
    let mut version = None;
    let mut target_duration = None;
    let mut start_offset_micros = None;
    let mut media_sequence = None;
    let mut playlist_type = None;
    let mut independent_segments = false;
    let mut pending_duration = None;
    let mut pending_program_time = false;
    let mut segments = Vec::new();
    let mut end_list = false;

    for (line_no, line) in lines.into_iter().skip(1) {
        if line.is_empty() {
            if pending_duration.is_some() {
                return malformed(line_no, "segment URI must immediately follow EXTINF");
            }
            continue;
        }

        if end_list {
            if line.starts_with('#') && !line.starts_with("#EXT") {
                continue;
            }
            return malformed(line_no, "content follows EXT-X-ENDLIST");
        }

        if line.starts_with('#') {
            if pending_duration.is_some() {
                return malformed(line_no, "segment URI must immediately follow EXTINF");
            }
            if let Some(value) = line.strip_prefix("#EXT-X-VERSION:") {
                set_once(
                    &mut version,
                    parse_positive_u32(value, line_no)?,
                    line_no,
                    "duplicate version",
                )?;
            } else if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
                set_once(
                    &mut target_duration,
                    parse_positive_u64(value, line_no, "invalid target duration")?,
                    line_no,
                    "duplicate target duration",
                )?;
            } else if let Some(value) = line.strip_prefix("#EXT-X-START:") {
                let value = value.strip_prefix("TIME-OFFSET=").ok_or(Error::Malformed {
                    line: line_no,
                    reason: "invalid start attributes",
                })?;
                set_once(
                    &mut start_offset_micros,
                    parse_signed_decimal_micros(value, line_no)?,
                    line_no,
                    "duplicate start tag",
                )?;
            } else if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
                set_once(
                    &mut media_sequence,
                    parse_u64(value, line_no, "invalid media sequence")?,
                    line_no,
                    "duplicate media sequence",
                )?;
            } else if let Some(value) = line.strip_prefix("#EXT-X-PLAYLIST-TYPE:") {
                let value = match value {
                    "EVENT" => PlaylistType::Event,
                    "VOD" => PlaylistType::Vod,
                    _ => return malformed(line_no, "invalid playlist type"),
                };
                set_once(
                    &mut playlist_type,
                    value,
                    line_no,
                    "duplicate playlist type",
                )?;
            } else if let Some(value) = line.strip_prefix("#EXT-X-PROGRAM-DATE-TIME:") {
                if value.is_empty()
                    || value
                        .bytes()
                        .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
                {
                    return malformed(line_no, "invalid program date-time");
                }
                if std::mem::replace(&mut pending_program_time, true) {
                    return malformed(line_no, "two program date-times precede one segment");
                }
            } else if let Some(value) = line.strip_prefix("#EXTINF:") {
                if target_duration.is_none() || media_sequence.is_none() {
                    return malformed(
                        line_no,
                        "target duration and media sequence must precede segments",
                    );
                }
                pending_duration = Some(parse_extinf(value, line_no)?);
            } else if line == "#EXT-X-INDEPENDENT-SEGMENTS" {
                if std::mem::replace(&mut independent_segments, true) {
                    return malformed(line_no, "duplicate independent-segments tag");
                }
            } else if let Some(value) = line.strip_prefix("#EXT-X-ALLOW-CACHE:") {
                if !matches!(value, "YES" | "NO") {
                    return malformed(line_no, "invalid allow-cache value");
                }
            } else if line == "#EXT-X-ENDLIST" {
                if pending_program_time {
                    return malformed(line_no, "program date-time has no segment");
                }
                end_list = true;
            } else if line == "#EXTM3U" {
                return malformed(line_no, "duplicate header");
            } else if let Some(feature) = forbidden_feature(line) {
                return Err(Error::UnsupportedFeature(feature));
            } else if line.starts_with("#EXT") {
                return Err(Error::UnsupportedTag(tag_name(line).to_owned()));
            }
            continue;
        }

        let duration = pending_duration.take().ok_or(Error::Malformed {
            line: line_no,
            reason: "segment URI has no EXTINF",
        })?;
        let sequence = media_sequence
            .expect("EXTINF gate established the media sequence")
            .checked_add(segments.len() as u64)
            .ok_or(Error::SequenceOverflow)?;
        let resource = source.resolve(line)?;
        if !path_without_query(&resource.path)
            .to_ascii_lowercase()
            .ends_with(".ts")
        {
            return Err(Error::SegmentIsNotMpegTs);
        }
        segments.push(Segment {
            sequence,
            duration,
            resource,
        });
        pending_program_time = false;
    }

    if pending_duration.is_some() {
        return Err(Error::Malformed {
            line: 0,
            reason: "EXTINF has no segment URI",
        });
    }
    if pending_program_time {
        return Err(Error::Malformed {
            line: 0,
            reason: "program date-time has no segment",
        });
    }
    let target_duration_secs = target_duration.ok_or(Error::MissingTargetDuration)?;
    let media_sequence = media_sequence.ok_or(Error::MissingMediaSequence)?;
    media_sequence
        .checked_add(segments.len() as u64)
        .ok_or(Error::SequenceOverflow)?;

    Ok(MediaPlaylist {
        source: source.clone(),
        version,
        target_duration_secs,
        start_offset_micros,
        media_sequence,
        playlist_type,
        segments,
        end_list,
    })
}

fn playlist_lines(text: &str) -> Result<Vec<(usize, &str)>, Error> {
    let mut lines = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        let line_no = index + 1;
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.contains('\r') || line.contains('\0') {
            return malformed(line_no, "control character in line");
        }
        if line != line.trim() {
            return malformed(line_no, "leading or trailing whitespace");
        }
        lines.push((line_no, line));
    }
    if lines.first().map(|(_, line)| *line) != Some("#EXTM3U") {
        return Err(Error::MissingHeader);
    }
    Ok(lines)
}

fn parse_variant_bandwidth(value: &str, line: usize) -> Result<u64, Error> {
    if value.is_empty() {
        return malformed(line, "empty variant attribute list");
    }
    let mut start = 0;
    let mut quoted = false;
    let mut items = Vec::new();
    for (index, byte) in value.bytes().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b',' if !quoted => {
                items.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    if quoted {
        return malformed(line, "unterminated quoted variant attribute");
    }
    items.push(&value[start..]);

    let mut keys = Vec::new();
    let mut bandwidth = None;
    for item in items {
        let (key, raw_value) = item.split_once('=').ok_or(Error::Malformed {
            line,
            reason: "variant attribute has no value",
        })?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
            || raw_value.is_empty()
            || keys.contains(&key)
        {
            return malformed(line, "invalid or duplicate variant attribute");
        }
        let quoted_value = raw_value.starts_with('"');
        if quoted_value {
            if raw_value.len() < 2
                || !raw_value.ends_with('"')
                || raw_value[1..raw_value.len() - 1].contains('"')
            {
                return malformed(line, "malformed quoted variant attribute");
            }
        } else if raw_value.contains('"') || raw_value.bytes().any(|b| b.is_ascii_whitespace()) {
            return malformed(line, "malformed variant attribute value");
        }
        keys.push(key);
        if key == "BANDWIDTH" {
            if quoted_value {
                return malformed(line, "bandwidth must be an integer");
            }
            bandwidth = Some(parse_positive_u64(raw_value, line, "invalid bandwidth")?);
        }
    }
    bandwidth.ok_or(Error::Malformed {
        line,
        reason: "variant has no bandwidth",
    })
}

fn parse_extinf(value: &str, line: usize) -> Result<Duration, Error> {
    let (number, _) = value.split_once(',').ok_or(Error::Malformed {
        line,
        reason: "EXTINF has no comma",
    })?;
    let (seconds, fraction) = match number.split_once('.') {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (number, None),
    };
    if seconds.is_empty() || !seconds.bytes().all(|b| b.is_ascii_digit()) {
        return malformed(line, "invalid EXTINF duration");
    }
    let seconds = seconds.parse::<u64>().map_err(|_| Error::Malformed {
        line,
        reason: "EXTINF duration overflows",
    })?;
    let nanos = if let Some(fraction) = fraction {
        if fraction.is_empty()
            || fraction.len() > 9
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return malformed(line, "invalid EXTINF fraction");
        }
        let fraction = fraction.parse::<u32>().map_err(|_| Error::Malformed {
            line,
            reason: "invalid EXTINF fraction",
        })?;
        fraction
            * 10_u32.pow(9 - number.split_once('.').expect("fraction is present").1.len() as u32)
    } else {
        0
    };
    if seconds == 0 && nanos == 0 {
        return malformed(line, "zero EXTINF duration");
    }
    Ok(Duration::new(seconds, nanos))
}

fn parse_signed_decimal_micros(value: &str, line: usize) -> Result<i64, Error> {
    let (negative, value) = match value.strip_prefix('-') {
        Some(value) => (true, value),
        None => (false, value),
    };
    let (seconds, fraction) = match value.split_once('.') {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (value, None),
    };
    if seconds.is_empty() || !seconds.bytes().all(|byte| byte.is_ascii_digit()) {
        return malformed(line, "invalid start time offset");
    }
    let seconds = seconds.parse::<i64>().map_err(|_| Error::Malformed {
        line,
        reason: "start time offset overflows",
    })?;
    let mut micros = seconds.checked_mul(1_000_000).ok_or(Error::Malformed {
        line,
        reason: "start time offset overflows",
    })?;
    if let Some(fraction) = fraction {
        if fraction.is_empty()
            || fraction.len() > 6
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return malformed(line, "invalid start time offset");
        }
        let fraction = fraction.parse::<i64>().map_err(|_| Error::Malformed {
            line,
            reason: "invalid start time offset",
        })?;
        micros = micros
            .checked_add(
                fraction
                    * 10_i64.pow(
                        6 - value.split_once('.').expect("fraction is present").1.len() as u32,
                    ),
            )
            .ok_or(Error::Malformed {
                line,
                reason: "start time offset overflows",
            })?;
    }
    if negative {
        micros.checked_neg().ok_or(Error::Malformed {
            line,
            reason: "start time offset overflows",
        })
    } else {
        Ok(micros)
    }
}

fn parse_positive_u32(value: &str, line: usize) -> Result<u32, Error> {
    let value = value.parse::<u32>().map_err(|_| Error::Malformed {
        line,
        reason: "invalid version",
    })?;
    if value == 0 {
        return malformed(line, "version must be positive");
    }
    Ok(value)
}

fn parse_u64(value: &str, line: usize, reason: &'static str) -> Result<u64, Error> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return malformed(line, reason);
    }
    value
        .parse::<u64>()
        .map_err(|_| Error::Malformed { line, reason })
}

fn parse_positive_u64(value: &str, line: usize, reason: &'static str) -> Result<u64, Error> {
    let value = parse_u64(value, line, reason)?;
    if value == 0 {
        return malformed(line, reason);
    }
    Ok(value)
}

fn set_once<T>(
    slot: &mut Option<T>,
    value: T,
    line: usize,
    reason: &'static str,
) -> Result<(), Error> {
    if slot.is_some() {
        return malformed(line, reason);
    }
    *slot = Some(value);
    Ok(())
}

fn forbidden_feature(line: &str) -> Option<&'static str> {
    match tag_name(line) {
        "#EXT-X-KEY" | "#EXT-X-SESSION-KEY" => Some("encrypted segments"),
        "#EXT-X-BYTERANGE" => Some("byte-range segments"),
        "#EXT-X-MAP" => Some("fMP4 initialization map"),
        "#EXT-X-DISCONTINUITY" | "#EXT-X-DISCONTINUITY-SEQUENCE" => Some("discontinuities"),
        _ => None,
    }
}

fn tag_name(line: &str) -> &str {
    line.split_once(':').map_or(line, |(name, _)| name)
}

fn malformed<T>(line: usize, reason: &'static str) -> Result<T, Error> {
    Err(Error::Malformed { line, reason })
}

fn has_scheme(uri: &str) -> bool {
    let Some(colon) = uri.find(':') else {
        return false;
    };
    let first_delimiter = uri.find(['/', '?']).unwrap_or(uri.len());
    if colon > first_delimiter || colon == 0 {
        return false;
    }
    let scheme = &uri[..colon];
    scheme.as_bytes()[0].is_ascii_alphabetic()
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

fn normalize_absolute_path(path: &str) -> Result<String, Error> {
    validate_uri_text(path)?;
    if !path.starts_with('/') || path.starts_with("//") {
        return Err(Error::InvalidUri("path is not origin-relative"));
    }
    let (raw_path, query) = path
        .split_once('?')
        .map_or((path, None), |(path, query)| (path, Some(query)));
    if raw_path.is_empty() {
        return Err(Error::InvalidUri("URI has no path"));
    }

    let mut components = Vec::new();
    for component in raw_path[1..].split('/') {
        match component {
            "" if raw_path == "/" => {}
            "" => return Err(Error::InvalidUri("empty path component")),
            "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(Error::InvalidUri("path escapes origin root"));
                }
            }
            _ => components.push(component),
        }
    }
    let mut normalized = if components.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", components.join("/"))
    };
    if let Some(query) = query {
        if query.is_empty() {
            return Err(Error::InvalidUri("empty query"));
        }
        normalized.push('?');
        normalized.push_str(query);
    }
    Ok(normalized)
}

fn validate_uri_text(uri: &str) -> Result<(), Error> {
    if uri.contains('#') {
        return Err(Error::InvalidUri("fragments are unsupported"));
    }
    if uri.contains('\\') {
        return Err(Error::InvalidUri("backslash in URI"));
    }
    let bytes = uri.as_bytes();
    if bytes
        .iter()
        .any(|b| !b.is_ascii() || b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return Err(Error::InvalidUri("non-ASCII or whitespace byte"));
    }
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return Err(Error::InvalidUri("malformed percent escape"));
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn path_without_query(path: &str) -> &str {
    path.split_once('?').map_or(path, |(path, _)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(path: &str) -> Resource {
        Resource::new(Origin::http("192.0.2.10", 32400), path).unwrap()
    }

    fn media(path: &str, sequence: u64, names: &[&str], end_list: bool) -> MediaPlaylist {
        let source = source(path);
        let mut text = format!(
            "#EXTM3U\n#EXT-X-VERSION:4\n#EXT-X-TARGETDURATION:3\n#EXT-X-MEDIA-SEQUENCE:{sequence}\n"
        );
        for name in names {
            text.push_str("#EXTINF:2.002000, nodesc\n");
            text.push_str(name);
            text.push('\n');
        }
        if end_list {
            text.push_str("#EXT-X-ENDLIST\n");
        }
        parse_media(&source, &text).unwrap()
    }

    #[test]
    fn observed_one_variant_master_resolves_quoted_attributes_and_relative_child() {
        let start = source("/video/:/transcode/universal/start.m3u8?X-Plex-Token=secret");
        let parsed = parse_master(
            &start,
            "#EXTM3U\r\n#EXT-X-VERSION:4\r\n#EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=3538000,RESOLUTION=1280x720,FRAME-RATE=60.000000,CODECS=\"avc1.640028,mp4a.40.2\"\r\nsession/example/base/index.m3u8\r\n",
        )
        .unwrap();

        assert_eq!(parsed.version, Some(4));
        assert_eq!(parsed.variant.bandwidth, 3_538_000);
        assert_eq!(
            parsed.variant.resource.path,
            "/video/:/transcode/universal/session/example/base/index.m3u8"
        );
        assert_eq!(parsed.variant.resource.origin, start.origin);
    }

    #[test]
    fn media_sequence_relative_segments_program_time_and_endlist_are_preserved() {
        let child = source("/session/example/base/index.m3u8");
        let parsed = parse_media(
            &child,
            "#EXTM3U\n#EXT-X-VERSION:4\n#EXT-X-TARGETDURATION:3\n#EXT-X-MEDIA-SEQUENCE:41\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXT-X-PROGRAM-DATE-TIME:2024-05-22T17:18:48.044851000Z\n#EXTINF:0.551044, nodesc\n../segments/00041.ts\n#EXTINF:1.001000, nodesc\n/absolute/00042.ts?part=2\n#EXT-X-ENDLIST\n",
        )
        .unwrap();

        assert_eq!(parsed.media_sequence, 41);
        assert_eq!(parsed.playlist_type, Some(PlaylistType::Event));
        assert_eq!(parsed.segments[0].sequence, 41);
        assert_eq!(parsed.segments[0].duration, Duration::new(0, 551_044_000));
        assert_eq!(
            parsed.segments[0].resource.path,
            "/session/example/segments/00041.ts"
        );
        assert_eq!(parsed.segments[1].sequence, 42);
        assert_eq!(
            parsed.segments[1].resource.path,
            "/absolute/00042.ts?part=2"
        );
        assert!(parsed.end_list);
    }

    #[test]
    fn exact_same_origin_absolute_uri_is_allowed_but_other_origins_are_rejected() {
        let master = source("/start.m3u8");
        let same = parse_master(
            &master,
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nhttp://192.0.2.10:32400/session/index.m3u8\n",
        )
        .unwrap();
        assert_eq!(same.variant.resource.path, "/session/index.m3u8");

        let cross = parse_master(
            &master,
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nhttps://cdn.example/session/index.m3u8\n",
        );
        assert_eq!(cross, Err(Error::CrossOrigin));
        assert_eq!(
            master.resolve("//cdn.example/00000.ts"),
            Err(Error::CrossOrigin)
        );
    }

    #[test]
    fn overlapping_refreshes_emit_each_sequence_once_and_keep_endlist_sticky() {
        let first = media("/session/index.m3u8", 7, &["00007.ts", "00008.ts"], false);
        let second = media(
            "/session/index.m3u8",
            8,
            &["00008.ts", "00009.ts", "00010.ts"],
            true,
        );
        let stale_without_endlist = media(
            "/session/index.m3u8",
            8,
            &["00008.ts", "00009.ts", "00010.ts"],
            false,
        );
        let mut tracker = MediaTracker::default();

        assert_eq!(
            tracker
                .apply(&first)
                .unwrap()
                .new_segments
                .iter()
                .map(|segment| segment.sequence)
                .collect::<Vec<_>>(),
            vec![7, 8]
        );
        let refresh = tracker.apply(&second).unwrap();
        assert_eq!(
            refresh
                .new_segments
                .iter()
                .map(|segment| segment.sequence)
                .collect::<Vec<_>>(),
            vec![9, 10]
        );
        assert!(refresh.end_list);
        assert!(tracker.apply(&second).unwrap().new_segments.is_empty());
        assert!(tracker.apply(&stale_without_endlist).unwrap().end_list);
    }

    #[test]
    fn failed_refresh_does_not_advance_the_dedupe_cursor() {
        let first = media("/session/index.m3u8", 4, &["00004.ts"], false);
        let mut gap = media(
            "/session/index.m3u8",
            5,
            &["00005.ts", "00006.ts", "00008.ts"],
            false,
        );
        // The parser itself assigns consecutive numbers. Corrupt the pure input here to prove
        // the tracker's transaction boundary independently of parser validation.
        gap.segments[2].sequence = 8;
        let valid = media(
            "/session/index.m3u8",
            5,
            &["00005.ts", "00006.ts", "00007.ts"],
            false,
        );
        let mut tracker = MediaTracker::default();
        tracker.apply(&first).unwrap();

        assert_eq!(
            tracker.apply(&gap),
            Err(Error::SequenceGap {
                expected: 7,
                found: 8
            })
        );
        assert_eq!(
            tracker
                .apply(&valid)
                .unwrap()
                .new_segments
                .iter()
                .map(|segment| segment.sequence)
                .collect::<Vec<_>>(),
            vec![5, 6, 7]
        );
    }

    #[test]
    fn changed_overlap_gap_new_source_and_post_end_growth_are_rejected() {
        let first = media("/session/index.m3u8", 10, &["00010.ts", "00011.ts"], false);
        let changed = media(
            "/session/index.m3u8",
            11,
            &["changed.ts", "00012.ts"],
            false,
        );
        let gap = media("/session/index.m3u8", 13, &["00013.ts"], false);
        let other = media("/other/index.m3u8", 12, &["00012.ts"], false);
        let ending = media("/session/index.m3u8", 11, &["00011.ts", "00012.ts"], true);
        let growth = media("/session/index.m3u8", 12, &["00012.ts", "00013.ts"], true);
        let mut tracker = MediaTracker::default();
        tracker.apply(&first).unwrap();

        assert_eq!(tracker.apply(&changed), Err(Error::SegmentChanged(11)));
        assert_eq!(
            tracker.apply(&gap),
            Err(Error::SequenceGap {
                expected: 12,
                found: 13
            })
        );
        assert_eq!(tracker.apply(&other), Err(Error::RefreshSourceChanged));
        tracker.apply(&ending).unwrap();
        assert_eq!(tracker.apply(&growth), Err(Error::SegmentAfterEndList(13)));
    }

    #[test]
    fn multiple_variants_are_rejected() {
        let result = parse_master(
            &source("/start.m3u8"),
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\na.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2000\nb.m3u8\n",
        );
        assert_eq!(result, Err(Error::MultipleVariants));
    }

    #[test]
    fn byte_assembly_features_are_rejected_explicitly() {
        let cases = [
            (
                "#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"",
                "encrypted segments",
            ),
            ("#EXT-X-BYTERANGE:1000@0", "byte-range segments"),
            ("#EXT-X-MAP:URI=\"init.mp4\"", "fMP4 initialization map"),
            ("#EXT-X-DISCONTINUITY", "discontinuities"),
            ("#EXT-X-DISCONTINUITY-SEQUENCE:4", "discontinuities"),
        ];
        for (tag, feature) in cases {
            let text = format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MEDIA-SEQUENCE:0\n{tag}\n#EXTINF:1.0,\n00000.ts\n"
            );
            assert_eq!(
                parse_media(&source("/session/index.m3u8"), &text),
                Err(Error::UnsupportedFeature(feature)),
                "{tag}"
            );
        }
    }

    #[test]
    fn fmp4_segment_without_a_map_is_still_rejected() {
        let result = parse_media(
            &source("/session/index.m3u8"),
            "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:1.0,\n00000.m4s\n",
        );
        assert_eq!(result, Err(Error::SegmentIsNotMpegTs));
    }

    #[test]
    fn malformed_and_unknown_forms_fail_closed() {
        let base = source("/session/index.m3u8");
        let cases = [
            ("#EXT-X-TARGETDURATION:3\n", Error::MissingHeader),
            (
                "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MEDIA-SEQUENCE:0\n00000.ts\n",
                Error::Malformed { line: 4, reason: "segment URI has no EXTINF" },
            ),
            (
                "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:nan,\n00000.ts\n",
                Error::Malformed { line: 4, reason: "invalid EXTINF duration" },
            ),
            (
                "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PART:DURATION=1,URI=\"part.ts\"\n",
                Error::UnsupportedTag("#EXT-X-PART".into()),
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_media(&base, text), Err(expected));
        }
    }

    #[test]
    fn path_resolution_normalizes_dot_segments_and_rejects_ambiguous_uris() {
        let base = source("/a/b/index.m3u8?token=x");
        assert_eq!(
            base.resolve("../c/./00001.ts").unwrap().path,
            "/a/c/00001.ts"
        );
        assert!(matches!(
            base.resolve("../../../escape.ts"),
            Err(Error::InvalidUri(_))
        ));
        assert!(matches!(
            base.resolve("bad%2.ts"),
            Err(Error::InvalidUri(_))
        ));
        assert!(matches!(
            base.resolve("data:text/plain,x"),
            Err(Error::InvalidUri(_))
        ));
        assert!(matches!(
            base.resolve("space here.ts"),
            Err(Error::InvalidUri(_))
        ));
    }

    #[test]
    fn resource_debug_never_prints_a_playlist_query() {
        let resource = source("/session/index.m3u8?X-Plex-Token=not-for-a-log&part=2");
        let debug = format!("{resource:?}");
        assert!(debug.contains("/session/index.m3u8?<query>"));
        assert!(!debug.contains("not-for-a-log"));
        assert!(!debug.contains("part=2"));
    }

    #[test]
    fn observed_sanitized_plex_corpus_accepts_legacy_allow_cache() {
        let master = source("/video/:/transcode/universal/start.m3u8?X-Plex-Token=secret");
        let master = parse_master(
            &master,
            "#EXTM3U\n#EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=575000,RESOLUTION=480x200,FRAME-RATE=24.000\nsession/sanitized/base/index.m3u8\n",
        )
        .unwrap();
        let media = parse_media(
            &master.variant.resource,
            "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-START:TIME-OFFSET=39.000000\n#EXT-X-ALLOW-CACHE:NO\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:2.002000,\n00000.ts\n#EXTINF:2.002000,\n00001.ts\n#EXT-X-ENDLIST\n",
        )
        .unwrap();
        assert_eq!(media.start_offset_micros, Some(39_000_000));
        assert_eq!(media.segments.len(), 2);
        assert!(media.end_list);
    }

    #[test]
    fn start_offset_accepts_the_signed_decimal_shape_but_rejects_extensions() {
        let base = source("/session/index.m3u8");
        let playlist = |tag: &str| {
            format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:3\n{tag}\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:2,\n00000.ts\n"
            )
        };
        assert_eq!(
            parse_media(&base, &playlist("#EXT-X-START:TIME-OFFSET=-1.25"))
                .unwrap()
                .start_offset_micros,
            Some(-1_250_000)
        );
        for tag in [
            "#EXT-X-START:TIME-OFFSET=nan",
            "#EXT-X-START:TIME-OFFSET=1.0000001",
            "#EXT-X-START:TIME-OFFSET=1,PRECISE=YES",
            "#EXT-X-START:PRECISE=YES,TIME-OFFSET=1",
        ] {
            assert!(
                matches!(
                    parse_media(&base, &playlist(tag)),
                    Err(Error::Malformed { .. })
                ),
                "{tag}"
            );
        }
        let duplicate = playlist("#EXT-X-START:TIME-OFFSET=1").replacen(
            "#EXT-X-MEDIA-SEQUENCE",
            "#EXT-X-START:TIME-OFFSET=2\n#EXT-X-MEDIA-SEQUENCE",
            1,
        );
        assert_eq!(
            parse_media(&base, &duplicate),
            Err(Error::Malformed {
                line: 4,
                reason: "duplicate start tag"
            })
        );
    }

    #[test]
    fn start_offset_selects_its_segment_and_total_duration_is_the_whole_movie() {
        let base = source("/session/index.m3u8");
        let parsed = parse_media(
            &base,
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-START:TIME-OFFSET=3.000000\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:2,\n00000.ts\n#EXTINF:2,\n00001.ts\n#EXTINF:2,\n00002.ts\n#EXT-X-ENDLIST\n",
        )
        .unwrap();
        assert_eq!(parsed.total_duration(), Ok(Duration::from_secs(6)));
        assert_eq!(parsed.preferred_start_index(), Ok(1));

        let mut negative = parsed.clone();
        negative.start_offset_micros = Some(-2_000_000);
        assert_eq!(negative.preferred_start_index(), Ok(2));
        negative.start_offset_micros = Some(6_000_000);
        assert_eq!(negative.preferred_start_index(), Ok(3));
    }

    #[test]
    fn transport_inherits_auth_without_exposing_or_replacing_it() {
        let master = source("/start.m3u8?session=s&X-Plex-Token=top-secret");
        let auth = InheritedAuth::capture(&master).unwrap();
        let child = master.resolve("session/index.m3u8?part=2").unwrap();
        assert_eq!(
            auth.request_path(&child).unwrap(),
            "/session/index.m3u8?part=2&X-Plex-Token=top-secret"
        );
        assert_eq!(format!("{auth:?}"), "InheritedAuth(<redacted>)");
        assert_eq!(
            auth.request_path(&source("/segment.ts?X-Plex-Token=other")),
            Err(Error::CredentialChanged)
        );
        assert_eq!(
            auth.request_path(&source(
                "/segment.ts?X-Plex-Token=top-secret&x-plex-token=top-secret"
            )),
            Err(Error::MultipleCredentials)
        );
    }

    #[test]
    fn fresh_segment_clocks_form_one_normalized_millisecond_timeline() {
        let mut timeline = SegmentTimeline::default();
        let mut first = timeline.begin(Duration::new(2, 2_000_000)).unwrap();
        assert_eq!(first.normalize_video(900_000_000_000), MediaTimeMs(0));
        assert_eq!(first.normalize_video(901_125_900_000), MediaTimeMs(1_125));
        // A later physically-arriving audio packet gets its own lane origin, rather than an
        // arbitrary 1.1-second skew inherited from packet order.
        assert_eq!(first.normalize_audio(899_980_000_000), MediaTimeMs(0));
        assert_eq!(first.normalize_audio(900_000_000_000), MediaTimeMs(20));
        timeline.commit(first);

        let mut second = timeline.begin(Duration::from_secs(2)).unwrap();
        assert_eq!(second.normalize_video(7_000_000_000), MediaTimeMs(2_002));
        assert_eq!(second.normalize_video(7_020_000_000), MediaTimeMs(2_022));
        timeline.commit(second);
        assert_eq!(timeline.end(), MediaTimeMs(4_002));
        assert_eq!(timeline.end_ns(), 4_002_000_000);
    }

    #[test]
    fn an_exact_fractional_start_boundary_selects_the_next_segment() {
        let base = source("/session/index.m3u8");
        let parsed = parse_media(
            &base,
            "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-START:TIME-OFFSET=2.002000\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:2.002000,\n00000.ts\n#EXTINF:2.002000,\n00001.ts\n#EXT-X-ENDLIST\n",
        )
        .unwrap();
        assert_eq!(parsed.preferred_start_index(), Ok(1));

        let mut rounded = parsed;
        rounded.start_offset_micros = Some(2_000_000);
        assert_eq!(
            rounded.preferred_start_index(),
            Ok(0),
            "whole-second flooring repeats the segment which already played",
        );
    }
}
