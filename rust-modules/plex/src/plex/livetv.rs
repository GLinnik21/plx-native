//! Live TV reads: the server's DVRs, each one's guide lineup and what is airing now
//! (`docs/pms-api.md` §10).
//!
//! A DVR is a tuner the server owns (an HDHomeRun, or anything speaking its HTTP protocol, such
//! as ErsatzTV) plus the EPG provider that names its channels. Everything a channel row shows
//! comes from that provider, under the DVR's `epgIdentifier` path prefix
//! (`/tv.plex.providers.epg.xmltv:7/…`): the lineup lists the mapped channels, the grid the
//! airings.
//!
//! Watching a channel is three requests, in the order the official LG client makes them (measured
//! 2026-10-21, `docs/pms-api.md` §10): the tune, which makes the server record the channel into a
//! session of its own; the universal decision for that session, which names the playlist this
//! client reads; and the timeline, whose `stopped` is what ends the recording and frees the tuner.

use super::client::{Client, QueryBuilder};
use super::models::{Dvr, EpgChannel, Metadata};
use super::params::TimelineState;

/// One channel as a Live TV row shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveChannel {
    /// The DVR that tunes it — the `{dvr}` of `/livetv/dvrs/{dvr}/channels/{channel}/tune`.
    pub dvr: String,
    /// The guide's channel id, the `{channel}` of the tune (`"C101.242.ersatztv.org"`).
    pub channel: String,
    /// The virtual channel number, `"101"`.
    pub vcn: String,
    /// The channel's name without its number in front (`"Family Channel"`).
    pub name: String,
    /// The provider's logo URL. It is served by the TUNER's host, which the TV may not reach, so
    /// it is fetched through the server's photo transcoder like any other image.
    pub thumb: String,
    /// What is on now: the show of an episode, the title of anything else. Empty when the guide
    /// has no airing for this channel at this second.
    pub now: String,
    /// Unix seconds the current airing ends; 0 when nothing is airing. The row is stale after it.
    pub ends_at: i64,
}

/// `GET /livetv/dvrs`.
pub const DVRS_PATH: &str = "/livetv/dvrs";

/// The provider id is server-sent and becomes the first path segment, so only the characters a
/// provider id is made of pass (`tv.plex.providers.epg.xmltv:7`): never a `/`, `?` or `%`.
pub fn is_provider_id(epg: &str) -> bool {
    !epg.is_empty()
        && epg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'_' | b'-'))
}

/// `GET /{epg}/lineups/dvr/channels` — the channels the DVR has mapped, nothing else.
pub fn epg_channels_path(epg: &str) -> String {
    format!("/{epg}/lineups/dvr/channels")
}

/// `GET /{epg}/grid?beginsAt<=now&endsAt>=now` — one airing per channel: the one on at `now`.
/// The operators are part of the KEY (`beginsAt%3C=`), which [`QueryBuilder`] passes as given.
pub fn on_now_path(epg: &str, now: i64) -> String {
    QueryBuilder::new(format!("/{epg}/grid"))
        .int("beginsAt%3C", now)
        .int("endsAt%3E", now)
        .build()
}

/// The DVRs worth a row: a well-formed provider id and at least one tuner the server last saw
/// alive. A DVR whose tuners all read `dead` would list channels that cannot be tuned.
pub fn usable_dvrs(dvrs: &[Dvr]) -> impl Iterator<Item = &Dvr> {
    dvrs.iter().filter(|d| {
        is_provider_id(&d.epg_identifier) && d.device.iter().any(|t| t.status == "alive")
    })
}

/// A server-sent id that becomes one path segment of the tune: letters, digits and `._-` only.
pub fn is_path_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !id.starts_with('.')
}

/// `POST /livetv/dvrs/{dvr}/channels/{channel}/tune`, or `None` for an id that is not one segment.
pub fn tune_path(dvr: &str, channel: &str) -> Option<String> {
    (is_path_id(dvr) && is_path_id(channel))
        .then(|| format!("/livetv/dvrs/{dvr}/channels/{channel}/tune"))
}

/// What a tune hands back: the server's own session for the channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveSession {
    /// `/livetv/sessions/{uuid}` — the decision's `path` and the timeline's `key`.
    pub key: String,
    /// The airing being recorded, the timeline's `ratingKey`. May be empty.
    pub rating_key: String,
}

const SESSIONS: &str = "/livetv/sessions/";

/// `/livetv/sessions/{uuid}` with nothing after the id.
fn is_session_key(key: &str) -> bool {
    key.strip_prefix(SESSIONS).is_some_and(|id| {
        (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
    })
}

/// The session a tune answered with. The response nests it (subscription → grab operation →
/// item), so this takes the first object anywhere in the body whose `key` is a session key,
/// rather than a fixed path through levels this client has never needed to read.
pub fn session_of(body: &serde_json::Value) -> Option<LiveSession> {
    use serde_json::Value;
    match body {
        Value::Object(map) => {
            if let Some(key) = map.get("key").and_then(Value::as_str).filter(|k| is_session_key(k)) {
                let rating_key = match map.get("ratingKey") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Number(n)) => n.to_string(),
                    _ => String::new(),
                };
                return Some(LiveSession { key: key.to_owned(), rating_key });
            }
            map.values().find_map(session_of)
        }
        Value::Array(items) => items.iter().find_map(session_of),
        _ => None,
    }
}

/// The key names of a JSON tree to `depth` levels, no values: `{MediaContainer:{size,Hub:[{…}]}}`.
/// What a log line may say about a body whose contents it must not repeat.
pub fn shape_of(v: &serde_json::Value, depth: usize) -> String {
    use serde_json::Value;
    match v {
        Value::Object(map) if depth > 0 => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| match v {
                    Value::Object(_) | Value::Array(_) => format!("{k}:{}", shape_of(v, depth - 1)),
                    _ => k.clone(),
                })
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(items) if depth > 0 => {
            format!("[{}]", items.first().map_or_else(String::new, |v| shape_of(v, depth - 1)))
        }
        Value::Object(_) => "{…}".into(),
        Value::Array(_) => "[…]".into(),
        _ => "_".into(),
    }
}

/// `GET /video/:/transcode/universal/decision` for a live session — the official client's
/// direct-play ask (its profile augmentation is added by the caller). `hasMDE=1` is the part that
/// makes it a direct play: the client vouches for its own decode, and PMS answers "MDE=1000, Direct
/// play OK" without matching a direct-play profile — which `Generic` has none of for HLS/MPEG-TS
/// (without it: "No direct play video profile exists for protocol hls", measured on the TV
/// 2026-10-21). `subtitles=none` as on every `hasMDE` ask this app sends: PMS 400s `hasMDE` +
/// `directPlay` with the default `auto` when a subtitle is selected.
pub fn live_decision_path(session: &LiveSession, playback: &str) -> String {
    QueryBuilder::new("/video/:/transcode/universal/decision")
        .str("path", &session.key)
        .int("mediaIndex", 0)
        .int("partIndex", 0)
        .str("protocol", "hls")
        .int("hasMDE", 1)
        .int("directPlay", 1)
        .int("directStream", 1)
        .int("directStreamAudio", 1)
        .str("subtitles", "none")
        .str("session", playback)
        .build()
}

/// What this client plays for a live session: the playlist and the codecs the Load declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LivePlay {
    pub part: String,
    pub vcodec: String,
    pub acodec: String,
}

/// The live decision's profile augmentation: the official LG client's (read from the server log,
/// 2026-10-21, which PMS answered "Direct play OK") — an HLS/MPEG-TS transcode target and NO
/// direct-play profile, so `Generic`'s own HLS direct play stands — with its codec lists narrowed to
/// the ones this pipeline decodes, should the server ever have to convert.
pub const LIVE_PROFILE_EXTRA: &str = "add-transcode-target(type=videoProfile&context=all&protocol=hls\
    &container=mpegts&videoCodec=h264,hevc&audioCodec=aac,ac3,eac3)";

/// The query the official client reads a live playlist with: completed segments only.
const LIVE_EDGE: &str = "?offset=-1.000000";

/// The session's own playlist for one playback, the shape every measured part had.
pub fn session_playlist(session: &LiveSession, playback: &str) -> Option<LivePlay> {
    is_path_id(playback).then(|| LivePlay {
        part: format!("{}/{playback}/index.m3u8{LIVE_EDGE}", session.key),
        vcodec: RECORDED_VCODEC.to_owned(),
        acodec: RECORDED_ACODEC.to_owned(),
    })
}

/// The codecs of every measured recording (`docs/pms-api.md` §10), for a decision that names none.
const RECORDED_VCODEC: &str = "h264";
const RECORDED_ACODEC: &str = "aac";

fn or_recorded(codec: &str, recorded: &str) -> String {
    if codec.is_empty() { recorded } else { codec }.to_owned()
}

/// The playlist a decision named for this session: under the session's own key and an `.m3u8`,
/// with its Media's codecs (the decision OUTPUT is what the Load must describe).
///
/// The part's query is replaced by the official client's `offset=-1.000000`. The decision's own
/// (`offset={first}&X-Plex-Incomplete-Segments=1`, measured on the TV 2026-10-21) lists the segment
/// still being written with `#EXTINF:0`, which `hls::parse_media` refuses as a whole playlist.
pub fn live_part_of(session: &LiveSession, decision: &super::MediaContainer) -> Option<LivePlay> {
    let prefix = format!("{}/", session.key);
    decision.metadata.iter().flat_map(|m| &m.media).find_map(|media| {
        media
            .part
            .iter()
            .map(|p| p.key.as_str())
            .find(|k| {
                k.starts_with(&prefix)
                    && !k.contains("..")
                    && k.split_once('?').map_or(*k, |(path, _)| path).ends_with(".m3u8")
            })
            .map(|part| LivePlay {
                part: format!("{}{LIVE_EDGE}", part.split_once('?').map_or(part, |(path, _)| path)),
                vcodec: or_recorded(&media.video_codec, RECORDED_VCODEC),
                acodec: or_recorded(&media.audio_codec, RECORDED_ACODEC),
            })
    })
}

/// The lineup title without the channel number PMS puts in front of it.
fn channel_name(ch: &EpgChannel) -> String {
    let title = ch.title.trim();
    title
        .strip_prefix(ch.vcn.as_str())
        .filter(|rest| !ch.vcn.is_empty() && rest.starts_with(' '))
        .map_or(title, str::trim_start)
        .to_owned()
}

/// What a tile says is on: the show for an episode, the title for anything else.
fn airing_label(m: &Metadata) -> &str {
    if m.kind == "episode" && !m.grandparent_title.is_empty() {
        &m.grandparent_title
    } else {
        &m.title
    }
}

/// Join one DVR's lineup with its airings into rows, in channel-number order.
pub fn lineup(dvr: &Dvr, channels: &[EpgChannel], on_now: &[Metadata]) -> Vec<LiveChannel> {
    let mut rows: Vec<LiveChannel> = channels
        .iter()
        .filter(|ch| !ch.vcn.is_empty())
        .map(|ch| {
            let airing = on_now.iter().find_map(|m| {
                m.media
                    .iter()
                    .find(|media| media.channel_vcn == ch.vcn)
                    .map(|media| (m, media.ends_at))
            });
            LiveChannel {
                dvr: dvr.key.clone(),
                channel: ch.grid_key.clone(),
                vcn: ch.vcn.clone(),
                name: channel_name(ch),
                thumb: ch.thumb.clone(),
                now: airing.map_or_else(String::new, |(m, _)| airing_label(m).to_owned()),
                ends_at: airing.map_or(0, |(_, ends)| ends),
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        let num = |c: &LiveChannel| c.vcn.parse::<u64>().unwrap_or(u64::MAX);
        num(a).cmp(&num(b)).then_with(|| a.vcn.cmp(&b.vcn))
    });
    rows
}

impl Client {
    /// `GET /livetv/dvrs` — every DVR with its tuners. `None` when the read failed; an empty list
    /// is a server with no DVR.
    pub fn live_tv_dvrs(&self) -> Option<Vec<Dvr>> {
        self.get_json(DVRS_PATH).map(|c| c.dvr)
    }

    /// `GET /{epg}/lineups/dvr/channels` — one DVR's mapped channels.
    pub fn epg_channels(&self, epg: &str) -> Option<Vec<EpgChannel>> {
        if !is_provider_id(epg) {
            return None;
        }
        self.get_json(&epg_channels_path(epg)).map(|c| c.channel)
    }

    /// `GET /{epg}/grid` bounded to the airings on at `now` (Unix seconds).
    pub fn epg_on_now(&self, epg: &str, now: i64) -> Option<Vec<Metadata>> {
        if !is_provider_id(epg) {
            return None;
        }
        self.get_json(&on_now_path(epg, now)).map(|c| c.metadata)
    }

    /// Every channel this server can tune right now, with what is on. `None` when the DVR list or
    /// a lineup could not be read. A failed airing read keeps the channels, with nothing on.
    pub fn live_channels(&self, now: i64) -> Option<Vec<LiveChannel>> {
        let dvrs = self.live_tv_dvrs()?;
        let mut rows = Vec::new();
        for dvr in usable_dvrs(&dvrs) {
            let channels = self.epg_channels(&dvr.epg_identifier)?;
            let on_now = self
                .epg_on_now(&dvr.epg_identifier, now)
                .unwrap_or_default();
            rows.extend(lineup(dvr, &channels, &on_now));
        }
        Some(rows)
    }

    /// `POST …/tune` — the server starts recording the channel and answers once it streams (4.8 s
    /// measured). `None` when it refused, failed, or answered without a session. The playback
    /// session id is required: without one PMS answers 400 "A session identifier is required for
    /// this request" before it looks at the channel (measured on the TV, 2026-10-21).
    pub fn tune(&self, _off: &plx_base::task::OffFrame, dvr: &str, channel: &str, playback: &str) -> Option<LiveSession> {
        let q = QueryBuilder::new(tune_path(dvr, channel)?).str("X-Plex-Session-Identifier", playback);
        let body = self.post_value(&self.playback_identity(q).build())?;
        let session = session_of(&body);
        if session.is_none() {
            // The tune's body has never been captured: say what it held, names only, no values.
            plx_base::eventlog::log(&format!("livetv: tune answered without a session; shape {}", shape_of(&body, 3)));
        }
        session
    }

    /// The playlist this client reads for `session`: the decision's part, or — when the decision
    /// was refused or named none — the session's own playlist for this playback,
    /// `{key}/{playback}/index.m3u8`: PMS names the recording's consumer after the session id the
    /// tune carried (`Grabber/{channel}-{playback}` in its log, 2026-10-21).
    ///
    /// The decision asks with the `Generic` profile and [`LIVE_PROFILE_EXTRA`]: with no profile PMS
    /// answers 400 "unable to find a matching profile", and with the library's (whose direct-play
    /// profile names mkv/mp4 only) it decides a conversion — "No direct play video profile exists
    /// for protocol hls, with container mpegts" — and then refuses the session's own playlist
    /// ("doesn't allow direct play"). Both measured on the TV, 2026-10-21.
    pub fn live_part(&self, _off: &plx_base::task::OffFrame, session: &LiveSession, playback: &str) -> Option<LivePlay> {
        let header = format!("X-Plex-Session-Identifier: {playback}");
        let q = QueryBuilder::new(live_decision_path(session, playback))
            .str("X-Plex-Client-Profile-Name", "Generic")
            .str("X-Plex-Client-Profile-Extra", LIVE_PROFILE_EXTRA);
        let decision = self.get_json_with_headers(&self.playback_identity(q).build(), &[&header]);
        if let Some(play) = decision.as_ref().and_then(|d| live_part_of(session, d)) {
            return Some(play);
        }
        let verdict = decision
            .as_ref()
            .and_then(|d| d.metadata.iter().flat_map(|m| &m.media).flat_map(|m| &m.part).next())
            .map_or("", |p| p.decision.as_str());
        plx_base::eventlog::log(&format!(
            "livetv: the decision named no playlist (answered={} part decision={verdict:?}); reading the session's own",
            decision.is_some() as i32
        ));
        session_playlist(session, playback)
    }

    /// `POST /:/timeline` for a live session: `time`/`duration` stay 0 as the official client sends
    /// them. `Stopped` is the request that ends the recording and frees the tuner.
    pub fn live_timeline(&self, session: &LiveSession, state: TimelineState, watched_ms: i64, playback: &str) -> bool {
        let q = QueryBuilder::new("/:/timeline")
            .str("key", &session.key)
            .opt_str("ratingKey", &session.rating_key)
            .str("state", state.as_str())
            .int("time", 0)
            .int("duration", 0)
            .int("playbackTime", watched_ms.max(0))
            .str("X-Plex-Session-Identifier", playback);
        // `continuing=0`: nothing follows on this session (the official client's stop says so).
        let q = if state == TimelineState::Stopped { q.int("continuing", 0) } else { q };
        self.post_ok(&self.playback_identity(q).build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::MediaContainer;

    fn page(json: &[u8]) -> MediaContainer {
        serde_json::from_slice::<super::super::models::Envelope>(json)
            .expect("live tv fixture")
            .media_container
    }

    // Shapes measured on PMS 1.43.3 with an ErsatzTV tuner; hosts are documentation addresses.
    const DVRS: &[u8] = br#"{"MediaContainer":{"size":1,"Dvr":[{"key":"7",
        "uuid":"00000000-0000-0000-0000-000000000007","language":"es","country":"ESP",
        "lineup":"lineup://tv.plex.providers.epg.xmltv/http%3A%2F%2F192.0.2.10%3A8409%2Fiptv%2Fxmltv.xml#ErsatzTV",
        "epgIdentifier":"tv.plex.providers.epg.xmltv:7",
        "Device":[{"key":"6","status":"alive","state":"enabled","uri":"http://192.0.2.10:8409",
          "make":"ErsatzTV","tuners":"2","lastSeenAt":1792000000,
          "ChannelMapping":[{"channelKey":"C101.242.ersatztv.org","deviceIdentifier":"101",
            "enabled":"1","lineupIdentifier":"101"}]}]}]}}"#;

    const CHANNELS: &[u8] = br#"{"MediaContainer":{"size":3,"Channel":[
        {"id":"C102.243.ersatztv.org","gridKey":"C102.243.ersatztv.org","vcn":"102",
         "thumb":"http://192.0.2.10:8409/iptv/logos/gen?text=Cartoons","title":"102 Cartoons",
         "callSign":"102 Cartoons"},
        {"id":"C101.242.ersatztv.org","gridKey":"C101.242.ersatztv.org","vcn":"101",
         "thumb":"http://192.0.2.10:8409/iptv/logos/gen?text=Family","title":"101 Family Channel",
         "callSign":"101 Family Channel"},
        {"id":"C103.244.ersatztv.org","gridKey":"C103.244.ersatztv.org","vcn":"103",
         "thumb":"","title":"Anime Movies","callSign":"Anime Movies"}]}}"#;

    const ON_NOW: &[u8] = br#"{"MediaContainer":{"size":2,"Metadata":[
        {"ratingKey":"tv%2Eplex%2Exmltv%3A%2F%2Fepisode%2FFamily%2F3%2F23","type":"episode",
         "title":"Family - S03E23 - Placeholder","grandparentTitle":"Family Show","onAir":true,
         "Media":[{"id":48,"channelVcn":"101","channelIdentifier":"C101.242.ersatztv.org",
           "protocol":"livetv","beginsAt":1792523168,"endsAt":1792524417,"onAir":true}],
         "Channel":[{"id":2,"filter":"channel=2","tag":"101 101 Family Channel"}]},
        {"ratingKey":"tv%2Eplex%2Exmltv%3A%2F%2Fmovie%2FPlaceholder","type":"movie",
         "title":"Placeholder Movie","Media":[{"channelVcn":"103","beginsAt":"1792520000",
           "endsAt":"1792527000"}]}]}}"#;

    #[test]
    fn measured_live_tv_shapes_parse() {
        let dvrs = page(DVRS).dvr;
        assert_eq!(dvrs.len(), 1);
        assert_eq!(
            (dvrs[0].key.as_str(), dvrs[0].epg_identifier.as_str()),
            ("7", "tv.plex.providers.epg.xmltv:7")
        );
        assert_eq!(
            (dvrs[0].device[0].key.as_str(), dvrs[0].device[0].status.as_str()),
            ("6", "alive")
        );

        let channels = page(CHANNELS).channel;
        assert_eq!(
            channels.iter().map(|c| c.vcn.as_str()).collect::<Vec<_>>(),
            ["102", "101", "103"]
        );
        assert_eq!(channels[1].grid_key, "C101.242.ersatztv.org");

        let on_now = page(ON_NOW).metadata;
        let media = &on_now[0].media[0];
        assert_eq!(
            (media.channel_vcn.as_str(), media.begins_at, media.ends_at),
            ("101", 1_792_523_168, 1_792_524_417)
        );
        assert_eq!(on_now[1].media[0].ends_at, 1_792_527_000, "string-encoded times still parse");
    }

    #[test]
    fn lineup_joins_airings_in_channel_order_and_drops_the_number_from_names() {
        let dvr = page(DVRS).dvr.remove(0);
        let rows = lineup(&dvr, &page(CHANNELS).channel, &page(ON_NOW).metadata);
        let shown: Vec<_> = rows
            .iter()
            .map(|r| (r.vcn.as_str(), r.name.as_str(), r.now.as_str(), r.ends_at))
            .collect();
        assert_eq!(
            shown,
            [
                ("101", "Family Channel", "Family Show", 1_792_524_417),
                ("102", "Cartoons", "", 0),
                ("103", "Anime Movies", "Placeholder Movie", 1_792_527_000),
            ]
        );
        assert!(rows.iter().all(|r| r.dvr == "7"));
        assert_eq!(rows[0].channel, "C101.242.ersatztv.org", "the tune names the guide's id");
        assert_eq!(rows[0].thumb, "http://192.0.2.10:8409/iptv/logos/gen?text=Family");
    }

    #[test]
    fn only_dvrs_with_a_live_tuner_and_a_clean_provider_id_get_a_row() {
        let mut dead = page(DVRS).dvr.remove(0);
        dead.device[0].status = "dead".into();
        let mut odd = page(DVRS).dvr.remove(0);
        odd.epg_identifier = "tv.plex.providers.epg.xmltv:7/../library".into();
        let good = page(DVRS).dvr.remove(0);
        let all = [dead, odd, good];
        let usable: Vec<_> = usable_dvrs(&all).collect();
        assert_eq!(usable.len(), 1);
        assert!(usable[0].device[0].status == "alive" && is_provider_id(&usable[0].epg_identifier));
        assert!(!is_provider_id(""));
        assert!(!is_provider_id("a?b"));
    }

    #[test]
    fn a_tune_names_one_dvr_and_one_channel_segment_or_nothing() {
        assert_eq!(
            tune_path("7", "C101.242.ersatztv.org").as_deref(),
            Some("/livetv/dvrs/7/channels/C101.242.ersatztv.org/tune")
        );
        for (dvr, channel) in [("7", ""), ("", "C1"), ("7", "../library"), ("7", "a/b"), ("7?x", "C1"), ("7", "..")] {
            assert_eq!(tune_path(dvr, channel), None, "{dvr:?} {channel:?}");
        }
    }

    // The tune's body was not captured (only its size, 2764 bytes); this nests the session the
    // way the subscription/grab/item names in the server log describe, two levels down.
    const TUNED: &str = r#"{"MediaContainer":{"size":1,"MediaSubscription":[{"id":112,"type":4,
        "MediaGrabOperation":[{"status":"inprogress","Metadata":{"key":"/livetv/sessions/9b9fe727-0000-46db-b94c-ef512e3e61d2",
        "ratingKey":"355108","type":"episode","grandparentTitle":"Family Show","live":"1"}}]}]}}"#;

    #[test]
    fn the_session_is_found_wherever_the_tune_nests_it() {
        let body: serde_json::Value = serde_json::from_str(TUNED).unwrap();
        assert_eq!(
            session_of(&body),
            Some(LiveSession {
                key: "/livetv/sessions/9b9fe727-0000-46db-b94c-ef512e3e61d2".into(),
                rating_key: "355108".into(),
            })
        );
        let numeric: serde_json::Value =
            serde_json::from_str(r#"{"Metadata":[{"key":"/livetv/sessions/abcdef01","ratingKey":7}]}"#).unwrap();
        assert_eq!(session_of(&numeric).unwrap().rating_key, "7");
        for odd in [
            r#"{"key":"/library/metadata/1"}"#,
            r#"{"key":"/livetv/sessions/abcdef01/xyz/index.m3u8"}"#,
            r#"{"key":"/livetv/sessions/../../library"}"#,
            r#"{"MediaContainer":{"size":0}}"#,
        ] {
            assert_eq!(session_of(&serde_json::from_str(odd).unwrap()), None, "{odd}");
        }
    }

    #[test]
    fn the_live_profile_is_an_hls_target_that_leaves_generic_s_direct_play_alone() {
        assert!(LIVE_PROFILE_EXTRA.starts_with("add-transcode-target(type=videoProfile&context=all&protocol=hls&container=mpegts&"));
        assert!(!LIVE_PROFILE_EXTRA.contains("add-direct-play-profile"), "one replaces Generic's HLS direct play");
        assert!(!LIVE_PROFILE_EXTRA.contains(char::is_whitespace));
    }

    #[test]
    fn a_body_s_shape_names_its_keys_and_never_its_values() {
        let body: serde_json::Value =
            serde_json::from_str(r#"{"MediaContainer":{"size":1,"title":"secret","Item":[{"key":"/x","n":2}]}}"#).unwrap();
        // serde_json's map is ordered by key here, so the names come sorted
        assert_eq!(shape_of(&body, 3), "{MediaContainer:{Item:[{…}],size,title}}");
        assert_eq!(shape_of(&body, 4), "{MediaContainer:{Item:[{key,n}],size,title}}");
        assert!(!shape_of(&body, 9).contains("secret"));
    }

    #[test]
    fn without_a_usable_decision_the_session_s_own_playlist_is_read_under_the_playback_id() {
        assert_eq!(
            session_playlist(&session(), "d1f32c49-d8f8-4621-9075-f3d80792824a"),
            Some(LivePlay {
                part: "/livetv/sessions/9b9fe727-0000-46db-b94c-ef512e3e61d2/d1f32c49-d8f8-4621-9075-f3d80792824a/index.m3u8?offset=-1.000000".into(),
                vcodec: "h264".into(),
                acodec: "aac".into(),
            })
        );
        assert_eq!(session_playlist(&session(), "../x"), None);
        assert_eq!(session_playlist(&session(), ""), None);
    }

    fn session() -> LiveSession {
        LiveSession { key: "/livetv/sessions/9b9fe727-0000-46db-b94c-ef512e3e61d2".into(), rating_key: "355108".into() }
    }

    #[test]
    fn the_decision_asks_to_direct_play_the_session_and_its_part_must_stay_under_it() {
        assert_eq!(
            live_decision_path(&session(), "pb1"),
            "/video/:/transcode/universal/decision?path=%2Flivetv%2Fsessions%2F9b9fe727-0000-46db-b94c-ef512e3e61d2\
             &mediaIndex=0&partIndex=0&protocol=hls&hasMDE=1&directPlay=1&directStream=1&directStreamAudio=1&subtitles=none&session=pb1"
        );
        let decided = |key: &str| {
            page(format!(r#"{{"MediaContainer":{{"Metadata":[{{"Media":[{{"videoCodec":"hevc","Part":[{{"key":"{key}","decision":"directplay"}}]}}]}}]}}}}"#).as_bytes())
        };
        let measured = "/livetv/sessions/9b9fe727-0000-46db-b94c-ef512e3e61d2/clientid/index.m3u8?offset=-1.000000";
        assert_eq!(
            live_part_of(&session(), &decided(measured)),
            Some(LivePlay { part: measured.into(), vcodec: "hevc".into(), acodec: "aac".into() }),
            "the decision's codecs, the recording's where it names none"
        );
        let incomplete = "/livetv/sessions/9b9fe727-0000-46db-b94c-ef512e3e61d2/clientid/index.m3u8?offset=0.021333&X-Plex-Incomplete-Segments=1";
        assert_eq!(
            live_part_of(&session(), &decided(incomplete)).map(|p| p.part).as_deref(),
            Some(measured),
            "the in-progress segment (#EXTINF:0) is not asked for"
        );
        for wrong in [
            "/livetv/sessions/other/clientid/index.m3u8",
            "/livetv/sessions/9b9fe727-0000-46db-b94c-ef512e3e61d2/../../x.m3u8",
            "/livetv/sessions/9b9fe727-0000-46db-b94c-ef512e3e61d2/clientid/00001.ts",
        ] {
            assert_eq!(live_part_of(&session(), &decided(wrong)), None, "{wrong}");
        }
    }

    #[test]
    fn paths_carry_the_provider_and_the_window_operators_in_the_key() {
        assert_eq!(
            epg_channels_path("tv.plex.providers.epg.xmltv:7"),
            "/tv.plex.providers.epg.xmltv:7/lineups/dvr/channels"
        );
        assert_eq!(
            on_now_path("tv.plex.providers.epg.xmltv:7", 1_792_524_066),
            "/tv.plex.providers.epg.xmltv:7/grid?beginsAt%3C=1792524066&endsAt%3E=1792524066"
        );
    }
}
