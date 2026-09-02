//! Typed request params for the playback protocol ops — built per call by `route` from its
//! playback state (the client itself is stateless about the current item/selection).
//!
//! These were rebuilt FROM the live `route.rs` (not the original design sketch): the shipped
//! protocol carries a per-playback session id on every request, distinguishes a container-only
//! REMUX from a re-encode, and reports PlayQueue + stream-selection ids on the timeline.

/// The wire/container contract for one universal-transcoder session.
///
/// This is carried beside the request instead of inferred from its eventual URL: the capability
/// profile, decision query, start endpoint and demux strategy are one choice and must never drift
/// independently. Fixed HLS is deliberately a single-rendition session on the measured PMS; an
/// adaptive quality change primes a replacement encoder session rather than pretending that the
/// one-entry master playlist is client-selectable ABR.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TranscodeDelivery {
    /// The established progressive Matroska stream (`start.mkv`).
    #[default]
    ProgressiveMkv,
    /// A fixed rendition delivered as a complete HLS VOD playlist of MPEG-TS segments.
    FixedHls { seconds_per_segment: u8 },
}

/// Exact content boundary for a universal-transcoder start.
///
/// PMS declares this query parameter as a number of seconds, not an integer. Keeping the value in
/// microseconds matches the six-decimal `EXTINF`/`EXT-X-START` contract and prevents an adaptive
/// encoder handoff at 2.002 s from being rounded back into the segment which already played.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TranscodeOffset {
    Fresh,
    AtMicros(i64),
}

impl TranscodeOffset {
    pub fn from_seconds(seconds: i64) -> Self {
        if seconds < 0 {
            Self::Fresh
        } else {
            Self::AtMicros(seconds.saturating_mul(1_000_000))
        }
    }

    pub fn from_micros(micros: i64) -> Self {
        Self::AtMicros(micros.max(0))
    }

    pub(crate) fn wire_seconds(self) -> Option<String> {
        let Self::AtMicros(micros) = self else {
            return None;
        };
        Some(format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000))
    }
}

/// One universal-transcoder request (decision registration + the delivery's start endpoint). Mirrors
/// `route::universal_base`: the CURRENT audio/subtitle selection rides every transcode of the
/// item. `session` is the PMS playback/timeline wire id; `encoder_session` owns the physical
/// transcoder. The mismatch probe measured the two independently, but the simultaneous-encoder
/// TV spike proved PMS cannot prime a replacement while it shares the old X-Plex id. Production
/// therefore keeps them equal for each encoder; the app's stable playback generation is internal
/// and an adaptive replacement changes both wire fields together.
///
/// **A ceiling is answered by choosing the FLAVOR, and only then by asking for a number.** The
/// only bitrate literal in the whole spec is `maxVideoBitrate`, on the RE-ENCODE branch of
/// `transcoder::transcode_query`; the [`remux`](Self::remux) branch deliberately sends no cap at
/// all, because a resolution or bitrate cap is precisely what makes PMS re-encode instead of copy.
/// And on the third path — direct play, this client's default and its whole point — a cap means
/// nothing, since no encoder is running to obey it. So a link with a ceiling (Plex's 2 Mbit/s
/// relay) is answered by denying direct play and denying the remux, leaving the one branch that
/// can pick a rate: `transcoder::link_policy` is that decision, with the reasoning and the
/// honesty note.
///
/// **[`ceiling`](Self::ceiling) does not contradict that paragraph — it is its second half, and
/// it arrives only after the first half has run.** This field said "there is no bitrate field
/// here" until 2026-08-23, and the sentence was right about a LINK ceiling and wrong as a general
/// rule, because a link is not the only thing that can impose one. A **user-chosen** ceiling
/// (`route::Quality` — the picker behind the player's `…` menu, for LG checklist #43 CASE1) is a
/// different input with the identical mechanism: `route::quality_policy` returns the same
/// [`LinkPolicy`](super::LinkPolicy) two flags `link_policy` does, `build_stream` composes the two
/// by AND so the stricter always wins, and only once direct play and the remux have BOTH been
/// denied does a number reach the wire here. Adding the field without that gate is the change that
/// looks like it works and does nothing: the file that most needs the cap — a 30 Mbit/s source on
/// a 4 Mbit/s link — is exactly the one that direct-plays, where no encoder ever reads it.
///
/// The invariant that keeps the two halves honest: a spec may carry a ceiling **and**
/// `remux == true` only when the source was already measured under it, so the remux branch still
/// sends no cap and still does not need one. It is asserted in two places because it is two
/// claims — `transcoder`'s tests pin that a ceiling is INERT on a remux, and `route`'s gates 2
/// and 4 pin that a remux is only ever reached for a source under it.
pub struct TranscodeSpec<'a> {
    pub rating_key: &'a str,
    /// The active encoder's PMS playback/timeline wire id.
    pub session: &'a str,
    /// Physical universal-transcoder identity. Production keeps it equal to `session`; the two
    /// fields remain explicit because the protocol probe and fixtures grade their wire roles.
    pub encoder_session: &'a str,
    /// The coupled profile/query/endpoint/demux contract for this encoder session.
    pub delivery: TranscodeDelivery,
    /// true = container-only remux (the source codecs are direct-playable, the container
    /// isn't): copy video+audio into progressive MKV, no re-encode, keeps 4K/HDR.
    /// false = full re-encode to the profile's HEVC/AC3 target at up to 4K.
    pub remux: bool,
    /// **The server must not COPY the video track — an encoder has to run.** Only meaningful on
    /// the re-encode flavor (`remux == false`), where it swaps `directStream=1` for
    /// `directStream=0` + `directStreamAudio=1`: video re-encoded, audio still copied when it can
    /// be. Off for every ordinary transcode, where a copy is a free win.
    ///
    /// It exists because "we refused direct play" and "the server will re-encode" are NOT the same
    /// statement, and the gap between them shipped a fix that fixed nothing. `directStream=1`
    /// permits a stream copy, and PMS takes it whenever the source fits the caps the query carries
    /// — resolution, bitrate, and the profile's own limitation axes (width/height/bitDepth).
    /// **None of those axes can express Dolby Vision**, so a Profile 5 file refused by
    /// `route::video_direct_plays` came back `Part.decision=transcode` with the VIDEO stream's own
    /// decision `copy`: the identical IPT-PQ bitstream, one container down, the identical wrong
    /// colours (measured against the dev PMS 2026-08-21 — `docs/pms-api.md` §"What the server
    /// actually does with a Dolby Vision source").
    ///
    /// Setting it makes the ask honest, and the answer honest with it: a server that CAN convert
    /// the file does, and a server that cannot says so (this PMS answers
    /// `generalDecisionCode 2000` / *"File is unplayable. DoVi (Profile 5) color space is not
    /// supported."*), which `route::refusal` turns into the player's read-out quoting that
    /// sentence. Both beat a picture in the wrong colours with nothing on any surface to explain
    /// it.
    pub no_video_copy: bool,
    /// Source audio stream id to select (0 = server default).
    pub audio_stream_id: i64,
    /// Subtitle stream id to BURN (0 = none). Burn is Plex's decision for our profile —
    /// it advertises no soft-sub support (direct-play subs are client-rendered instead).
    pub subtitle_stream_id: i64,
    /// Restart at this exact content boundary, or omit `offset` for a fresh start.
    pub offset: TranscodeOffset,
    /// The bound this playback's RE-ENCODE may not exceed — the user's pick off the quality
    /// ladder or Auto controller's current rung. `None` = Original/unrestricted and resolves to
    /// the historical [`Ceiling::NATIVE_4K`] query values.
    ///
    /// Read on the re-encode branch **only**, and that is not an oversight: see the type doc
    /// above. By the time a `Some` reaches here, `route::quality_policy` has already refused the
    /// two flavors no number can bind.
    pub ceiling: Option<Ceiling>,
}

/// A bound a playback must come in under, in the units PMS's own params use: `maxVideoBitrate` is
/// **kbps**, `videoResolution` is a `WxH` pair. One value rather than two scattered ints, because
/// the two axes have to move together — a rung that halves the rate and keeps 4K asks the server
/// for something it cannot make look like anything.
///
/// Lives here rather than in `route` so the ladder's rungs and the query that spends them are one
/// type: `route::Quality::ceiling` is the only constructor of a non-default one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ceiling {
    /// `maxVideoBitrate`, in **kbps** (PMS's unit — not bits, not bytes).
    pub max_kbps: i64,
    /// `videoResolution`, as the frame the encode may not exceed.
    pub max_w: i64,
    pub max_h: i64,
}

impl Ceiling {
    /// What the re-encode branch asked for before user ceilings existed: the panel's own native 4K
    /// at a rate high enough to be no bound in practice. This is the `None`/Original substitute;
    /// Auto always supplies an explicit bootstrap or controller rung.
    pub const NATIVE_4K: Ceiling = Ceiling {
        max_kbps: 60000,
        max_w: 3840,
        max_h: 2160,
    };

    /// `videoResolution`'s value for this bound.
    pub fn resolution(&self) -> String {
        format!("{}x{}", self.max_w, self.max_h)
    }

    /// Does a source measured at `kbps` and `w`x`h` come in under this bound?
    ///
    /// **An unmeasured source does NOT** — `0` is "the server did not say", and it fails CLOSED
    /// here while the same `0` PASSES `route::video_direct_plays`'s device-capability gate. The
    /// asymmetry is the point and it is a decision, not an inconsistency: a device bound is a
    /// CAPABILITY, so an unknown degrades to "assume yesterday's behaviour works" (the rule
    /// `devcaps::parse` applies); a user ceiling is an explicit ASK, and the only way to honour an
    /// ask about a file you have not measured is to route it to the branch where the server
    /// applies the bound for you. Failing open here would reproduce, for every play from a shelf
    /// that never loaded a detail page, exactly the do-nothing bug the type doc describes.
    pub fn admits(&self, kbps: i64, w: i64, h: i64) -> bool {
        kbps > 0 && w > 0 && h > 0 && kbps <= self.max_kbps && w <= self.max_w && h <= self.max_h
    }
}

/// One paged section-listing query (the Library browse grid). Built per fetch by the browse
/// store; consumed by `library.rs::section_items_query`. Filters are `(param, value)` pairs
/// appended verbatim (`("genre","150")`, `("unwatched","1")`, shows: `("unwatchedLeaves","1")`)
/// — tag-id values come from `section_directory` value lists. Paging: Start AND Size are
/// always sent together — PMS silently ignores a lone `X-Plex-Container-Size` query param
/// (verified live on PMS 1.43.2, 2026-07-19).
pub struct SectionQuery<'a> {
    pub section_key: i64,
    /// `sort=key[:desc]` value, e.g. "titleSort" / "addedAt:desc"; empty = server default.
    pub sort: &'a str,
    pub filters: &'a [(String, String)],
    pub start: i64,
    pub size: i64,
    /// `includeMeta=1` — the response's `Meta.Type[]` carries the section's full server-driven
    /// Sort/Filter menus; request it on the first page of a section, skip on later pages.
    pub include_meta: bool,
}

/// Server-side stream selection for a part (`PUT /library/parts/{id}`) — the transcoder
/// encodes the part's SELECTED audio and burns its SELECTED subtitle; only a PUT changes
/// them (query params on the stream URL do not).
pub struct StreamSelection {
    pub part_id: i64,
    /// 0 = keep the server default (the PUT omits audioStreamID).
    pub audio_stream_id: i64,
    /// Always sent: 0 = subtitles OFF (suppresses a default-selected burn), else burn this id.
    pub subtitle_stream_id: i64,
}

/// One `/:/timeline` progress report (POST — the spec verb). `play_queue_*` empty = omit;
/// `*_stream_id` 0 = omit. PMS exposes timeline playback and encoder ownership independently,
/// but active HLS playback reports the current encoder's coupled wire identity so they remain
/// correlated.
pub struct TimelineReport<'a> {
    pub rating_key: &'a str,
    pub state: TimelineState,
    pub time_ms: i64,
    pub duration_ms: i64,
    pub session: &'a str,
    pub play_queue_id: &'a str,
    pub play_queue_item_id: &'a str,
    pub audio_stream_id: i64,
    pub subtitle_stream_id: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TimelineState {
    Playing,
    Paused,
    Stopped,
}

impl TimelineState {
    pub fn as_str(self) -> &'static str {
        match self {
            TimelineState::Playing => "playing",
            TimelineState::Paused => "paused",
            TimelineState::Stopped => "stopped",
        }
    }
}
