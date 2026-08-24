//! Item detail data layer for the detail page: full metadata (genres, cast, crew,
//! audio/subtitle streams), the TV season/episode hierarchy, and the related hub —
//! fetched on demand into a single CURRENT item. Idiomatic Rust (String/Vec), like the
//! browse catalog (pms.rs) — the fixed C buffers from the C port are gone.
use std::panic::catch_unwind;
use std::ptr::{addr_of, addr_of_mut};

/// Plex's resume rule, in ONE place (home Continue-Watching, the detail Play button, and the
/// plxnative-play harness all apply it): resume only past 10s and before 95% watched, else start
/// from the beginning. Both args are MILLISECONDS; the returned position is NANOSECONDS
/// (what `player::resume_at` takes).
pub(crate) fn resume_ns(resume_ms: i64, dur_ms: i64) -> i64 {
    if resume_ms > 10_000 && (dur_ms <= 0 || (resume_ms as f64) < 0.95 * dur_ms as f64) {
        resume_ms * 1_000_000
    } else {
        0
    }
}

/// Friendly display name for an audio/subtitle codec id — the ONE codec→name map (the track
/// menu's section accessory and the Info card's track line both read it, so the same track
/// can't be named two ways).
pub(crate) fn friendly_codec(codec: &str) -> String {
    match codec.to_lowercase().as_str() {
        "truehd" => "Dolby TrueHD".to_string(),
        "eac3" | "ec-3" => "Dolby Digital Plus".to_string(),
        "ac3" => "Dolby Digital".to_string(),
        "dts" | "dca" => "DTS".to_string(),
        "aac" => "AAC".to_string(),
        "flac" => "FLAC".to_string(),
        "opus" => "Opus".to_string(),
        "mp3" => "MP3".to_string(),
        other if other.is_empty() => String::new(),
        other => other.to_uppercase(),
    }
}

/// One credit on the Cast & Crew shelf. PMS ships crew (`Director[]`/`Writer[]`) in the SAME shape
/// as the actors (`Role[]`) minus the `role` attribute, so a crew credit is this same struct with
/// its JOB in `role` — see [`crew_credits`].
pub(crate) struct Cast {
    pub(crate) tag: String,   // person's name
    pub(crate) role: String,  // character (an actor) — or the job, "Director"/"Writer" (crew)
    pub(crate) thumb: String, // headshot (often an external metadata-static.plex.tv URL)
    /// The person's numeric library id (`Role[].id`), 0 when absent.
    pub(crate) id: i64,
    /// The person's global Plex guid (`Role[].tagKey`) — the id's stand-in when the server
    /// omits the numeric one.
    pub(crate) tag_key: String,
}

impl Cast {
    /// The `personId` for `/library/people/{personId}/media` — the numeric id when the server
    /// sent one, else the global guid (PMS accepts EITHER; both verified live 2026-07-29).
    /// Empty when the row carries neither, which is the "this headshot opens nothing" case the
    /// cast row's OK arm gates on.
    pub(crate) fn person_key(&self) -> String {
        if self.id > 0 {
            self.id.to_string()
        } else {
            self.tag_key.clone()
        }
    }
}

/// What PMS says about a video stream's **Dolby Vision** layering — read together, because the
/// only question worth asking of them is a joint one: **is the base layer, alone, a correct
/// picture?**
///
/// It has to be a joint question because the buffer-feed pipeline has no other option. We feed one
/// elementary stream to a decoder that has never heard of an RPU, so whatever the base layer
/// contains is exactly what reaches the panel. That is fine for **Profile 8.1**, whose base layer
/// IS an HDR10 stream — ignoring the RPU costs the dynamic metadata and nothing else. It is not
/// fine for **Profile 5**, which is single-layer IPT-PQ with no HDR10 fallback: decoded as
/// ordinary HEVC it is not a dimmer picture but a WRONG one, with the washed, pink-green cast of
/// IPT read as YCbCr. And it cannot work at all for **Profile 7**, whose picture is split across
/// two layers we cannot interleave.
///
/// All zero is "the server said nothing", which is also what a non-DV stream produces — see
/// [`Dovi::base_layer_unusable`] for why silence must never convict.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct Dovi {
    /// `DOVIPresent` — the file carries Dolby Vision at all. Everything below is meaningless
    /// without it, because a non-DV stream sends none of these fields and so reads as all-zero.
    pub(crate) present: bool,
    /// `DOVIProfile` — 5 / 7 / 8 …; 0 means the server did not say.
    pub(crate) profile: i64,
    /// `DOVIBLCompatID` — 0 none (P5) / 1 HDR10 / 2 SDR / 4 HLG. NB the dev server's P7 item
    /// sends **6**, so this field alone does not identify a dual-layer file.
    pub(crate) bl_compat: i64,
    /// `DOVIELPresent` — an enhancement layer is present (P7).
    pub(crate) el_present: bool,
    // ---- DESCRIPTIVE ONLY, below. The four fields above answer the PLAYBACK question this struct
    // exists for ("is the base layer a correct picture"); these three are read by
    // `ui::tracks_panel` and by nothing else. They live here rather than on `Detail` so there is
    // one home for "what the server said about Dolby Vision" — but do not reach for them in a
    // decision without the live sweep `plex::Stream`'s DOVI comment records.
    /// `DOVILevel` — the DV level (a bitrate/resolution tier); 0 = the server did not say.
    pub(crate) level: i64,
    /// `DOVIVersion`, DECOMPOSED — the server sends the dotted string `"1.0"`, and this holds it
    /// as `(1, 0)`. Rendered back by [`Dovi::version_str`].
    ///
    /// **Why not a `String`:** this struct is `Copy` and rides `route::Session`, whose `IDLE` is a
    /// `const`; more to the point `route::playback_preview_of` reads a `Dovi` **on the detail
    /// page's per-frame draw path**, so a `String` field would put a heap allocation in a hot
    /// frame — the same cost `ui::info_panel` refuses when it declines to clone `route::url()`.
    /// A DV version is dotted-numeric by specification, so the decomposition is lossless for every
    /// shape the field can take; anything unparseable lands as `(0, 0)` and draws no row at all.
    pub(crate) version: (i64, i64),
    /// `DOVIBLPresent` — the base layer is in the file.
    pub(crate) bl_present: bool,
    /// `DOVIRPUPresent` — the RPU (the dynamic metadata) is in the file.
    pub(crate) rpu_present: bool,
}

impl Dovi {
    /// The all-zero record — "the server said nothing about Dolby Vision", which is also exactly
    /// what an ordinary SDR file produces. A `const` rather than [`Default::default`] because
    /// `route`'s idle `Session` is a `const` item and cannot call one.
    pub(crate) const NONE: Dovi = Dovi {
        present: false,
        profile: 0,
        bl_compat: 0,
        el_present: false,
        level: 0,
        version: (0, 0),
        bl_present: false,
        rpu_present: false,
    };

    /// [`Self::version`] back as the dotted string the server sent (`"1.0"`), or `None` when it
    /// sent none. `None` and `Some("0.0")` are different answers and the panel draws only the
    /// former's absence — a server that really reported version 0 would still get a row.
    pub(crate) fn version_str(&self) -> Option<String> {
        (self.version != (0, 0)).then(|| format!("{}.{}", self.version.0, self.version.1))
    }

    /// Parse `DOVIVersion`'s dotted string into [`Self::version`]'s pair. PURE, and deliberately
    /// forgiving: a value this does not understand is `(0, 0)` — i.e. "the server said nothing" —
    /// because a version string is a caption and must never be a parse failure that costs the
    /// whole item.
    pub(crate) fn parse_version(s: &str) -> (i64, i64) {
        let s = s.trim();
        if s.is_empty() {
            return (0, 0);
        }
        let (a, b) = match s.split_once('.') {
            // a trailing ".0.0" (a three-part version) keeps its first two components
            Some((a, rest)) => (a, rest.split('.').next().unwrap_or("0")),
            None => (s, "0"),
        };
        match (a.trim().parse::<i64>(), b.trim().parse::<i64>()) {
            (Ok(a), Ok(b)) if a >= 0 && b >= 0 => (a, b),
            _ => (0, 0),
        }
    }

    /// PURE: **true when the base layer on its own is not a picture we can put on the panel
    /// correctly** — i.e. when this bitstream, decoded as ordinary HEVC by a pipeline that was
    /// never TOLD it is Dolby Vision, shows the user wrong colours (or nothing).
    ///
    /// Note the "never told" clause: it is the whole question this predicate asks, and since
    /// 2026-08-21 it is no longer the only thing we can do — [`Dovi::presentation`] can declare
    /// the stream to the pipeline, and a declared Profile 5 is displayed correctly. What survives
    /// unchanged is every path where the bitstream reaches a decoder with NO declaration attached,
    /// and that is now this predicate's job:
    ///
    /// - the direct-play gate **when no node will be sent** (`presentation` is written in terms of
    ///   this, so the two can never contradict each other), and
    /// - the server's permission to **COPY** the video — a remux or a `directStream` transcode
    ///   hands us the identical elementary stream one container down, and the Load payload built
    ///   for that path declares nothing. `route::build_stream` reads it for both
    ///   ([`crate::plex::TranscodeSpec::no_video_copy`] and the `remux` gate) and that is why a
    ///   declared Profile 5 still refuses a copy: the declaration rides the DIRECT PLAY, not the
    ///   file, so the same pixels arriving by another route are as wrong as they ever were.
    ///
    /// Two disqualifiers, and they are found by different fields:
    /// - **an enhancement layer** (`el_present`, Profile 7) — one elementary stream is all the
    ///   pipeline feeds, so the other layer simply never arrives. This is the only test that
    ///   catches P7: measured live 2026-08-21, the dev server's P7 item reports `bl_compat = 6`,
    ///   which sails through any `== 0` check.
    /// - **no cross-compatible base layer** (Profile 5 / `bl_compat == 0`) — IPT-PQ, which an
    ///   HEVC decoder will happily decode and a panel will happily show, incorrectly.
    ///
    /// **Silence must not convict, and that is the whole subtlety here.** Every field is 0 both
    /// when the server omits it and when there is no Dolby Vision at all, so a bare
    /// `bl_compat == 0` would refuse direct play for *every ordinary SDR file in the library*.
    /// `present` guards the outer question and a KNOWN `profile` guards the compat-id test, so a
    /// server that reports `DOVIPresent` and nothing else falls through to the existing gates
    /// unchanged. That direction is deliberate, and the price of getting it wrong is higher than
    /// it first looks: a true answer here does not merely reroute an item, it also withdraws the
    /// server's permission to COPY the video
    /// ([`crate::plex::TranscodeSpec::no_video_copy`] — without which the refusal accomplishes
    /// nothing at all), and a server that cannot encode the result then refuses the playback
    /// outright. So a false positive costs the film, not just its 4K and its HDR10. The same
    /// misread-degrades-to-assumed rule [`crate::route::video_direct_plays`] applies to an unknown
    /// frame size, applied to a field whose silence is indistinguishable from a legitimate zero.
    ///
    /// The one measured reassurance, and it is worth having before trusting the `profile > 0`
    /// guard: every Dolby Vision stream on the dev server (34 of them, swept 2026-08-21) sends all
    /// eight `DOVI*` keys together. No shape there reports a profile without also reporting a
    /// compatibility id, which is the only combination that guard could misread.
    pub(crate) fn base_layer_unusable(&self) -> bool {
        if !self.present {
            return false;
        }
        self.el_present || self.profile == 5 || (self.profile > 0 && self.bl_compat == 0)
    }

    /// PURE: **how this stream will be presented** — the ONE predicate behind both halves of the
    /// Dolby Vision decision, so they cannot drift apart. The direct-play gate
    /// ([`crate::route::video_direct_plays`]) asks it whether to refuse; the Load payload
    /// ([`crate::player::engine`]) asks the same value whether to emit a `DolbyHdrInfo` node. One
    /// call, one answer, two consumers.
    ///
    /// `signal` is whether we are willing to declare Dolby Vision for a stream **whose base layer
    /// we could not show without it** — i.e. Profile 5. It is TRUE in every shipping configuration
    /// now; `/tmp/plxnative-nodv` ([`dv_withheld`]) is the only thing that clears it, and it exists
    /// to bisect, not to protect. It stays a parameter rather than a read inside this function so
    /// the whole rule stays pure and both settings are unit-testable.
    ///
    /// The four arms, and why each is where it is:
    ///
    /// - **not `present`** → [`DvPresentation::NotDv`]. No Dolby Vision, nothing to say, and
    ///   nothing to refuse. This is every ordinary file in the library.
    /// - **`el_present`** → refuse, always, `signal` or not. Profile 7 splits its picture across a
    ///   base and an enhancement layer; the pipeline feeds ONE elementary stream and cannot
    ///   interleave the other, so no payload key makes it displayable. (This is also the only
    ///   thing that identifies a dual-layer file: the dev server's P7 reports `bl_compat = 6`.)
    ///   It is deliberately checked BEFORE the declaration arm — which is what keeps the emitted
    ///   node's `trackType` at `"single"` and, with `encryptionType` fixed at `"clear"`, makes the
    ///   pipeline's `dv-dual-svp` secure-video-path flag unreachable. We cannot satisfy that flag.
    /// - **no node will be sent** (a profile the server never named, or Profile 5 with the
    ///   trigger unarmed — see the comment on `declare`, which is where the trigger's reach
    ///   narrowed) → fall back to
    ///   exactly the pre-declaration rule: refuse iff [`base_layer_unusable`](Self::base_layer_unusable).
    ///   That is what makes "keep the refusal for any case where we would not send the node" a
    ///   property of the code rather than of a reviewer's memory. The `profile <= 0` half also
    ///   preserves the never-convict-on-silence rule: a server that reports `DOVIPresent` and
    ///   nothing else still falls through to `NotDv` and plays as it always has, because we cannot
    ///   tell that silence from an SDR file, and `getInt` wants a real profile id anyway.
    /// - otherwise → **declare it**, and direct play is then correct — including for Profile 5,
    ///   whose refusal this inverts. The decompile of this TV's own `libpf` (2026-08-21) is the
    ///   evidence: `CustomPipeline::parseOptionStringSpi` sets `hasDolbyHdrInfo` on the mere
    ///   PRESENCE of the key, and `getVideoCaps` then adds `dolby-vision=TRUE` (+ the profile
    ///   hint) to the `video/x-h265` caps it was already going to build. The codec string does not
    ///   change; the node is the entire difference between an IPT-PQ stream shown in wrong colours
    ///   and one the panel puts in Dolby Vision mode.
    pub(crate) fn presentation(&self, signal: bool) -> DvPresentation {
        if !self.present {
            return DvPresentation::NotDv;
        }
        if self.el_present {
            return DvPresentation::Refuse("dual-layer");
        }
        // **Whether a node will be sent, and the trigger is no longer the whole answer.** A
        // stream whose base layer is already a correct picture on its own declares
        // UNCONDITIONALLY; only one whose base layer is not — Profile 5 — waits behind
        // [`dv_withheld`]. **Both arms now declare in every shipping configuration**, and the
        // distinction survives only as a bisect and as the record of why it was ever needed.
        //
        // It was needed because a declared **P5 hitched** and a declared P8 did not: P5's display-
        // management lookup missed for 2 frames in every 12 (`Requested <pts> PTS can not be found
        // in LUT Buffer`), while a cross-compatible base layer masks the same fault — when the
        // dynamic metadata is late, an HDR10/HLG/SDR image that was already right is what shows.
        // That fault was **one 90 kHz tick** in the LUT key and is fixed
        // ([`crate::player::engine`]'s `pts_nudge_ns`): 3 misses in 90 s on the shipped default,
        // against 160 in 45 s before. So the asymmetry that put P5 behind a trigger is gone, and
        // with it the trigger's opt-in polarity.
        //
        // Written through [`base_layer_unusable`](Self::base_layer_unusable) rather than a bare
        // `profile != 5` so the two can no more drift apart here than they can in the gate below.
        let declare = signal || !self.base_layer_unusable();
        if !declare || self.profile <= 0 {
            return if self.base_layer_unusable() {
                DvPresentation::Refuse("no cross-compatible base layer")
            } else {
                DvPresentation::NotDv
            };
        }
        DvPresentation::Declare(DolbyHdrInfo {
            profile_id: self.profile,
            // Honest derivation, and unreachable as `"dual"` while the `el_present` arm above
            // returns first — written this way so that the day an interleaver exists, the payload
            // follows the refusal being relaxed instead of quietly lying about the track.
            track_type: if self.el_present { "dual" } else { "single" },
            // Never `"all"`: paired with `trackType: "dual"` that is what sets `dv-dual-svp`, the
            // secure-video-path flag, which this app cannot satisfy.
            encryption_type: "clear",
        })
    }

    /// [`presentation`](Self::presentation) with the trigger read for you — the form both real
    /// call sites use, so the gate and the payload are answered from one latched bool.
    pub(crate) fn presentation_now(&self) -> DvPresentation {
        self.presentation(!dv_withheld())
    }
}

/// The `option.externalStreamingInfo.contents.DolbyHdrInfo` node of the Starfish Load payload:
/// what we tell LG's pipeline about this stream's Dolby Vision.
///
/// The three fields are the ones the TV's own parser reads, at the paths and in the types the
/// decompile proved (`Options::checkKeyExistance` for the node itself, then `getInt` for
/// `profileId` and `getString` for the other two). `profileId` **must** be a JSON integer;
/// omitting it leaves the pipeline's `-1` sentinel, which still yields `dolby-vision=TRUE` with
/// only the profile hint missing — a legitimate fallback, not a failure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct DolbyHdrInfo {
    /// `DOVIProfile` as the server reported it. 5 (single-layer IPT-PQ) is the case this exists
    /// for; 8.x declares fine too and gains the dynamic metadata its base layer alone lacks.
    pub(crate) profile_id: i64,
    /// `"single"` or `"dual"` — one elementary stream or a base + enhancement pair.
    pub(crate) track_type: &'static str,
    /// `"clear"`. See [`Dovi::presentation`] for why this is never `"all"`.
    pub(crate) encryption_type: &'static str,
}

/// What [`Dovi::presentation`] decided: the single value the direct-play gate and the Load payload
/// both read. Three states rather than a bool, because "there is no Dolby Vision here" and "there
/// is, and we are declaring it" are the same answer to the GATE and opposite answers to the
/// PAYLOAD — which is precisely the pair that used to be two predicates and could disagree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DvPresentation {
    /// Not Dolby Vision, or not identifiably so. Play it as ordinary HEVC, declare nothing.
    NotDv,
    /// Direct play, with this node spliced into the Load payload.
    Declare(DolbyHdrInfo),
    /// Direct play is refused, with the short reason for the log line at the decision.
    ///
    /// "cross-compatible" rather than "HDR10" deliberately: HDR10 (`bl_compat` 1) is merely the
    /// common case, and an SDR (2) or HLG (4) base layer is equally displayable. What Profile 5
    /// lacks is a base layer conformant to ANY ordinary transfer, which is what id 0 means.
    Refuse(&'static str),
}

impl DvPresentation {
    /// Direct play is refused (and, at `build_stream`, the reason for the log line).
    pub(crate) fn refusal(&self) -> Option<&'static str> {
        match self {
            Self::Refuse(why) => Some(why),
            _ => None,
        }
    }
    /// The node to splice into the Load payload, if any.
    pub(crate) fn declared(&self) -> Option<DolbyHdrInfo> {
        match self {
            Self::Declare(d) => Some(*d),
            _ => None,
        }
    }
}

crate::dev::latched_flag!(
    /// `/tmp/plxnative-dvnonode` — with `-dv` also armed, keep the Dolby Vision **direct play**
    /// but send **no** `DolbyHdrInfo` node. Diagnostic only, and it exists for one question the
    /// trigger surface could not otherwise ask.
    ///
    /// The two things that changed together the day Profile 5 first direct-played are the
    /// DECLARATION and the 4K HEVC direct play of a file that had never been fed before. Every
    /// other combination is reachable by choosing a title — a Profile 8 direct-plays with the node
    /// or without it, because its gate does not depend on the trigger — but the P5 file has only
    /// two states, "declared and direct-played" and "refused", since [`Dovi::presentation`]
    /// answers both halves from one value. This is the missing cell: the same bytes, the same
    /// path, the declaration alone removed.
    ///
    /// Applied at the payload ([`crate::player::engine`]), NEVER at the gate — suppressing the
    /// node at the gate would send the file to the transcoder and measure a different pipeline.
    /// Note the resulting picture is expected to be WRONG (an IPT-PQ stream shown as ordinary
    /// HDR); this knob is for judging cadence, not colour.
    pub(crate) fn dv_node_suppressed = "dvnonode";
);

crate::dev::latched_flag!(
    /// `/tmp/plxnative-nodv` — **withhold the Dolby Vision declaration**, for a bisect. The
    /// polarity is inverted from what it was, and the inversion is the point.
    ///
    /// This was `/tmp/plxnative-dv`, an opt-IN, default off, with a note in this doc saying to
    /// flip the default "once the node has been seen to put a correct picture on a real panel".
    /// The reason for the caution was real — the payload is the `sourceInfo` envelope, which the
    /// pipeline parses before anything decodes, and a malformed one does not fail loudly, it
    /// wedges the video sink. The condition has now been met, twice over and on the last profile
    /// that had not met it:
    ///
    /// - **the picture is correct** — Profile 5 direct-played, photographed, with the set's own
    ///   "Dolby Vision / Dolby Atmos" read-out on screen;
    /// - **and its one measured defect is fixed.** A declared P5 used to lose the display-
    ///   management lookup for 2 frames in 12 (a ~2 Hz tone pulse); that was one 90 kHz tick in
    ///   the LUT key and is gone — 3 misses in 90 s on the shipped default, against 160 in 45 s.
    ///   See `player::engine::pts_nudge_ns`.
    ///
    /// So every Dolby Vision stream the pipeline can feed is now declared by default, and this
    /// knob only takes it away. Note what it does NOT do: withholding the declaration re-imposes
    /// the old refusal on Profile 5 (`base_layer_unusable`), so this bisects "declared vs
    /// transcoded", not "declared vs direct-played-undeclared". [`dv_node_suppressed`]
    /// (`/tmp/plxnative-dvnonode`) is the finer instrument for that, and is why both exist.
    ///
    /// Latched once per process rather than read per call, which is what guarantees the gate and
    /// the payload cannot disagree WITHIN a session: `tests/run.py` clears `/tmp/plxnative-*`
    /// between cases, so an unlatched read could legitimately answer differently at the route
    /// decision and at the Load a few frames later, and direct-play a Profile 5 with no node.
    pub(crate) fn dv_withheld = "nodv";
);

// `Default` is for TESTS: every field is a zero/empty that means "PMS did not say", so a fixture
// can name the two or three fields its case is about instead of the fifteen it is not.
#[derive(Clone, Default)]
pub(crate) struct Stream {
    pub(crate) id: i64, // Plex stream id (for &audioStreamID / &subtitleStreamID)
    pub(crate) index: i64, // PMS stream index (container order) — the ordinal mapping sorts by it
    pub(crate) lang: String,      // display name ("English")
    pub(crate) lang_code: String, // ISO code ("eng") — the route's language preference matches this
    pub(crate) codec: String,
    pub(crate) channels: i64,
    pub(crate) layout: String, // audioChannelLayout, e.g. "5.1(side)"
    /// Per-STREAM bitrate in kbps (0 = the server did not say) — NOT the file's. It is what tells
    /// seven same-language AC3 tracks apart in `ui::tracks_panel`, where language and codec alone
    /// cannot.
    pub(crate) bitrate: i64,
    /// The codec profile, lower-case as PMS sends it. **On an audio track this is where Atmos
    /// is** — `"dolby digital plus + dolby atmos"` (probed live 2026-08-21); on the video track it
    /// is `"main 10"`. See [`Stream::has_atmos`].
    pub(crate) profile: String,
    /// Video track only: bits per component (10 for Main 10); 0 = not said.
    pub(crate) bit_depth: i64,
    /// Video track only: chroma subsampling as PMS spells it, e.g. `"4:2:0"`.
    pub(crate) chroma: String,
    pub(crate) title: String,
    pub(crate) sdh: bool,
    pub(crate) ad: bool,
    pub(crate) forced: bool,
    pub(crate) default: bool, // the file's default track (drives the "Original:" audio label)
    /// external/sidecar stream (downloaded .srt etc. — NOT inside the container). The client
    /// renderer can't reach it on direct-play; only a server transcode can burn it.
    pub(crate) external: bool,
    /// PMS `Stream.selected` — the server's CURRENT pick for this part, i.e. the track a user
    /// chose on ANY Plex client (phone, web, another TV) and the one `select_streams` writes.
    /// `route`'s selection ladder prefers it over its own defaults, which is what makes a pick
    /// made elsewhere survive here instead of being silently overwritten. NB for AUDIO the server
    /// marks a selected stream on essentially every part — for an untouched one that is just the
    /// container `default` echoed back — so `route::pick_dp_audio` only treats it as a choice when
    /// it names a DIFFERENT stream, and never as a reason to transcode. Read its doc before using
    /// this flag anywhere else.
    pub(crate) selected: bool,
}

impl Stream {
    /// Does this audio track carry **Dolby Atmos**?
    ///
    /// **The answer is in `profile`, and only there** — probed live against the dev server
    /// 2026-08-21. The Atmos track on the P5 test item sends
    /// `profile: "dolby digital plus + dolby atmos"` while its `audioChannelLayout` is the
    /// ordinary `"5.1(side)"` and its `title` is `null`. A client that looked at the layout, the
    /// title or the channel count would badge nothing, forever and silently. (PMS *also* composes
    /// it into `displayTitle`, but that is a pre-formatted user string in the server's own words;
    /// the profile is the structured field.) Dolby's own AC-4 spec §3.1.1.1 says the same thing
    /// from the other end — *"It is not possible to derive whether content is branded as Dolby
    /// Atmos by inspecting the channel configuration."*
    ///
    /// Two consumers, and they are why this lives here rather than in the panel that first needed
    /// it: the track menu's `EAC3 5.1 + Atmos` detail line, and the Load payload's
    /// `contents.immersive` node ([`crate::route::stream_immersive`]) — one is a caption and the
    /// other is a statement to the television's pipeline, so the predicate has to be the data
    /// layer's, not a screen's.
    ///
    /// Deliberately a substring test rather than an equality: the field is a human-readable
    /// composition and the codec half of it varies (`"dolby digital plus + dolby atmos"` here,
    /// but TrueHD and AC-4 compose the same way). "atmos" is the part that means Atmos.
    pub(crate) fn has_atmos(&self) -> bool {
        self.profile.to_ascii_lowercase().contains("atmos")
    }
}

#[derive(Default)]
pub(crate) struct Episode {
    pub(crate) rk: String,
    pub(crate) index: i64,   // episode number
    pub(crate) season: i64,  // parentIndex
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) aired: String, // originallyAvailableAt
    pub(crate) dur_ms: i64,
    pub(crate) thumb: String,
    pub(crate) resume_ms: i64, // viewOffset (0 = not started)
    /// `viewCount ≥ 1` — played through at least once. Deliberately INDEPENDENT of `resume_ms`
    /// on the wire: PMS keeps both on an episode that was finished and then started again, so
    /// which of the two a tile shows is a presentation rule at the draw site (see
    /// `ui/detail.rs`'s filmstrip), not a mutual exclusion the data layer can assume.
    pub(crate) watched: bool,
    pub(crate) part: String,   // Media[0].Part[0].key (to play)
    pub(crate) rating: String,
    pub(crate) vcodec: String, // Media[0].videoCodec (for the direct-play/transcode decision)
    pub(crate) acodec: String, // Media[0].audioCodec
}

// Deliberately NOT `Default`: every construction site spells every field, so adding one to a
// season is a compile error at each of them rather than a silent zero (the counts below are
// exactly the kind of field that reads as a legitimate value when it defaults).
pub(crate) struct Season {
    pub(crate) rk: String,
    pub(crate) index: i64,
    pub(crate) title: String,
    /// episodes in this season (`leafCount`); 0 when the server sent no count
    pub(crate) leaf_count: i64,
    /// how many of those are watched (`viewedLeafCount`)
    pub(crate) viewed_leaf_count: i64,
}

impl Season {
    /// Every episode of this season is watched — the season-scope form of the container rule
    /// `fetch_detail` applies to a show (`viewed >= leaf && leaf > 0`). The `leaf_count > 0` half is
    /// load-bearing: a season the server sent no counts for is `0 >= 0`, which would otherwise read
    /// as watched.
    ///
    /// **No caller outside its own tests.** This doc used to claim the season tab's tick read it —
    /// that tick does not exist, and the two sites that DO spell the rule out (`fetch_detail`'s
    /// container test below, `pms::unwatched`) hold a `PmsMovie`, not a `Season`, so neither can
    /// call it. Kept for the "Mark Season Watched" row, and because the tests below are where the
    /// `leaf_count > 0` guard is actually written down.
    #[allow(dead_code)]
    pub(crate) fn watched(&self) -> bool {
        self.leaf_count > 0 && self.viewed_leaf_count >= self.leaf_count
    }
}

/// A tile of the Related shelf — the **shared catalog row**, not a private three-field struct.
///
/// It used to be `{ rk, title, thumb }`: the poster art and nothing about the item. That shortfall
/// was load-bearing in two visible ways. The shelf could draw no watched tick and no resume bar,
/// while every other poster surface in the app draws both; and the press-and-hold context menu had
/// nothing to build rows from, so a hold on a Related tile did nothing at all — the owner-reported
/// gap — while the same hold on Home, the Library grid, Search and a person's filmography opened a
/// menu.
///
/// **The data was never missing.** `/related`'s rows are the SAME wire DTO every other listing
/// parses, carrying `viewCount`, `viewOffset`, `duration`, `type` and `Media[0].Part[0]`;
/// `fetch_related` simply copied three fields out and dropped the rest. So the fix is not to widen
/// this struct field by field but to stop having one: [`crate::pms::parse_item`] is the ONE
/// `plex::Metadata` → row mapping that the hub catalog, the Library grid and the person page
/// already share, and it owns rules a re-derivation gets wrong. The sharpest is that a related
/// **SHOW** is watched on `viewedLeafCount >= leafCount` and never on `viewCount > 0`, so a series
/// you are three episodes into is neither watched nor unwatched — which is exactly the state whose
/// menu must offer BOTH write verbs (`ui::widgets::row_watch_state`).
///
/// Carrying `sid` is the second thing this buys, and it is a correctness property rather than a
/// convenience: a related item is a key on the server THIS PAGE is mounted on, and both servers
/// number their ratingKeys from 1 (`docs/shared-servers.md` §2). `fetch_related` stamps each row
/// with the sid it fetched from, so every downstream use — the art request, the context menu's
/// `SID`, the scrobble — addresses the right machine BY CONSTRUCTION rather than by a comment
/// asking the next caller to remember `plex::current_server()` is the wrong answer here.
pub(crate) type Related = crate::pms::PmsMovie;

/// Clone because the playing-item store keeps the played leaf's OWN chapters (see [`PlayingItem`]) —
/// on the detail-page play path they are cloned from the already-loaded `Detail` rather than refetched.
#[derive(Clone)]
pub(crate) struct Chapter {
    pub(crate) index: i64,    // 1-based chapter number
    pub(crate) start_ms: i64, // startTimeOffset — the seek target + timestamp label
    pub(crate) title: String, // Chapter.tag; empty → UI shows "Chapter {index}"
    pub(crate) thumb: String, // server image path → resolve_tex (empty if no chapter thumbs)
}

/// Parse an item's `Chapter[]` into the app's model — the ONE `plex::Chapter` → [`Chapter`] mapping,
/// shared by the detail parse and the playing-item store (which must agree: the Chapters strip seeks
/// with these offsets, so two mappings is two chances to disagree about which item they describe).
fn convert_chapters(chapters: &[crate::plex::Chapter]) -> Vec<Chapter> {
    chapters
        .iter()
        .map(|c| Chapter {
            index: c.index,
            start_ms: c.start_time_offset,
            title: c.tag.clone(),
            thumb: c.thumb.clone(),
        })
        .collect()
}

/// Which timeline segment a [`Marker`] describes. Only the two the player acts on are modelled —
/// PMS also emits `commercial` on recorded content, which [`convert_markers`] drops, so an
/// unhandled kind can never be mistaken for one of these.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MarkerKind {
    Intro,
    Credits,
}

/// A server-detected intro / credits segment of the playing item (`?includeMarkers=1`). Drives
/// the in-player Skip prompt and — for an episode with something queued after it — the moment the
/// Up Next control takes over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Marker {
    pub(crate) kind: MarkerKind,
    pub(crate) start_ms: i64,
    pub(crate) end_ms: i64,
    /// this credits segment runs to the end of the item (PMS `final: true`)
    pub(crate) final_seg: bool,
}

/// Parse a leaf's `Marker[]` into the app's model, dropping kinds the player has no behaviour for
/// and any segment whose offsets are not a forward range (a zero-length or inverted marker would
/// otherwise produce a prompt that can never be satisfied by seeking to its end).
fn convert_markers(markers: &[crate::plex::Marker]) -> Vec<Marker> {
    markers
        .iter()
        .filter_map(|m| {
            let kind = match m.kind.as_str() {
                "intro" => MarkerKind::Intro,
                "credits" => MarkerKind::Credits,
                _ => return None,
            };
            (m.end_time_offset > m.start_time_offset && m.start_time_offset >= 0).then_some(Marker {
                kind,
                start_ms: m.start_time_offset,
                end_ms: m.end_time_offset,
                final_seg: m.is_final != 0,
            })
        })
        .collect()
}

/// Segments the user has already skipped in THIS playback, identified by kind + start (a leaf has
/// at most an intro and a credits, so this never grows past two).
static mut SKIPPED: Vec<(MarkerKind, i64)> = Vec::new();

/// Record that `m` has been skipped, so it is never offered again for this item.
///
/// This is what makes skipping terminal, and it is not belt-and-braces. `av_seek_frame` is called
/// with `AVSEEK_FLAG_BACKWARD` (`ff.rs`), so it lands on the keyframe **at or before** the target —
/// seeking to a marker's `end_ms` therefore resumes a few seconds INSIDE the segment, whose keyframe
/// spacing is the file's, not ours. Without this latch the button reappeared moments after the skip
/// and pressing it seeked to the same place again: press → jump back a little → press → forever.
/// Padding the seek target cannot fix that (keyframe intervals vary from 2 s to 10 s); refusing to
/// re-offer a segment the user has already dismissed can, and is what they meant by the press.
pub(crate) fn mark_skipped(m: Marker) {
    let key = (m.kind, m.start_ms);
    unsafe {
        let v = &mut *addr_of_mut!(SKIPPED);
        if !v.contains(&key) {
            v.push(key);
        }
    }
}

/// The segment the playhead is inside right now, or None — the ONE live "what am I in" read, so
/// no UI module has to re-derive it (and none has to ask another module a question about it).
///
/// Gated on `is_playing()`: through the whole pre-roll (Connecting/Buffering/Seeking) `playpos_ns`
/// is still 0 or frozen at a seek target, and an item whose intro starts at 0 would otherwise
/// report a segment during every load. Segments already skipped are filtered out — see
/// [`mark_skipped`].
pub(crate) fn active_marker() -> Option<Marker> {
    if !crate::player::is_playing() {
        return None;
    }
    let m = marker_at(playing_markers(), crate::player::playpos_ns() / 1_000_000)?;
    let skipped = unsafe { &*addr_of!(SKIPPED) };
    (!skipped.contains(&(m.kind, m.start_ms))).then_some(m)
}

/// The last stretch of an episode counts as its credits when the server never said where the
/// credits are. Credits DETECTION is a Plex Pass feature: on a server without one, no item ever
/// carries a credits marker, so the Up Next tile — armed exclusively off that marker — could
/// never appear, and binge-watching ended every episode by dropping the user back to the detail
/// page (found by the Plex Pass dependency audit after issue #22). Synthesizing the segment
/// reuses the entire existing chain — tile, countdown, cancel latch, HUD hold — instead of
/// growing a parallel EOS path.
///
/// Deliberately narrow: only when a successor EXISTS (a movie's tail must not grow a Skip
/// Credits pill pointing nowhere), only when the item carries no credits marker AT ALL (a server
/// that said "credits start at 41:03" must not be second-guessed at 30s-before-end), and only
/// when the item is long enough that its tail is clearly an ending (> 3x the window, so a short
/// clip does not spend a third of its runtime offering the next one).
pub(crate) const TAIL_WINDOW_MS: i64 = 30_000;
pub(crate) fn synthesized_tail_marker(has_next: bool) -> Option<Marker> {
    if !has_next || !crate::player::is_playing() {
        return None;
    }
    if playing_markers().iter().any(|m| m.kind == MarkerKind::Credits) {
        return None;
    }
    let dur_ms = crate::player::duration_ns() / 1_000_000;
    let pos_ms = crate::player::playpos_ns() / 1_000_000;
    tail_marker(pos_ms, dur_ms)
}

/// The pure half of [`synthesized_tail_marker`] — the window geometry alone, host-testable.
pub(crate) fn tail_marker(pos_ms: i64, dur_ms: i64) -> Option<Marker> {
    if dur_ms < TAIL_WINDOW_MS * 3 {
        return None;
    }
    let start_ms = dur_ms - TAIL_WINDOW_MS;
    (pos_ms >= start_ms)
        .then_some(Marker { kind: MarkerKind::Credits, start_ms, end_ms: dur_ms, final_seg: true })
}

/// The marker containing `pos_ms`, if any — the ONE "am I inside a skippable segment" rule, shared
/// by the skip prompt and the end-of-episode handoff so they can never disagree about where a segment
/// begins. The range is half-open (`start <= pos < end`) so the prompt clears itself the instant a
/// skip lands on `end_ms` rather than re-offering the segment it just left.
///
/// A `final` credits marker is treated as running to `i64::MAX` rather than its stated `end_ms`:
/// PMS sets that end to the container duration, but our playhead is the DECODER's, which routinely
/// stops a few hundred ms short of it — so the prompt would blink out over the last frames.
pub(crate) fn marker_at(markers: &[Marker], pos_ms: i64) -> Option<Marker> {
    markers
        .iter()
        .find(|m| {
            let end = if m.final_seg { i64::MAX } else { m.end_ms };
            pos_ms >= m.start_ms && pos_ms < end
        })
        .copied()
}

/// Which badge artwork a `Rating.image` string names — the provider AND its icon state, both of
/// which the server encodes in that one string. Rotten Tomatoes has **five** states:
/// `rottentomatoes://image.rating.ripe` is the fresh tomato, `…rating.certified` the Certified
/// Fresh one, `…rating.rotten` the green splat, `…rating.upright` the standing popcorn bucket and
/// `…rating.spilled` the tipped one; `imdb://image.rating` and `themoviedb://image.rating` carry no
/// state because those providers have only one mark.
///
/// The art is chosen by parsing that string and **never** by comparing `value` to a threshold:
/// Rotten Tomatoes' critic and audience cutoffs differ from each other and move, so a 6.0 can be
/// fresh on one axis and rotten on the other — the server already knows which, and says so here.
/// The PROVIDER likewise comes from the URI scheme and never from `Rating.type`: IMDb and TMDB both
/// arrive as `audience`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RatingArt {
    TomatoFresh,
    /// Certified Fresh — a distinct Rotten Tomatoes mark (the wreathed tomato), not a synonym for
    /// [`RatingArt::TomatoFresh`]. It is a rarer, higher bar than plain fresh, the server takes the
    /// trouble to name it, and it is the one state whose art we would otherwise be inventing by
    /// substitution. Folding it onto the plain tomato threw that away silently.
    TomatoCertified,
    TomatoRotten,
    PopcornUpright,
    PopcornSpilled,
    Imdb,
    Tmdb,
}

impl RatingArt {
    /// Parse `provider://image.rating[.state]`. The state is the **last dot-separated segment**,
    /// which is how Plex's own web bundle reads it (`t.substr(t.lastIndexOf(".") + 1)`) — so a
    /// state we have never seen still lands on the right arm instead of on a prefix match.
    ///
    /// An unknown provider — or a Rotten Tomatoes string with no state we recognise, which leaves
    /// tomato-vs-popcorn genuinely undetermined — yields `None`: a score whose artwork cannot be
    /// attributed is not badged at all, rather than badged with a guess.
    pub(crate) fn from_image(image: &str) -> Option<RatingArt> {
        let (provider, rest) = image.split_once("://")?;
        // "image.rating.ripe" → "ripe"; a stateless "image.rating" → "rating", which matches no arm.
        // A path with no dot at all yields "" here where Plex yields the whole URI — both match
        // nothing, which is the answer that matters.
        let state = rest.rsplit_once('.').map(|(_, s)| s).unwrap_or("");
        match provider {
            "rottentomatoes" => match state {
                "ripe" => Some(RatingArt::TomatoFresh),
                "certified" => Some(RatingArt::TomatoCertified),
                "rotten" => Some(RatingArt::TomatoRotten),
                "upright" => Some(RatingArt::PopcornUpright),
                "spilled" => Some(RatingArt::PopcornSpilled),
                _ => None,
            },
            "imdb" => Some(RatingArt::Imdb),
            "themoviedb" | "tmdb" => Some(RatingArt::Tmdb),
            _ => None,
        }
    }

    /// Display order of the badge row — **IMDb, then Rotten Tomatoes' critic tomato, then its
    /// audience popcorn, then TMDB** (`Details Screen.dc.html`). Fixed here (rather than left as wire
    /// order) so the row reads the same on every item; PMS returns the array alphabetically by
    /// provider. All three tomato states share one rank because they are one SLOT — the critic
    /// verdict — in three moods.
    ///
    /// IMDb leads because it is the score most viewers hold a reference for: an 8.1 out of 10 needs
    /// no calibration, where a tomato percentage is only meaningful once you know which side of RT's
    /// cutoff it fell. It also puts the row's two /10-vs-% unit changes at the ends rather than
    /// adjacent in the middle. Reordering this reorders the hero on every item, so it belongs in one
    /// place: here, not at the draw site.
    fn rank(self) -> u8 {
        match self {
            RatingArt::Imdb => 0,
            RatingArt::TomatoFresh | RatingArt::TomatoCertified | RatingArt::TomatoRotten => 1,
            RatingArt::PopcornUpright | RatingArt::PopcornSpilled => 2,
            RatingArt::Tmdb => 3,
        }
    }

    /// The provider's name, **as the provider sets it** — the row spells this in words instead of
    /// drawing anyone's mark, so it is the only thing identifying the source and it has to be
    /// right. `IMDb`, not `IMDB`.
    ///
    /// It is also the GROUPING key: Rotten Tomatoes' critic and audience scores are two readings
    /// from one source, so they share one caption and sit under it as a pair, while IMDb and TMDB
    /// are a caption and a number each. Equal names group; [`RatingArt::rank`] already orders the
    /// five RT states adjacently, so grouping is a run-length pass over the sorted list and never
    /// needs to reorder anything.
    pub(crate) fn provider(self) -> &'static str {
        match self {
            RatingArt::Imdb => "IMDb",
            RatingArt::TomatoFresh
            | RatingArt::TomatoCertified
            | RatingArt::TomatoRotten
            | RatingArt::PopcornUpright
            | RatingArt::PopcornSpilled => "ROTTEN TOMATOES",
            RatingArt::Tmdb => "TMDB",
        }
    }
}

/// One review score to badge on the detail hero: the artwork the server named, the score as PMS
/// normalises it (0–10 for every provider — a 91% tomato arrives as 9.1), and whether PMS filed it
/// as a critic or an audience score.
pub(crate) struct Rating {
    pub(crate) art: RatingArt,
    pub(crate) value: f64,
    pub(crate) critic: bool,
}

/// Build the badge list from an item's review scores. `Rating[]` wins whenever it is present: it is
/// the superset AND the only form carrying per-score provider identity.
///
/// The flat `rating`/`audienceRating` pair is the fallback for a response that omits the array.
/// **Today's only caller is `fetch_detail`, and `/library/metadata/{rk}` always sends the array**,
/// so that branch is reached by nothing but its test right now — it is here because the OTHER
/// shape is already on the wire and already needed: a section listing sends the flat pair and no
/// `Rating[]` at all (verified live 2026-07-29), so the moment a grid or the home hero wants a
/// score, this is the function it will call. In that branch the SLOT is the critic/audience
/// distinction — that is precisely what the two field names mean — since a flat row has no `type`.
///
/// Rows whose artwork cannot be attributed are dropped, as are non-positive scores: PMS omits a
/// score it does not have, and `de_f64` defaults that to 0.0, so "0.0" means absent, not zero.
fn convert_ratings(it: &crate::plex::Metadata) -> Vec<Rating> {
    let mut out: Vec<Rating> = if !it.ratings.is_empty() {
        it.ratings
            .iter()
            .filter_map(|r| {
                Some(Rating {
                    art: RatingArt::from_image(&r.image)?,
                    value: r.value,
                    critic: r.kind == "critic",
                })
            })
            .filter(|r| r.value > 0.0)
            .collect()
    } else {
        [
            (it.rating_image.as_str(), it.rating, true),
            (it.audience_rating_image.as_str(), it.audience_rating, false),
        ]
        .into_iter()
        .filter_map(|(img, value, critic)| Some(Rating { art: RatingArt::from_image(img)?, value, critic }))
        .filter(|r| r.value > 0.0)
        .collect()
    };
    // Rank orders the marks; `critic` breaks a tie inside one rank, so if a provider ever sends
    // both a critic and an audience score behind the SAME mark, the critic one still leads. Stable
    // sort, so anything these two keys don't separate keeps its wire order.
    out.sort_by_key(|r| (r.art.rank(), !r.critic));
    // ONE badge per slot. Two rows at the same rank is a contradiction the row cannot draw — a
    // ripe AND a rotten critic score, or the two flat fields both naming IMDb (the OpenAPI spec's
    // own example puts `imdb://image.rating` in `audienceRatingImage`) — and two identical marks
    // carrying different numbers is worse than one. The sort already put the one to keep first.
    out.dedup_by_key(|r| r.art.rank());
    out
}

#[derive(Default)]
pub(crate) struct Detail {
    /// WHICH SERVER this item was fetched from — the other half of its identity. `rk` on its own
    /// names an item on no machine in particular the moment a shared server is registered (both
    /// number from 1; docs/shared-servers.md §2), and every equality test that reads this struct
    /// therefore compares the pair through [`crate::plex::same_item`]: `cached_playing`'s cache
    /// hit, `pump_season`'s ownership test, `detail::reselect`, and the BACK trail's node.
    pub(crate) sid: crate::plex::ServerId,
    pub(crate) rk: String,
    /// Which SERVER this item was fetched from, as the OWNER'S HANDLE ("friend") — empty whenever
    /// it came from the signed-in user's own server, which is every item today.
    ///
    /// The person, never the machine: the machine's name (`nas-home`) belongs to the Sources list
    /// and to a failure read-out, and appears nowhere else in the product. Empty is the ABSENCE of
    /// an attribution, not an empty one — the detail hero draws no separator and no run at all for
    /// it ([`crate::ui::detail`]'s facts row), so a single-server library pays nothing for the
    /// feature: no gap, no dot, no draw call.
    ///
    /// Captured at FETCH time and stored, rather than read from `plex::servers::current()` at paint
    /// time: the page outlives the fetch, and the current server can move under it while a load is
    /// in flight. Populated by the multi-server data layer when it lands (`docs/shared-servers.md`
    /// step 2 threads `ServerId` through this struct); the roster field behind it is
    /// `plex::account::Resource::source_title`.
    pub(crate) source: String,
    /// The item's PORTABLE identity (`plex://movie/…`) — the same string on every server that
    /// matched this film, and the only one that is. It is what "Also available" asks the other
    /// sources about, because their copy has a different `rk` and may even have a different title:
    /// measured across this household's two servers, one film is `2029` here and `5274` there,
    /// under a Russian title. Empty when the server sent none, which simply means no cross-source
    /// lookup is possible for this item.
    pub(crate) guid: String,
    pub(crate) is_show: bool,
    pub(crate) kind: String,       // this item's own type: movie | episode | show | season
    pub(crate) show_title: String, // grandparentTitle — the show name, when this item is an episode
    pub(crate) show_rk: String,    // grandparentRatingKey — the show's rk (episode → its show)
    pub(crate) season: i64,        // parentIndex — season number, when an episode
    pub(crate) index: i64,         // index — episode number, when an episode
    pub(crate) title: String,
    pub(crate) year: i64,
    pub(crate) rating: String, // contentRating
    pub(crate) summary: String,
    /// The marketing one-liner (`tagline`) — *"Everyone deserves the chance to fly."*
    ///
    /// **Atmosphere, never content**, which is why it is drawn only in the About alert
    /// ([`crate::ui::about_panel`]) under the synopsis and nowhere on the page itself: it says
    /// nothing a viewer needs in order to decide, so it earns a line only where there is room to
    /// read the whole record. Empty for most items and for every episode — absence is the ordinary
    /// case, and the panel drops the line AND its gap rather than reserving a hole.
    ///
    /// It arrives on the SINGLE-key `/library/metadata/{rk}` fetch, which asks for no field
    /// exclusions. Both of the other reads in `plex/library.rs` pass
    /// `excludeFields=summary,tagline` — the batched `metadata_many` and the section listing — so
    /// anything derived from THOSE has never seen it and never will.
    pub(crate) tagline: String,
    pub(crate) aired: String,
    pub(crate) dur_ms: i64,
    pub(crate) resume_ms: i64, // viewOffset (0 = not partially watched) — the resume position
    pub(crate) watched: bool,  // movie: viewCount ≥ 1; show: viewedLeafCount ≥ leafCount
    pub(crate) part: String,   // Media[0].Part[0].key for a leaf (movie/episode); empty for a show
    pub(crate) vcodec: String, // Media[0].videoCodec (drives the direct-play/transcode decision)
    pub(crate) acodec: String, // Media[0].audioCodec
    pub(crate) video_fps: f64, // video Stream frameRate (0 = unknown); feeds the Load esInfo
    // ---- the PRIMARY version's technical fields (plex::Metadata::primary_media = Media[0], NOT a
    // best-of pick — a multi-version item has more, and choosing among them needs a version picker
    // that does not exist yet). For a SHOW these are borrowed from its first episode, like the
    // About footer's audio/subtitle lists. `bitrate`/`width`/`height` are unused by the UI today
    // and carried for the video-quality ladder ("26.1 Mbps 4K (Original)").
    pub(crate) video_resolution: String, // "4k" | "1080" | "720" | "sd" — the hero's media badge
    pub(crate) width: i64,               // stored frame size, not the resolution class (1918x802
    pub(crate) height: i64,              // is a 1080p scope movie) — badge off video_resolution
    pub(crate) bitrate: i64,             // kbps, whole-stream
    // ---- the rest of the primary version's technical record, added for `ui::tracks_panel` — and
    // since 2026-08-23 ON THE ROUTING PATH too: `route::source_kbps` takes `video`'s own bitrate
    // from here to judge a source against the user's quality ceiling, preferring it over the
    // whole-file `bitrate` above precisely so a rung does not bite one AC-3 track early. So this
    // block is no longer only the inspector's, and dropping the backfill would silently move every
    // rung's threshold with a green suite. Same caveat as the block above: version 0, not a
    // best-of pick.
    /// `Part[0].container`, falling back to `Media[0].container` — `"mp4"`, `"mkv"`, ….
    pub(crate) container: String,
    /// `Part[0].file` — the part's absolute path ON THE SERVER. Shown as the Track-information
    /// panel's header line. Not a URL, not reachable from here, and the one field on `Detail` most
    /// likely to be non-ASCII, so elide it by CHARACTER.
    pub(crate) file: String,
    /// `Part[0].size` in BYTES (0 = the server did not say).
    pub(crate) size: i64,
    /// `Media[0].aspectRatio` as a number — `2.35` (0.0 = not said).
    pub(crate) aspect_ratio: f64,
    /// The primary version's VIDEO track, whole. `vcodec`/`width`/`height`/`bitrate` above are the
    /// Media-level summary the play path reads; this is the stream's own record, and the only
    /// place its profile, bit depth, chroma and per-stream bitrate live. `None` for a show
    /// container that never got an episode backfill, and for an audio-only part.
    pub(crate) video: Option<Stream>,
    /// the video stream is HDR (PQ/HLG transfer or Dolby Vision) — with [`Self::hdr`] true AND
    /// the item facing a real RE-ENCODE (`route::Preview::Converts`, **not** merely "not
    /// direct-playable": a container-only remux copies the picture and keeps HDR10 intact) AND the
    /// server known Pass-less, the facts row warns that the transcode will be HDR→SDR without
    /// tone-mapping (a Plex Pass server feature; see docs/plex-pass-audit.md). Any weaker
    /// combination shows nothing.
    pub(crate) hdr: bool,
    /// The video stream's Dolby Vision layering. [`Self::hdr`] answers "should the facts row warn
    /// about tone mapping"; this answers the harder one the PLAY path needs — whether the base
    /// layer alone is a correct picture, i.e. whether direct play is honest for this file. Read by
    /// [`crate::route::playback_preview_of`] so the page's "how this plays" answer agrees with
    /// what Play will actually do.
    pub(crate) dovi: Dovi,
    pub(crate) art: String,
    pub(crate) thumb: String,
    /// The item's own `UltraBlurColors` corners (tl, tr, br, bl — the ring order
    /// [`plex::UltraBlurColors::corners`](crate::plex::UltraBlurColors::corners) owns) and whether
    /// the server sent a usable envelope — what keys the detail page's ambient GROUND. It lives on
    /// the LOADED item and not only on the catalog row because a page opened from the Library grid,
    /// a Related tile or the person page is never in the home catalog (`pms::index_of_rk` searches
    /// the hubs only), and those were exactly the pages sitting on flat grey with no wash at all.
    pub(crate) blur: [[f32; 3]; 4],
    pub(crate) has_blur: bool,
    pub(crate) genres: Vec<String>,
    pub(crate) countries: Vec<String>,
    pub(crate) cast: Vec<Cast>,
    /// Director[] tags, in server order — the hero's "Directed by …" line.
    pub(crate) directors: Vec<String>,
    /// Director[] + Writer[] as credits (each carrying its JOB in `role`), drawn on the Cast &
    /// Crew shelf AFTER the actors. Kept apart from `cast` so "who acted" stays answerable; the
    /// shelf addresses both through [`Detail::credit`].
    pub(crate) crew: Vec<Cast>,
    pub(crate) audio: Vec<Stream>,
    pub(crate) subs: Vec<Stream>,
    pub(crate) seasons: Vec<Season>,   // shows only
    pub(crate) episodes: Vec<Episode>, // the currently-selected season
    /// SHOWS: the episode the SERVER says is next to watch (`OnDeck`, one request, no extra round
    /// trip). Show-level and therefore **independent of the selected season tab**, which is the whole
    /// reason it is here: `episodes` above holds one season, so a next-episode the client worked out
    /// itself changed every time you browsed to another tab. `None` for a movie, for a show with
    /// nothing on deck (never started, or finished), and on any server that omitted the hub.
    pub(crate) on_deck: Option<Episode>,
    pub(crate) cur_season: usize,
    pub(crate) related: Vec<Related>,
    pub(crate) chapters: Vec<Chapter>,
    pub(crate) markers: Vec<Marker>, // intro / credits segments (leaf items only)
    pub(crate) ratings: Vec<Rating>, // review scores, critic-first (see convert_ratings)
}

impl Detail {
    /// How many tiles the Cast & Crew shelf holds: every actor, then every crew credit.
    pub(crate) fn credits_len(&self) -> usize {
        self.cast.len() + self.crew.len()
    }
    /// The `i`th shelf credit in that one flat index space, so the screen's focus/geometry can
    /// address a tile by position without re-deriving which vec it fell in.
    pub(crate) fn credit(&self, i: usize) -> Option<&Cast> {
        if i < self.cast.len() {
            self.cast.get(i)
        } else {
            self.crew.get(i - self.cast.len())
        }
    }
}

// The one loaded detail item (the detail page shows a single item at a time).
static mut CURRENT: Option<Detail> = None;

/// the currently-loaded detail item, or None
pub(crate) fn current() -> Option<&'static Detail> {
    unsafe { (*addr_of!(CURRENT)).as_ref() }
}
/// TEST-ONLY installer for the loaded item. The UI's layout tests need a `Detail` on screen with
/// no PMS behind them; every other writer of `CURRENT` goes through the landing mailbox, which is
/// exactly the invariant those tests must not have to fake. Crate-global, so callers hold
/// [`crate::testlock::serial`].
#[cfg(test)]
pub(crate) fn install_for_test(d: Option<Detail>) {
    unsafe { *addr_of_mut!(CURRENT) = d }
}

/// **OPTIMISTIC**, MAIN THREAD: flip what the LOADED item says about `(sid, rk)`'s watch state,
/// before the server has been told. Returns whether anything on this page was about that item.
///
/// The detail page's twin of [`crate::pms::edit_item`], and it exists for the same reason: the write
/// that justifies it now happens on a worker (`crate::viewstate`), so without this the hero's toggle
/// and the filmstrip's checks would sit unchanged for as long as the item's server takes to answer —
/// which on a share is seconds, and reads as the press having missed.
///
/// THREE things can be about the item, and all three are updated:
/// * **the loaded item itself** — a movie, or the show whose hero toggle was pressed;
/// * **one EPISODE of the loaded season** — the filmstrip's context menu, whose rk is a leaf of the
///   show `CURRENT` holds. Its season's `viewedLeafCount` moves with it, because the season tab's
///   tick is derived from that count ([`Season::watched`]) and a tick that disagreed with the
///   episode row beneath it is precisely the "two surfaces describing one item two ways" this page
///   already refuses elsewhere.
/// * **a tile of the RELATED shelf** — a different item entirely, which is what makes it the odd
///   one out: it is matched on its OWN `sid` (each row carries one, see [`Related`]) rather than on
///   the page's, and it is checked unconditionally rather than as one arm of a chain. Since
///   2026-08-21 that shelf's tiles have a context menu of their own, so this page can mark a third
///   item watched — and without this pass the tile it was pressed on kept its old tick and resume
///   bar until a refetch, which reads as the row having done nothing.
///
/// `resume_ms` is cleared with the flag for the same reason `pms::set_watched` clears it: the
/// still's own resolver (`ui::detail::ep_state`) reads progress ahead of the watched mark, so an
/// episode keeping its old `viewOffset` would still draw its resume bar and no check.
///
/// The landed refresh is the truth and silently corrects any of this; see [`crate::viewstate`].
pub(crate) fn set_watched_local(sid: crate::plex::ServerId, rk: &str, on: bool) -> bool {
    unsafe {
        let Some(d) = (*addr_of_mut!(CURRENT)).as_mut() else { return false };
        // The RELATED shelf first, and unconditionally — it is the one store here whose rows are
        // OTHER items, so it is neither the loaded item nor one of its episodes and must not be
        // reached through either of their early returns below. A related tile is also the one the
        // press most often came FROM (its context menu is why this page can mark a third item
        // watched at all), and left out, the tile kept its old tick and bar until a refetch.
        //
        // Not `else`-chained with the two arms under it for a subtler reason as well: the same
        // title can legitimately be BOTH the loaded item and a row of some other page's shelf, and
        // a hub that lists an item alongside itself is not something this function should trust
        // itself to rule out.
        let mut hit = false;
        for m in d.related.iter_mut() {
            if crate::plex::same_item((m.sid, &m.rk), (sid, rk)) {
                // the shared three-field flip (`watched`/`unwatched`/`resume_ms` move together, or
                // the tile wears the progress bar it had before and shows no tick at all)
                crate::pms::set_watched(m, on);
                hit = true;
            }
        }
        if crate::plex::same_item((d.sid, &d.rk), (sid, rk)) {
            d.watched = on;
            d.resume_ms = 0;
            return true;
        }
        // An episode is only ever a leaf of the loaded show, so it is matched on the PAGE's server —
        // `Episode` carries no `sid` of its own precisely because it cannot come from anywhere else.
        // Both misses below return `hit` rather than `false`: the Related pass above may already
        // have changed this page, and reporting "nothing here was about that item" after editing a
        // tile would be this function contradicting itself. (`viewstate` only logs the verdict, but
        // a false negative is the kind that goes unnoticed until something starts trusting it.)
        if d.sid != sid {
            return hit;
        }
        let Some(i) = d.episodes.iter().position(|e| e.rk == rk) else { return hit };
        let was = d.episodes[i].watched;
        d.episodes[i].watched = on;
        d.episodes[i].resume_ms = 0;
        if was != on {
            let cur = d.cur_season;
            if let Some(s) = d.seasons.get_mut(cur) {
                // clamped to the season's own leaf count: a server that sent none leaves this 0, and
                // `Season::watched` reads `leaf_count > 0` first, so an unknown season stays unknown
                let step = if on { 1 } else { -1 };
                s.viewed_leaf_count = (s.viewed_leaf_count + step).clamp(0, s.leaf_count.max(0));
            }
        }
        true
    }
}

/// drop the loaded detail (on leaving the detail page). Also supersedes any in-flight async
/// fetch — otherwise a load requested on the way in lands after the page closed and silently
/// repopulates CURRENT (and NOW, via `sync_now_playing`) behind whatever screen is now mounted.
pub(crate) fn clear() {
    supersede_detail();
    unsafe { *addr_of_mut!(CURRENT) = None }
}

/// TEST ONLY — install `d` as the loaded item, bypassing the fetch and its mailbox. The screens'
/// pure focus/label math reads `current()`, and the only real way to populate it is a PMS round
/// trip, which the host suite has no server for. Compiled out of the shipped binary. CURRENT is a
/// crate-wide global that this module's own tests also drive, so hold `crate::testlock::serial()`
/// across any test that calls this.
#[cfg(test)]
pub(crate) fn set_current_for_test(d: Option<Detail>) {
    unsafe { *addr_of_mut!(CURRENT) = d }
}

/// A compact descriptor of the item currently *playing*, for the in-player Info card. Unlike
/// `current()` (which stays on the detail page's show/movie), this always describes the playing
/// **leaf**: an episode carries the show title + SxEy + episode name + its still; a movie carries the
/// movie title + landscape art. Set by the play paths — `sync_now_playing()` after a leaf load, or
/// explicitly by show-page episode play (where `current()` is still the show).
pub(crate) struct NowPlaying {
    pub(crate) is_episode: bool,
    pub(crate) title: String,     // big title: show title (episode) or movie title
    pub(crate) ep_title: String,  // episode name (episode only)
    pub(crate) season: i64,
    pub(crate) index: i64,
    pub(crate) summary: String,
    pub(crate) year: i64,
    pub(crate) dur_ms: i64,
    pub(crate) rating: String,
    pub(crate) thumb: String,     // 16:9 still (episode) / landscape art (movie)
    pub(crate) detail_rk: String, // "Go to Show"/"Go to Movie" target
}
static mut NOW: Option<NowPlaying> = None;
pub(crate) fn now_playing() -> Option<&'static NowPlaying> {
    unsafe { (*addr_of!(NOW)).as_ref() }
}
pub(crate) fn set_now_playing(np: Option<NowPlaying>) {
    unsafe { *addr_of_mut!(NOW) = np }
}
/// Refresh `now_playing` from `current()` — call after a leaf `load_detail` (Continue-Watching /
/// off-catalog play, where `current()` becomes the played leaf). A show/season load leaves it None.
pub(crate) fn sync_now_playing() {
    let np = current().and_then(|d| match d.kind.as_str() {
        "episode" => Some(NowPlaying {
            is_episode: true,
            title: d.show_title.clone(),
            ep_title: d.title.clone(),
            season: d.season,
            index: d.index,
            summary: d.summary.clone(),
            year: d.year,
            dur_ms: d.dur_ms,
            rating: d.rating.clone(),
            thumb: d.thumb.clone(),
            detail_rk: d.show_rk.clone(),
        }),
        "movie" => Some(NowPlaying {
            is_episode: false,
            title: d.title.clone(),
            ep_title: String::new(),
            season: 0,
            index: 0,
            summary: d.summary.clone(),
            year: d.year,
            dur_ms: d.dur_ms,
            rating: d.rating.clone(),
            thumb: if !d.art.is_empty() { d.art.clone() } else { d.thumb.clone() },
            detail_rk: d.rk.clone(),
        }),
        _ => None, // show / season → not a playing leaf
    });
    set_now_playing(np);
}

// ---- fetches (all via the typed crate::plex client; serde DTOs, no Value scraping) ----
//
// Every one of them takes the `ServerId` rather than reaching for `plex::client()`: they run on the
// detail/season workers, and the house rule is that a worker reads no statics (`pms::parse_item`
// carries the same note). It is also what stamps the row — an item fetched from slot 1 must be
// recorded as slot 1's whatever `client()` answers with by the time the fetch returns.
fn fetch_detail(sid: crate::plex::ServerId, rk: &str) -> Option<Detail> {
    let it = crate::plex::client_for(sid)?.metadata(rk)?;
    let media0 = it.primary_media();
    // one read, both fields (see `Detail::blur`)
    let blur = it.ultra_blur_colors.and_then(|u| u.corners());
    let mut d = Detail {
        sid,
        rk: rk.to_string(),
        // WHOSE server this fetch went to, asked of the registry with the id we were handed.
        //
        // This read `dev::read("shared")` and nothing else, which meant the attribution existed
        // only under a trigger: on a signed-in television every item's `source` was empty and the
        // hero drew no "Shared by" run at all, whatever server the item came from. Owner-reported
        // 2026-08-14. `ServerFacts::handle` is empty on our own server, which is exactly what this
        // field wants — absence, not an empty owner — so the mapping needs no special case.
        //
        // dev: **`/tmp/plxnative-shared` WINS WHEN ARMED** — the precedence every trigger in this
        // app has (`crate::dev`'s module doc: `plxnative-token` beats the signed-in session), and
        // the phrase to grep for, because the same stand-in is read by `ui::home`'s hero run and
        // `ui::search::results`' owner annotation and the three must agree. A trigger exists to
        // FORCE a state, so an armed one outranks the real answer, and an armed EMPTY file forces
        // the absence of a handle rather than doing nothing. It stamps one handle onto every item
        // this session loads, which is what a fully-borrowed library looks like.
        source: crate::dev::read("shared")
            .unwrap_or_else(|| crate::plex::server_facts(sid).map(|f| f.handle.clone()).unwrap_or_default()),
        // the portable identity — what "Also available" resolves across the other sources
        guid: it.guid.clone(),
        is_show: it.kind == "show",
        kind: it.kind.clone(),
        show_title: it.grandparent_title.clone(),
        show_rk: it.grandparent_rating_key.clone(),
        season: it.parent_index,
        index: it.index,
        title: it.title.clone(),
        year: it.year,
        rating: it.content_rating.clone(),
        summary: it.summary.clone(),
        tagline: it.tagline.clone(),
        aired: it.originally_available_at.clone(),
        dur_ms: it.duration,
        resume_ms: it.view_offset,
        watched: if it.kind == "show" || it.kind == "season" {
            it.leaf_count > 0 && it.viewed_leaf_count >= it.leaf_count
        } else {
            it.view_count > 0
        },
        // empty for a show (no Media on the show container)
        part: it.first_part().map(|p| p.key.clone()).unwrap_or_default(),
        vcodec: media0.map(|m| m.video_codec.clone()).unwrap_or_default(),
        acodec: media0.map(|m| m.audio_codec.clone()).unwrap_or_default(),
        video_fps: 0.0, // set from the video Stream by parse_streams below
        // likewise set by parse_streams (one assignment point), which for a SHOW runs a second
        // time over its first episode — the show container carries no Media of its own
        video_resolution: String::new(),
        width: 0,
        height: 0,
        bitrate: 0,
        // …as are the five below, which `parse_streams` fills from that same primary version — so
        // a show's borrowed technicals and its FILE column can never describe two different files.
        container: String::new(),
        file: String::new(),
        size: 0,
        aspect_ratio: 0.0,
        video: None,
        hdr: false,
        dovi: Dovi::default(),
        art: it.art.clone(),
        thumb: it.thumb.clone(),
        blur: blur.unwrap_or_default(),
        has_blur: blur.is_some(),
        genres: it.genre.iter().map(|t| t.tag.clone()).collect(),
        countries: it.country.iter().map(|t| t.tag.clone()).collect(),
        cast: it
            .role
            .iter()
            .map(|r| Cast {
                tag: r.tag.clone(),
                role: r.role.clone(),
                thumb: r.thumb.clone(),
                id: r.id,
                tag_key: r.tag_key.clone(),
            })
            .collect(),
        // deduped like the shelf below it: a repeated Director[] row would otherwise read
        // "Directed by Jane Doe, Jane Doe"
        directors: dedup_tags(&it.director),
        crew: crew_credits(&it),
        audio: Vec::new(),
        subs: Vec::new(),
        seasons: Vec::new(),
        episodes: Vec::new(),
        on_deck: it.on_deck.as_ref().and_then(|h| h.metadata.as_deref()).map(convert_episode),
        cur_season: 0,
        related: Vec::new(),
        chapters: convert_chapters(&it.chapter),
        markers: convert_markers(&it.marker),
        ratings: convert_ratings(&it),
    };
    // audio/subtitle streams (movies carry Media/Part/Stream; a show does not — its
    // episodes do, so load_detail backfills a show's streams from its first episode).
    parse_streams(&it, &mut d);
    Some(d)
}

/// The crew jobs we surface, in the order they appear on the shelf. PMS names the job by the
/// ARRAY the person arrived in (`Director[]`/`Writer[]`) — the rows themselves carry no job
/// attribute, and (verified live) no `role` either, so this is where the sub-caption comes from.
const CREW_JOBS: [&str; 2] = ["Director", "Writer"];

/// The named tags of one crew array, in server order, without the blanks or the repeats.
fn dedup_tags(tags: &[crate::plex::Tag]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags.iter().filter(|t| !t.tag.is_empty()) {
        if !out.iter().any(|s| s == &t.tag) {
            out.push(t.tag.clone());
        }
    }
    out
}

/// Fold `Director[]` + `Writer[]` into the credits the Cast & Crew shelf draws after the actors.
///
/// One person credited twice collapses into ONE tile: two identical headshots side by side read as
/// a duplication bug, not as two credits. Across the arrays that means the writer-director's tile
/// reads "Director, Writer"; WITHIN one array (PMS does emit a repeated tag row after an agent
/// merge) the job is already there and is not repeated — "Director, Director" is nobody's credit.
/// Nameless rows are dropped: a tile with no name and (often) no headshot is a blank circle the
/// focus can still land on.
fn crew_credits(it: &crate::plex::Metadata) -> Vec<Cast> {
    let mut out: Vec<Cast> = Vec::new();
    for (job, list) in CREW_JOBS.iter().zip([&it.director, &it.writer]) {
        for t in list.iter().filter(|t| !t.tag.is_empty()) {
            match out.iter_mut().find(|c| c.tag == t.tag) {
                Some(c) if !c.role.ends_with(job) => {
                    c.role.push_str(", ");
                    c.role.push_str(job);
                }
                Some(_) => {}
                // the id/guid ride along exactly as they do for an actor: a director is a person
                // with a `tagKey`, so a crew tile opens the same person page a cast tile does
                None => out.push(Cast {
                    tag: t.tag.clone(),
                    role: job.to_string(),
                    thumb: t.thumb.clone(),
                    id: t.id,
                    tag_key: t.tag_key.clone(),
                }),
            }
        }
    }
    out
}

/// Convert a part's Stream[] into (audio, subs, video_fps, video_is_hdr, dovi) — the ONE
/// plex::Stream → Stream mapping (the detail parse and the playing-tracks store both use it).
/// HDR is the video stream's transfer characteristic (PQ or HLG) or a Dolby Vision flag — the
/// input to the facts row's "HDR → SDR without tone-mapping" warning, which only matters where
/// the item would transcode on a server that cannot tone-map. [`Dovi`] is the finer-grained
/// companion to that flag and answers a different question — not "is this HDR" but "can we show
/// the base layer at all" (`route::video_direct_plays` gates direct play on it).
/// What one part's `Stream[]` reduces to — the return of [`convert_streams`].
///
/// A named struct rather than the 5-tuple it was, because the Track-information panel needed the
/// VIDEO track itself and a sixth positional element is where a tuple stops being readable at the
/// call site. Every field keeps the meaning it had.
#[derive(Default)]
pub(crate) struct Streams {
    pub(crate) audio: Vec<Stream>,
    pub(crate) subs: Vec<Stream>,
    /// The part's video track, or `None` for an audio-only part. Carries the per-stream bitrate,
    /// profile, bit depth and chroma that `fps`/`hdr`/`dovi` alone throw away.
    pub(crate) video: Option<Stream>,
    /// the video track's `frameRate` (0 = unknown) — feeds the Load esInfo
    pub(crate) fps: f64,
    pub(crate) hdr: bool,
    pub(crate) dovi: Dovi,
}

fn convert_streams(streams: &[crate::plex::Stream]) -> Streams {
    let (mut audio, mut subs, mut fps) = (Vec::new(), Vec::new(), 0.0);
    let mut hdr = false;
    let mut dovi = Dovi::default();
    let mut video: Option<Stream> = None;
    for s in streams {
        let st = Stream {
            id: s.id,
            index: s.index,
            lang: s.language.clone(),
            lang_code: s.language_code.to_lowercase(),
            codec: s.codec.clone(),
            channels: s.channels,
            layout: s.audio_channel_layout.clone(),
            bitrate: s.bitrate,
            profile: s.profile.clone(),
            bit_depth: s.bit_depth,
            chroma: s.chroma_subsampling.clone(),
            sdh: s.hearing_impaired != 0,
            ad: s.audio_description != 0 || s.title.to_lowercase().contains("descri"),
            forced: s.forced != 0,
            default: s.is_default != 0,
            title: s.title.clone(),
            // embedded container streams carry no delivery key; sidecars do
            external: s.stream_type == 3 && !s.key.is_empty(),
            // the server's current pick for this part (a track chosen on another client)
            selected: s.selected != 0,
        };
        match s.stream_type {
            1 => {
                fps = s.frame_rate; // e.g. 23.976 — for the Load esInfo
                // PQ (HDR10) or HLG transfer, or Dolby Vision — the dev PMS sends
                // colorTrc=smpte2084 on its HDR10 items (probed live 2026-08-11)
                hdr = matches!(s.color_trc.as_str(), "smpte2084" | "arib-std-b67") || s.dovi_present != 0;
                // NB a Profile 5 file sends NO colorTrc at all (verified live 2026-08-21: the
                // dev server's one P5 item omits the field), so `dovi_present` is the only
                // thing that makes it read as HDR — and the layering fields below are the only
                // thing that makes it read as unplayable.
                //
                // Guarded on `dovi_present`, unlike the two assignments above it, and the
                // difference is deliberate. `fps` and `hdr` take the LAST video stream in the
                // part; a Dolby Vision record must instead SURVIVE one, because the direct-play
                // gate reads it and losing it fails the wrong way — a second `streamType: 1`
                // stream carrying no DOVI fields (embedded cover art is the shape to expect)
                // would blank a Profile 5 record back to `Dovi::default()`, which refuses
                // nothing, and the file would direct-play in the wrong colours again. No part on
                // the dev server has two video streams today (all 540 leaves swept 2026-08-21),
                // so this costs nothing and is not a change to any measured behaviour — it is the
                // one assignment here whose failure is silent and wrong rather than silent and
                // cosmetic.
                if s.dovi_present != 0 {
                    dovi = Dovi {
                        present: true,
                        profile: s.dovi_profile,
                        bl_compat: s.dovi_bl_compat_id,
                        el_present: s.dovi_el_present != 0,
                        level: s.dovi_level,
                        version: Dovi::parse_version(&s.dovi_version),
                        bl_present: s.dovi_bl_present != 0,
                        rpu_present: s.dovi_rpu_present != 0,
                    };
                }
                // The video track ITSELF, kept rather than reduced to `fps`/`hdr`/`dovi`. It is the
                // only place the stream's own bitrate, profile, bit depth and chroma survive, and
                // `ui::tracks_panel`'s VIDEO column is built from all four. Guarded like the DV
                // record and for the same reason — a second `streamType: 1` stream (embedded cover
                // art is the shape to expect) must not overwrite the real picture's technicals.
                if video.is_none() {
                    video = Some(st);
                }
            }
            2 => audio.push(st),
            3 => subs.push(st),
            _ => {}
        }
    }
    Streams { audio, subs, video, fps, hdr, dovi }
}

/// parse an item's Media[0].Part[0].Stream[] into d.audio / d.subs (the About footer), plus that
/// same version's technical fields (resolution/size/bitrate — the hero's media badge). Both ride
/// the ONE version (see `plex::Metadata::primary_media`), and both are borrowed from a show's
/// first episode by the same call in `fetch_item_streams`, so they can't describe different files.
fn parse_streams(it: &crate::plex::Metadata, d: &mut Detail) {
    if let Some(m) = it.primary_media() {
        d.video_resolution = m.video_resolution.clone();
        d.width = m.width;
        d.height = m.height;
        d.bitrate = m.bitrate;
        d.container = m.container.clone();
        d.aspect_ratio = m.aspect_ratio;
    }
    if let Some(p) = it.first_part() {
        d.file = p.file.clone();
        d.size = p.size;
        // The PART's container wins where it has one — a version can hold parts in different
        // containers, and the part is the thing the panel is describing. `Media.container` is the
        // fallback, already assigned above.
        if !p.container.is_empty() {
            d.container = p.container.clone();
        }
        let s = convert_streams(&p.stream);
        d.audio = s.audio;
        d.subs = s.subs;
        d.video = s.video;
        d.hdr = s.hdr;
        d.dovi = s.dovi;
        if s.fps > 0.0 {
            d.video_fps = s.fps;
        }
    }
}

// ---- the PLAYING-item store — the in-player source of truth ---------------------------------
// Unlike `current()` (the detail page's item — it stays on the SHOW during an episode play, and
// can be a different item entirely when playing straight from Home), this always holds the
// played leaf's OWN data. The track menu and the route's audio pick read its streams; feeding a
// menu built from episode 1's streams to a playback of episode 5 was a real track-identity bug.
// `markers` is here for exactly that reason and not on `Detail`: skipping episode 1's intro
// timing during episode 5 is the same bug wearing a different hat. `chapters` rides along for the
// third instance of it: the Chapters tab and strip used to read `current()`, so an episode played
// from a SHOW page found a show container (which carries no Chapter[]) and the tab silently
// vanished — while a `current()` holding some OTHER leaf would have seeked with its offsets.

#[derive(Clone)]
pub(crate) struct PlayingItem {
    /// The server the played leaf lives on. This is the store that feeds PLAYBACK — the track
    /// menu's `Stream.id`s (which are PUT back to a server), the direct-play gate's frame size, the
    /// esInfo fps, the chapters and the markers — so a bare-rk cache hit against a colliding item on
    /// the other machine is the silent failure this field exists to stop: every one of those values
    /// would be the wrong file's, with nothing on screen to say so.
    pub(crate) sid: crate::plex::ServerId,
    pub(crate) rk: String,
    pub(crate) audio: Vec<Stream>,
    pub(crate) subs: Vec<Stream>,
    pub(crate) video_fps: f64, // the played leaf's video fps (0 = unknown) — feeds the Load esInfo
    /// The source's stored frame size, `Media[0]` (0 = unknown). `route.rs`'s local direct-play
    /// gate tests it against the device table's bound: the smart-DP branch never asks PMS, so
    /// the profile's `*`-scoped width/height limitation cannot stop a 4K source from reaching a
    /// 1080p-bounded decoder unless the client also checks it here (issue #22's over-claim
    /// class — docs/plex-pass-audit.md, closing section).
    pub(crate) width: i64,
    pub(crate) height: i64,
    /// Whole-file bitrate in kbps (`Media[0].bitrate`). Auto uses this—not merely the video
    /// stream's rate—when deciding whether a remote connection has enough headroom to carry the
    /// original file, because the transport also has to carry audio and container overhead.
    pub(crate) bitrate: i64,
    /// The played leaf's Dolby Vision layering — the direct-play gate's other refusal, beside the
    /// frame size above and for the same reason: the smart-DP branch never asks PMS, so a file
    /// whose base layer we cannot display correctly (Profile 5, or a dual-layer Profile 7) would
    /// otherwise be fed to the decoder verbatim and shown in the wrong colours. See [`Dovi`].
    pub(crate) dovi: Dovi,
    pub(crate) markers: Vec<Marker>, // intro / credits segments — the in-player Skip prompt
    pub(crate) chapters: Vec<Chapter>, // chapter boundaries — the in-player Chapters tab/strip
}
static mut PLAYING: Option<PlayingItem> = None;

/// the playing leaf's own streams + markers (None until a catalog item starts playing).
/// Main-thread only.
pub(crate) fn playing() -> Option<&'static PlayingItem> {
    unsafe { (*addr_of!(PLAYING)).as_ref() }
}

/// The playing leaf's markers, or an empty slice — the ONE accessor the in-player skip prompt
/// reads, so no call site has to know the store can be absent mid-resolve.
pub(crate) fn playing_markers() -> &'static [Marker] {
    playing().map(|p| p.markers.as_slice()).unwrap_or(&[])
}

/// The playing leaf's chapters, or an empty slice — the ONE accessor the Chapters strip reads.
/// Deliberately NOT `current()`: during a show-page episode play `current()` is the SHOW (no
/// `Chapter[]` at all), which is why the tab never appeared on that path, and a `current()` holding
/// a different leaf would seek with another item's offsets.
pub(crate) fn playing_chapters() -> &'static [Chapter] {
    playing().map(|p| p.chapters.as_slice()).unwrap_or(&[])
}

/// Load the playing-item track store for `rk` at play time (route::build_stream). Reuses the
/// loaded detail's streams when it IS this item (no extra GET on the play path — the same
/// optimization the old `audio_tracks` fetch had); otherwise one metadata fetch. An empty `rk`
/// (local-sample / URL-override play) clears the store.
/// PURE: fetch the playing item's track lists. Safe on a worker — reads and writes no statics.
/// The cache-hit shortcut is `cached_playing` (main thread) and the install is `install_playing`,
/// because `playing()` hands out a `&'static` whose Vecs the track menu and info panel hold
/// slices into during playback.
/// MAIN THREAD: the cache-hit half of the old `load_playing` — reuse the loaded detail's streams
/// when it IS this item, so playing from a detail page costs no extra GET. Snapshotted into
/// `ResolveEnv` and handed to the worker; splitting the fetch out lost this and quietly added a
/// PMS round trip to every play from a detail page.
///
/// The hit test is the PAIR `(sid, rk)`, and this is the site where a bare key is most dangerous:
/// it SKIPS the PMS fetch, so a collision here silently substitutes the loaded page's item for the
/// one about to play — its audio/subtitle `Stream.id`s (then PUT to the other server), its frame
/// size (so the direct-play gate reasons about the wrong resolution), its fps, chapters and
/// markers. A miss costs one round trip; a false hit costs the whole playback.
///
/// This closed a TODO that stood here through the foundation commits: `Detail` had no server, so
/// the filter was the rk alone and the parameter was deliberately unused. `Detail.sid` is what
/// made the pair test possible.
pub(crate) fn cached_playing(sid: crate::plex::ServerId, rk: &str) -> Option<PlayingItem> {
    current()
        .filter(|d| crate::plex::same_item((d.sid, &d.rk), (sid, rk)) && !d.audio.is_empty())
        .map(|d| PlayingItem {
        sid,
        rk: rk.to_string(),
        audio: d.audio.clone(),
        subs: d.subs.clone(),
        video_fps: d.video_fps,
        width: d.width,
        height: d.height,
        bitrate: d.bitrate,
        dovi: d.dovi,
        markers: d.markers.clone(),
        chapters: d.chapters.clone(),
    })
}

/// `sid` names the server `rk` is a key on. It runs on the resolve worker, so the server must
/// arrive by value: `client_opt()` here would fetch whichever server is CURRENT, and a ratingKey
/// that also exists there would come back with a different film's stream list.
pub(crate) fn fetch_playing_item(sid: crate::plex::ServerId, rk: &str) -> Option<PlayingItem> {
    if rk.is_empty() {
        return None;
    }
    let it = crate::plex::client_for(sid).and_then(|c| c.metadata(rk));
    // Markers and chapters hang off the ITEM, streams off its first Part — so a part-less response
    // still yields both of those instead of discarding all three. `Client::metadata` already sends
    // `includeChapters=1` (plex/library.rs), so the Chapter[] is on the wire either way: taking it
    // here costs no request, and dropping it is what hid the Chapters tab on the episode path.
    let markers = it.as_ref().map(|it| convert_markers(&it.marker)).unwrap_or_default();
    let chapters = it.as_ref().map(|it| convert_chapters(&it.chapter)).unwrap_or_default();
    let st = it
        .as_ref()
        .and_then(|it| it.first_part().map(|p| convert_streams(&p.stream)))
        .unwrap_or_default();
    let (audio, subs, video_fps, dovi) = (st.audio, st.subs, st.fps, st.dovi);
    // the frame size rides the same PRIMARY version the streams come from (route.rs's
    // direct-play gate tests it against the device bound — see the field doc)
    let (width, height, bitrate) = it
        .as_ref()
        .and_then(|it| it.primary_media().map(|m| (m.width, m.height, m.bitrate)))
        .unwrap_or((0, 0, 0));
    Some(PlayingItem {
        sid,
        rk: rk.to_string(),
        audio,
        subs,
        video_fps,
        width,
        height,
        bitrate,
        dovi,
        markers,
        chapters,
    })
}

/// Retire BOTH descriptions of the item that was playing, together.
///
/// They have to move as one. `NOW` feeds the HUD caption and Info card; `PLAYING` feeds the track
/// menu and — since markers landed here — the skip/Up Next controls. Clearing only `NOW` (which is
/// what each play path used to do by hand) leaves the FINISHED episode's markers live for the whole
/// resolve + pre-roll of the next one, and a `final` credits marker is deliberately open-ended to
/// `i64::MAX`, so a stale one matches any playhead: the new episode would offer to skip its own
/// credits seconds after starting. Nothing fires today, but only by incidental ordering — this
/// makes it a contract instead.
pub(crate) fn retire_playing() {
    set_now_playing(None);
    retire_playing_item();
}

/// Retire ONLY the track/marker/chapter store, leaving the `NowPlaying` caption alone — what a NEW
/// play REQUEST does at its start ([`crate::route::request_play`]), beside the same retirement of
/// `UP_NEXT`.
///
/// The caption cannot be retired there because `detail::play_episode_at` sets it just BEFORE
/// requesting the play. The store must be, though: it is the PREVIOUS leaf's for the whole resolve
/// window (0.5-3 s, longer through a `/decision` handshake) and the HUD is up for all of it. With
/// chapters in here that became user-reachable — the transport advertised a Chapters tab whose OK
/// seeked the NEW episode to some other item's offset.
pub(crate) fn retire_playing_item() {
    unsafe {
        *addr_of_mut!(PLAYING) = None;
        (*addr_of_mut!(SKIPPED)).clear();
    }
}

/// MAIN THREAD: install a fetched playing-item store.
pub(crate) fn install_playing(pt: Option<PlayingItem>) {
    unsafe { (*addr_of_mut!(SKIPPED)).clear() }; // a different leaf's markers, so a fresh slate
    if let Some(pt) = &pt {
        crate::player::log(&format!(
            "playing item: rk={} audio={} subs={} markers={} chapters={}",
            pt.rk, pt.audio.len(), pt.subs.len(), pt.markers.len(), pt.chapters.len()));
    }
    unsafe { *addr_of_mut!(PLAYING) = pt };
}

// ---- list-position → demuxer-ordinal conversion --------------------------------------------
// The demuxer selects "the Nth stream of its type" in CONTAINER order; the menu/metadata lists
// are in PMS document order. These convert a list position to that container ordinal by sorting
// on PMS `Stream.index` (stable tie-break on list position, so an index-less response degrades
// to document order — the previous behavior).

/// Container-audio ordinal of `audio[i]` — what `player::set_audio_track`/`request_audio_track`
/// (→ ff's nth_audio_stream) consume.
pub(crate) fn audio_ordinal(audio: &[Stream], i: usize) -> i32 {
    if i >= audio.len() {
        return i as i32;
    }
    let me = (audio[i].index, i);
    audio.iter().enumerate().filter(|(j, s)| (s.index, *j) < me).count() as i32
}

/// Container ordinal of `subs[i]` among the EMBEDDED subtitle streams (all ff.rs enumerates —
/// sidecars are not in the container), or -1 when `subs[i]` is itself external (nothing to
/// client-render on direct-play; only a server transcode can burn it).
pub(crate) fn sub_render_ordinal(subs: &[Stream], i: usize) -> i32 {
    let s0 = match subs.get(i) {
        Some(s) if !s.external => s,
        _ => return -1,
    };
    let me = (s0.index, i);
    subs.iter()
        .enumerate()
        .filter(|(j, s)| !s.external && (s.index, *j) < me)
        .count() as i32
}

/// fetch one item's full metadata and parse its streams into `d` — used to borrow a
/// show's first-episode audio/subtitle tracks (the show container carries none).
fn fetch_item_streams(sid: crate::plex::ServerId, rk: &str, d: &mut Detail) {
    if let Some(it) = crate::plex::client_for(sid).and_then(|c| c.metadata(rk)) {
        parse_streams(&it, d);
    }
}

fn fetch_seasons(sid: crate::plex::ServerId, rk: &str) -> Vec<Season> {
    let mc = match crate::plex::client_for(sid).and_then(|c| c.children(rk)) {
        Some(m) => m,
        None => {
            // The empty Vec is the deliberate degrade (see `fetch_episodes`'s note), but by the
            // time `fetch_full` prints `seasons=` the refusal and a show that genuinely has no
            // seasons are the same zero — so the refusal has to say so HERE, or the log records a
            // failed GET as a fact about the library.
            crate::log(&format!("detail: rk={rk} — no season list (server unresolved, or it refused); the seasons= below is that, not a count"));
            return Vec::new();
        }
    };
    mc.metadata
        .iter()
        .filter(|x| x.kind == "season")
        .map(|x| Season {
            rk: x.rating_key.clone(),
            index: x.index,
            title: x.title.clone(),
            leaf_count: x.leaf_count,
            viewed_leaf_count: x.viewed_leaf_count,
        })
        .collect()
}

/// A season's episode list, or **None when the `/children` GET failed**. The failure has to stay
/// distinguishable from a genuinely empty season all the way to the pump: returning an empty Vec
/// for both is what let one transient GET blank a populated episode row, with no spinner, no error
/// and no way to ask the tab again. Same rule browse.rs's page fetch carries with its `total < 0`
/// sentinel ("a wiped-to-empty store here was a review-confirmed bug").
///
/// NB its siblings `fetch_seasons`/`fetch_related` deliberately KEEP the degrade-to-empty: both are
/// only ever called from `fetch_full`, which builds a Detail from nothing — there is no previous
/// list there to protect, and neither is worth failing the whole page over.
fn fetch_episodes(sid: crate::plex::ServerId, season_rk: &str) -> Option<Vec<Episode>> {
    let mc = crate::plex::client_for(sid)?.children(season_rk)?;
    Some(mc.metadata.iter().map(convert_episode).collect())
}

/// One `/children` row → an [`Episode`]. Split out of [`fetch_episodes`] so the wire → model
/// mapping is host-testable without a PMS — the watched flag in particular is DERIVED, and a
/// derivation nothing can exercise is how `viewCount` came to be parsed at
/// `plex/models.rs` and then dropped on the floor here for the whole life of the episode row.
fn convert_episode(x: &crate::plex::Metadata) -> Episode {
    let media0 = x.media.first();
    Episode {
        rk: x.rating_key.clone(),
        index: x.index,
        season: x.parent_index,
        title: x.title.clone(),
        summary: x.summary.clone(),
        aired: x.originally_available_at.clone(),
        dur_ms: x.duration,
        thumb: x.thumb.clone(),
        resume_ms: x.view_offset,
        // `viewCount` is ABSENT until the leaf has been watched once, so `> 0` is the whole test —
        // the same rule `fetch_detail` applies to a movie. (A show/season instead compares
        // `viewedLeafCount` to `leafCount`; an episode is a leaf and has neither.)
        watched: x.view_count > 0,
        part: x.first_part().map(|p| p.key.clone()).unwrap_or_default(),
        rating: x.content_rating.clone(),
        vcodec: media0.map(|m| m.video_codec.clone()).unwrap_or_default(),
        acodec: media0.map(|m| m.audio_codec.clone()).unwrap_or_default(),
    }
}

fn fetch_related(sid: crate::plex::ServerId, rk: &str) -> Vec<Related> {
    let mc = match crate::plex::client_for(sid).and_then(|c| c.related(rk)) {
        Some(m) => m,
        None => {
            // Same shape as `fetch_seasons` above: the degrade is deliberate, the silence is not —
            // an item with no related hub and a refused GET both reach `fetch_full`'s `related=0`.
            crate::log(&format!("detail: rk={rk} /related did not answer — the related= below is that refusal"));
            return Vec::new();
        }
    };
    related_rows(&mc, sid)
}

/// Related tiles this shelf holds at most. PMS answers `/related` with several titled hubs and we
/// concatenate them, so without a cap a well-connected film can carry a hundred rows into a strip
/// that shows six.
const RELATED_MAX: usize = 20;

/// `/related`'s response → the shelf's rows. **PURE**, split out of [`fetch_related`] so the three
/// things that can be wrong here are host-testable: which fields survive the copy, the de-duplication
/// across hubs, and the cap.
///
/// De-duplication is across the WHOLE response and not per hub, which is the point of it: PMS's
/// related hubs overlap heavily ("Similar Movies" and "More with <actor>" routinely name the same
/// film), and a flattened strip that listed it twice would put two tiles of one title side by side.
fn related_rows(mc: &crate::plex::MediaContainer, sid: crate::plex::ServerId) -> Vec<Related> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for h in &mc.hub {
        for x in &h.metadata {
            if x.rating_key.is_empty() || !seen.insert(x.rating_key.clone()) {
                continue;
            }
            // THE shared row mapping, not a three-field copy — see [`Related`]. `sid` is the
            // server this response came from, passed in and never looked up: `fetch_related` runs
            // on the detail worker, and the house rule (`pms::parse_item`'s own doc) is that a
            // worker reads no statics, because "the current server" can change while a fetch is in
            // flight and the rows in hand belong to the machine that was asked.
            out.push(crate::pms::parse_item(x, sid));
            if out.len() >= RELATED_MAX {
                return out;
            }
        }
    }
    out
}

/// The full detail fetch for `rk` (movie or show): item metadata + cast + streams, plus — for
/// shows — seasons, the first season's episodes and its stream backfill, plus the related hub.
/// 2 PMS round-trips for a movie, 5 for a show.
///
/// PURE NETWORK + PARSING — it touches no `static mut`, which is exactly what lets it run either
/// on the main thread ([`load_detail_now`]) or on a worker ([`request_detail`]). Keep it that way:
/// installing the result is the caller's job, and on the async path that must happen on the main
/// thread (see the DETAIL_SLOT note).
fn fetch_full(sid: crate::plex::ServerId, rk: &str) -> Option<Detail> {
    // `ms=` is the whole chain's wall clock. It is the exact cost `request_detail` moves off the
    // SDL loop, so it is the number to read when judging whether a call site can afford to block
    // — note the framedrop breakdown CANNOT show it (fd_pc0 starts after event handling).
    let t0 = std::time::Instant::now();
    // The ONE line that says a detail page was asked for and got nothing — the summary at the end
    // of this function prints on success only, and nothing downstream can speak for the failure.
    // Its worst shape is the page opened through `detail::open_rk`, which clears before requesting:
    // `current()` is then None, `detail_loading()` settles the moment this lands so the spinner
    // GOES, and the page sits on the catalog row's hero art with an empty body that nothing
    // re-requests. The other request sites keep whatever was loaded, so they degrade more quietly —
    // but all four are equally silent in the log without this, and byte-identical to the user
    // never having pressed OK.
    //
    // Worded as "no metadata" rather than "the GET failed": `fetch_detail` is
    // `client_for(sid)?.metadata(rk)?`, so this arm is also taken when the server id resolves to no
    // client at all and no request was ever issued. One line for both is right — the page is equally
    // empty either way — but it must not assert a round trip that may not have happened.
    let Some(mut d) = fetch_detail(sid, rk) else {
        crate::log(&format!("detail: rk={rk} sid={sid:?} — no metadata (server unresolved, or it refused)"));
        return None;
    };
    if d.is_show {
        d.seasons = fetch_seasons(sid, rk);
        if let Some(s0) = d.seasons.first() {
            // a first-season failure is not worth failing the whole page over — the hero, cast
            // and Related still load, and there is no previous list here to protect. It is still
            // named, because the `eps=` below cannot tell it from a season with no episodes.
            d.episodes = fetch_episodes(sid, &s0.rk).unwrap_or_else(|| {
                crate::log(&format!(
                    "detail: rk={rk} season rk={} /children did not answer — the eps= below is that refusal",
                    s0.rk));
                Vec::new()
            });
        }
        // A show carries no streams itself — backfill from ONE episode: the one the hero is
        // about, which is the one Play starts (`on_deck` when the show has been started, else
        // its first). Everything downstream reads this as "the show's" media, so borrowing from
        // a different episode than the button plays would have the chips, the About footer and
        // "how this plays" describing a file the user is not about to watch.
        let hero_ep = d.on_deck.as_ref().map(|e| e.rk.clone());
        let ep = hero_ep.or_else(|| d.episodes.first().map(|e| e.rk.clone()));
        if let Some(ep_rk) = ep {
            fetch_item_streams(sid, &ep_rk, &mut d);
            // NB `part`/`vcodec`/`acodec` are deliberately NOT backfilled. They are the item's OWN
            // playable file, and "a show has an empty part" is load-bearing elsewhere — `app.rs`'s
            // play trigger reads it as "this is a show, take the episode's resume point instead",
            // so filling it here would silently hand a show's duration to an episode's playback.
            // A consumer that wants to know how the HERO's episode will play asks the episode
            // (`ui::detail::draw_play_mode`), which is also the only place that knows which
            // episode the button would start.
        }
    }
    d.related = fetch_related(sid, rk);
    crate::player::log(&format!(
        "detail: rk={} '{}' show={} genres={} cast={} crew={} seasons={} eps={} related={} audio={} subs={} ms={}",
        d.rk, d.title, d.is_show, d.genres.len(), d.cast.len(), d.crew.len(), d.seasons.len(), d.episodes.len(),
        d.related.len(), d.audio.len(), d.subs.len(), t0.elapsed().as_millis()
    ));
    Some(d)
}

/// [`request_detail`] but BLOCKING — for the callers that read `current()` on the NEXT statement:
/// `open_rk_season` (whose chained `load_season_now` indexes `d.seasons`), home_activate's
/// play-a-show arm (which gates on `current().rk == expect`), and the headless `plxnative-play` /
/// `plxnative-detail` triggers (which derive the leaf part/codecs, or replay move_focus/on_ok, in
/// the same frame). Every remaining call of this is a deliberate freeze — hence the `_now` name.
pub(crate) fn load_detail_now(sid: crate::plex::ServerId, rk: &str) {
    // this synchronous load wins over anything in flight — both the detail worker (whose landing
    // would otherwise overwrite it a beat later) and the season fetch (same show re-opened: a
    // stale landing would overwrite the fresh first-season episode list)
    supersede_detail();
    supersede_season();
    let rk = rk.to_string();
    let _ = catch_unwind(move || {
        if let Some(d) = fetch_full(sid, &rk) {
            // the SAME cross-source resolve `pump_detail` kicks. This path installs CURRENT itself
            // rather than going through the mailbox, so a resolve hung only off the async landing
            // never ran for anything opened this way — `plxnative-detail`, `open_rk_season`, and
            // home's play-a-show arm. Found by the button staying absent on a device that had two
            // copies of the guid.
            request_alt_sources(d.sid, &d.rk, &d.guid);
            unsafe { *addr_of_mut!(CURRENT) = Some(d) }
            // if this load is a playing leaf (episode/movie), refresh the Info card's descriptor
            sync_now_playing();
        }
    });
}

// ---- async detail load ---------------------------------------------------------------------
// Opening a detail page used to block the SDL loop on 2 (movie) to 5 (show) sequential PMS
// round-trips, straight off the key handler. `request_detail` spawns the fetch and `pump_detail`
// installs the result — the page mounts THIS frame on the catalog row's art/title/summary and
// fills in a beat later. Same shape as the season mailbox below and route.rs's play resolve.
//
// The worker MUST NOT write CURRENT. `current()` hands out a `&'static Detail` that ~25 draw
// sites read within a frame, so a background store would drop the old `Detail` under a live
// reference — a use-after-free, not a lint. Keeping the main thread the sole writer is precisely
// what makes that `&'static` sound, so the worker's only output is the mailbox.
static DETAIL_GEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static DETAIL_DONE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
struct DetailResult {
    gen: u32,
    d: Option<Detail>, // None = the fetch failed or panicked — the page keeps the previous item
}
static DETAIL_SLOT: std::sync::Mutex<Option<DetailResult>> = std::sync::Mutex::new(None);

/// Invalidate any in-flight/pending detail fetch and mark the mailbox settled: bump the
/// generation (so a late landing is discarded by `pump_detail`), catch DETAIL_DONE up to it
/// (`detail_loading()` → false), and clear the slot. Returns the fresh generation.
fn supersede_detail() -> u32 {
    use std::sync::atomic::Ordering;
    let gen = DETAIL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    DETAIL_DONE.store(gen, Ordering::SeqCst);
    *DETAIL_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    gen
}

/// Post a finished fetch to the mailbox. MONOTONE: an older fetch landing late must never clobber
/// a newer result the pump hasn't consumed yet. Called from the worker (and from the tests, which
/// is the point of it being a named function rather than inline in the closure — the guard is the
/// one piece of this machinery that a test can't reach through `request_detail`).
fn land_detail(gen: u32, d: Option<Detail>) {
    let mut slot = DETAIL_SLOT.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().map(|r| r.gen < gen).unwrap_or(true) {
        *slot = Some(DetailResult { gen, d });
    }
}

/// MAIN THREAD, NON-BLOCKING. Supersedes any in-flight load and spawns the fetch; the result
/// lands via [`pump_detail`]. The caller mounts the detail page this same frame.
///
/// `sid` names the server to ask and is captured by the CALLER, on the main thread — the worker
/// must not read the current server (see the fetch block's note), and the page being opened may
/// belong to a machine that is not the current one at all.
pub(crate) fn request_detail(sid: crate::plex::ServerId, rk: &str) {
    use std::sync::atomic::Ordering;
    // drop any season fetch in flight for the OLD item — its landing would patch the new one
    supersede_season();
    // NOT supersede_detail(): the generation must move (a stale landing is discarded) but
    // DETAIL_DONE must stay behind so `detail_loading()` reports this fetch as in flight
    let gen = DETAIL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    *DETAIL_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let rk = rk.to_string();
    let spawned = crate::task::spawn_small("detail", move || {
        // the mailbox is filled OUTSIDE the guard so a panicking fetch still lands (as None) —
        // otherwise detail_loading() would report an in-flight fetch forever
        let d = catch_unwind(|| fetch_full(sid, &rk)).unwrap_or(None);
        land_detail(gen, d);
    });
    if !spawned {
        // no worker means nothing will ever land: catch DONE up or detail_loading() latches true
        // forever behind a spinner that can never resolve
        DETAIL_DONE.store(gen, Ordering::SeqCst);
    }
}

/// MAIN THREAD, once a frame, ROUTE-UNCONDITIONAL (a landing must never depend on which screen is
/// mounted — the play paths request a detail from Home and flip straight to the player). Installs
/// a landed fetch into CURRENT and returns true when a fresh item was published. A stale landing —
/// superseded by a newer request, by a blocking load, or by `clear()` when the page closed — is
/// dropped.
pub(crate) fn pump_detail() -> bool {
    use std::sync::atomic::Ordering;
    let taken = DETAIL_SLOT.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(r) = taken else { return false };
    if r.gen != DETAIL_GEN.load(Ordering::SeqCst) {
        return false; // superseded while in flight
    }
    DETAIL_DONE.store(r.gen, Ordering::SeqCst);
    // fetch failed: keep the previously loaded item. The mailbox carries no rk of its own, so the
    // line naming WHICH page it was is written at the fetch site (`fetch_full`) — read it there
    // rather than adding an anonymous one here. ONE arrival is not covered by that: a worker that
    // PANICKED lands `None` too, and never reached `fetch_full`'s line. `task`'s panic logger names
    // the thread and the source location, so the failure is in the log — it just is not in these
    // words, and a `None` here with no `detail:` line above it is that case.
    let Some(d) = r.d else { return false };
    // The LANDING is what defines which show's episodes are current, so the season supersede has
    // to happen here as well as at request time: a tab hop issued while this load was in flight
    // spawned a fetch against the OLD item, and its landing would patch these fresh episodes.
    supersede_season();
    // Ask the other sources about this item BEFORE the move: the resolve needs the item's own
    // server and its portable guid, and this is the one place both are known on the main thread.
    // A page with no guid, or a one-server install, spawns nothing.
    request_alt_sources(d.sid, &d.rk, &d.guid);
    unsafe { *addr_of_mut!(CURRENT) = Some(d) }
    // if this load is a playing leaf (episode/movie), refresh the Info card's descriptor from it
    sync_now_playing();
    true
}

// ---- "Also available": the same film on the OTHER sources ------------------------------------
//
// The cross-source resolve, in the mailbox shape the detail fetch uses and for the same reason: it
// is one round trip PER REGISTERED SOURCE, and doing it on the SDL loop would park the frame for a
// `connect(2)` timeout per unreachable share.
//
// It runs off the back of a landed detail rather than beside it, because it needs that detail's
// `guid` — which only the fetch can supply — and because a page with no guid (a server that sent
// none) must cost nothing at all.
static ALT_GEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static ALT_ROSTER_GEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
struct AltResult {
    gen: u32,
    roster_gen: u32,
    /// The SERVER the resolve was asked for — the other half of `rk`, and carried for exactly the
    /// reason [`SeasonResult::sid`] is. A `ratingKey` is a server-local integer dense from 1
    /// (docs/shared-servers.md §1), so leaving OUR film 4 and opening the SHARE's film 4 while a
    /// resolve is out passes an rk-only test — and the generation guard cannot see that hop either,
    /// since it only moves when a DETAIL lands and the new page's is still in flight. The panel
    /// would then list the other machine's copies, and OK on one would open a different film.
    sid: crate::plex::ServerId,
    /// The rk the resolve was asked FOR, carried so the landing can be matched against the page
    /// that is mounted now — `alt_sources::install` refuses any other pair, and this is what lets
    /// it.
    rk: String,
    list: Vec<crate::ui::alt_sources::AltCopy>,
}
static ALT_SLOT: std::sync::Mutex<Option<AltResult>> = std::sync::Mutex::new(None);

/// MAIN THREAD. Ask EVERY registered source whether it holds this guid — the item's own included.
///
/// Including our own copy is not redundancy, it is what the panel is: a list of every copy with the
/// one you are on ticked, whose first row is normally "This account". Querying rather than
/// synthesising that row from the open `Detail` also gets the one field the page does not have —
/// which LIBRARY the copy is in, since a detail page knows its item and not the shelf it came from.
/// And the gate counts distinct SOURCES, so a list built of the others alone can never reach two
/// and the control would never appear however many servers held the film. (It didn't: this
/// function skipped `sid` on its first outing and the button stayed absent on a device with two
/// copies of the same guid.)
///
/// Sources are captured here, on the main thread, as a plain list of ids — the worker resolves each
/// through `client_for` and never asks what is current.
///
/// `sid` is the ITEM's own server, captured with `rk` at the call site because the two are one
/// identity: it rides through [`AltResult`] to `alt_sources::install`, which refuses a landing for
/// any other pair. Without it a resolve parked on a dead share's `connect(2)` timeout lands on
/// whatever page holds the same ratingKey when it finally answers, which across two servers is the
/// ordinary case rather than an exotic one.
fn request_alt_sources(sid: crate::plex::ServerId, rk: &str, guid: &str) {
    use std::sync::atomic::Ordering;
    let gen = ALT_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let roster_gen = crate::plex::server_roster_gen();
    ALT_ROSTER_GEN.store(roster_gen, Ordering::SeqCst);
    *ALT_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    if guid.is_empty() {
        return; // nothing portable to match on; the panel stays absent
    }
    let others: Vec<crate::plex::ServerId> = crate::plex::server_ids().collect();
    if others.len() < 2 {
        return; // a one-server install pays nothing: no worker, no query, no control
    }
    let (rk, guid) = (rk.to_string(), guid.to_string());
    let n = others.len();
    let _ = crate::task::spawn_small("altsrc", move || {
        let list = catch_unwind(|| resolve_alt_sources(&others, &guid)).unwrap_or_default();
        // The one line that makes this chain debuggable from a device log. A guid is a public
        // metadata id — not an address, a token or a machine — so it is safe to log, and it is the
        // only string that identifies WHICH lookup this was.
        crate::log(&format!("altsrc: asked {n} source(s) for {guid} -> {} copy(ies)", list.len()));
        *ALT_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(AltResult { gen, roster_gen, sid, rk, list });
    });
}

/// WORKER. One `find_by_guid` per source, projected into rows. Pure of app state apart from the
/// registry (an atomic read whose clients are never freed), so it is gradeable against a fixture.
fn resolve_alt_sources(others: &[crate::plex::ServerId], guid: &str) -> Vec<crate::ui::alt_sources::AltCopy> {
    let mut out = Vec::new();
    for &id in others {
        let Some(c) = crate::plex::client_for(id) else { continue };
        // `None` here is "did not answer" and `Some(empty)` is "does not have it" — both contribute
        // no row, but only the second is a fact about the library. They are not collapsed at the
        // client (see `find_by_guid`) so a later revision can say "not reachable" in the panel.
        let Some(mc) = c.find_by_guid(guid) else { continue };
        let handle = crate::plex::server_facts(id).map(|f| f.handle.clone()).unwrap_or_default();
        for m in mc.metadata.iter() {
            let media0 = m.media.first();
            out.push(crate::ui::alt_sources::AltCopy {
                sid: id,
                library: m.library_section_title.clone(),
                // `None` is the ABSENCE of an owner, which is what the row spells "This account";
                // an empty handle must not become `Some("")`.
                owner: (!handle.is_empty()).then(|| handle.clone()),
                rk: m.rating_key.clone(),
                dur_ms: m.duration,
                res: media0.map(|x| x.video_resolution.clone()).unwrap_or_default(),
                width: media0.map(|x| x.width).unwrap_or_default(),
                height: media0.map(|x| x.height).unwrap_or_default(),
            });
        }
    }
    out
}

/// MAIN THREAD, once a frame. Hands a landed cross-source resolve to the panel's store.
pub(crate) fn pump_alt_sources() {
    use std::sync::atomic::Ordering;
    let roster_gen = crate::plex::server_roster_gen();
    if ALT_ROSTER_GEN.swap(roster_gen, Ordering::SeqCst) != roster_gen {
        ALT_GEN.fetch_add(1, Ordering::SeqCst);
        *ALT_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
        crate::ui::alt_sources::prune_inactive();
    }
    let taken = ALT_SLOT.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(r) = taken else { return };
    if r.gen != ALT_GEN.load(Ordering::SeqCst) || r.roster_gen != roster_gen {
        return; // superseded: the page moved on while this was in flight
    }
    // `install` re-checks the (server, rk) PAIR against the page actually mounted — this generation
    // test alone cannot see a page that was opened, left and re-opened between spawn and landing,
    // and the rk alone cannot see the two servers' keys colliding, which they do by default.
    crate::ui::alt_sources::install(r.sid, &r.rk, r.list);
}

/// True while a detail fetch is in flight — drives the detail page's loading spinner.
pub(crate) fn detail_loading() -> bool {
    use std::sync::atomic::Ordering;
    let gen = DETAIL_GEN.load(Ordering::SeqCst);
    gen != 0 && gen != DETAIL_DONE.load(Ordering::SeqCst)
}

// ---- season switching ----------------------------------------------------------------------
// The tab UI's season switch is ASYNC: `load_season` flips `cur_season` optimistically (the tab
// highlight moves at once), fetches the episodes on a worker thread, and `pump_season` (called by
// the detail page once a frame) applies the landed list on the main thread. The blocking
// `/children` GET used to run on the main loop, freezing the UI for every rapid season hop.
// Generations guard against out-of-order landings; results for a different item are discarded.
static SEASON_GEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static SEASON_DONE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
struct SeasonResult {
    gen: u32,
    /// the SERVER the show is on — the other half of `rk`. Without it, hopping from server A's show
    /// page to server B's page with the same rk while a `/children` fetch is in flight installs A's
    /// episode list onto B's page: the generation guard cannot see it (the hop bumped nothing that
    /// distinguishes them) and the rk test passes.
    sid: crate::plex::ServerId,
    rk: String,  // the show the fetch was for
    idx: usize,  // the season it was for
    prev: usize, // the season `cur_season` held before the optimistic flip — restored on failure
    eps: Option<Vec<Episode>>, // None = the fetch failed or panicked — the row keeps its episodes
}
static SEASON_RESULT: std::sync::Mutex<Option<SeasonResult>> = std::sync::Mutex::new(None);

/// Post a finished season fetch to the mailbox. MONOTONE: an older fetch landing late must never
/// clobber a newer result the pump hasn't consumed yet — that lost the newest season forever, and
/// with it the SEASON_DONE catch-up, wedging the loading spinner on. Named rather than inlined in
/// the worker closure for the same reason as `land_detail`: the guard is the one piece of this
/// machinery a test cannot reach through `load_season`.
fn land_season(
    gen: u32,
    sid: crate::plex::ServerId,
    rk: String,
    idx: usize,
    prev: usize,
    eps: Option<Vec<Episode>>,
) {
    let mut slot = SEASON_RESULT.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().map(|r| r.gen < gen).unwrap_or(true) {
        *slot = Some(SeasonResult { gen, sid, rk, idx, prev, eps });
    }
}

/// Invalidate any in-flight/pending season fetch and mark the mailbox settled: bump the generation
/// (so a late async landing is discarded), catch SEASON_DONE up to it (season_loading() → false),
/// and clear the slot. Returns the fresh generation. The ONE place the three season atomics move
/// together — used by the blocking `load_season_now`, and by all three detail entry points (a new
/// item supersedes the old show's pending fetch): `load_detail_now`, `request_detail` (dropping the
/// OLD item's fetch) and `pump_detail` (dropping one issued WHILE the load was in flight).
fn supersede_season() -> u32 {
    use std::sync::atomic::Ordering;
    let gen = SEASON_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    SEASON_DONE.store(gen, Ordering::SeqCst);
    *SEASON_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    gen
}

/// Switch the loaded show to season `idx` (the season tabs): `cur_season` flips immediately, the
/// episodes arrive via [`pump_season`]. Main-thread only (touches CURRENT).
pub(crate) fn load_season(idx: usize) {
    use std::sync::atomic::Ordering;
    // `prev` rides along so a FAILED fetch can put the tab back on the season whose episodes are
    // still listed (see `pump_season`) — the optimistic flip below is what has to be undone.
    // the loaded show's own server, read here on the MAIN thread — a season belongs to the item it
    // hangs off, so this is the one honest source for it (never `plex::current_server()`, which the
    // user may have moved since the page was opened)
    let (sid, rk, season_rk, prev) = match current()
        .and_then(|d| d.seasons.get(idx).map(|s| (d.sid, d.rk.clone(), s.rk.clone(), d.cur_season)))
    {
        Some(t) => t,
        None => return,
    };
    unsafe {
        if let Some(d) = (*addr_of_mut!(CURRENT)).as_mut() {
            d.cur_season = idx;
        }
    }
    let gen = SEASON_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let spawned = crate::task::spawn_small("season", move || {
        // the mailbox is filled OUTSIDE the guard so a panicking fetch still lands — as a
        // FAILURE (None), not as an empty season: a panic is not "this season has no episodes",
        // and otherwise season_loading() would report an in-flight fetch forever
        let eps = catch_unwind(|| fetch_episodes(sid, &season_rk)).unwrap_or(None);
        land_season(gen, sid, rk, idx, prev, eps);
    });
    if !spawned {
        // no worker means nothing will ever land: catch DONE up or the episode row keeps its
        // loading dim + spinner for the rest of the session. `cur_season` already moved, so the
        // tab highlight stays where the user put it and the old episodes stay listed.
        SEASON_DONE.store(gen, Ordering::SeqCst);
    }
}

/// [`load_season`] but BLOCKING — for the page-open paths (`open_rk_season`, and any caller that
/// plays `episodes[0]` right after) where the episode list must be right before the next line
/// runs. Invalidates any in-flight async fetch so a stale landing can't overwrite this one.
pub(crate) fn load_season_now(idx: usize) {
    let _ = catch_unwind(move || {
        let (sid, season_rk) = match current().and_then(|d| d.seasons.get(idx).map(|s| (d.sid, s.rk.clone()))) {
            Some(t) => t,
            None => return,
        };
        // `unwrap_or_default`, NOT propagation: this blocking twin still degrades to an empty
        // list on failure. Making it preserve the previous season would silently change what
        // `open_rk_season`'s chained play of `episodes[0]` launches — the WRONG season's first
        // episode under the requested season's name — and that path has no host coverage and needs
        // the full on-device suite. Deferred deliberately.
        let eps = fetch_episodes(sid, &season_rk).unwrap_or_default();
        supersede_season(); // drop any async fetch in flight; this synchronous list wins
        unsafe {
            if let Some(d) = (*addr_of_mut!(CURRENT)).as_mut() {
                d.episodes = eps;
                d.cur_season = idx;
            }
        }
    });
}

/// True while a season fetch is in flight — drives the episode row's loading dim + spinner.
pub(crate) fn season_loading() -> bool {
    use std::sync::atomic::Ordering;
    let gen = SEASON_GEN.load(Ordering::SeqCst);
    gen != 0 && gen != SEASON_DONE.load(Ordering::SeqCst)
}

/// Main-thread pump: apply a landed season fetch to CURRENT, discarding stale generations (a newer
/// request is in flight) and results for a different item. Returns true when the episode list just
/// changed — the detail page resets its episode focus/scroll on it.
pub(crate) fn pump_season() -> bool {
    use std::sync::atomic::Ordering;
    let res = SEASON_RESULT.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(r) = res else { return false };
    if r.gen != SEASON_GEN.load(Ordering::SeqCst) {
        return false; // superseded — the newer fetch will land after this
    }
    // SETTLE THE SPINNER FIRST — on failure as much as on success. `season_loading()` drives the
    // episode row's loading dim + spinner AND gates `play_episode_at`, so a failure that returned
    // before this store would spin that row and refuse every episode press for the rest of the
    // session.
    SEASON_DONE.store(r.gen, Ordering::SeqCst);
    unsafe {
        let Some(d) = (*addr_of_mut!(CURRENT)).as_mut() else { return false };
        // OWNERSHIP, as the (server, key) PAIR. The rk alone was enough while one machine was
        // reachable; with a share registered, hopping from A's show to B's show with the same rk
        // while a `/children` is in flight passes an rk-only test and installs A's episodes onto
        // B's page.
        if !crate::plex::same_item((d.sid, &d.rk), (r.sid, &r.rk)) {
            return false; // the page moved to another item — not ours to patch
        }
        match r.eps {
            Some(eps) => {
                d.episodes = eps;
                d.cur_season = r.idx;
                true
            }
            None => {
                // THE FETCH FAILED. Keep the episodes already on screen — one transient
                // `/children` failure used to blank a populated row, with no spinner and no error.
                // And put `cur_season` back on the season those episodes belong to: the tab
                // highlight and the row must agree (`play_episode_at` launches `episodes[i]` under
                // whichever tab reads selected), and it is what makes the tab RETRYABLE — both
                // load paths fetch only when the target `!= cur_season`, so a tab left marked
                // selected could never be asked for again.
                d.cur_season = r.prev;
                false
            }
        }
    }
}

#[cfg(test)]
mod marker_tests {
    use super::*;

    fn wire(kind: &str, start: i64, end: i64, is_final: bool) -> crate::plex::Marker {
        crate::plex::Marker {
            kind: kind.to_string(),
            start_time_offset: start,
            end_time_offset: end,
            is_final: is_final as i64,
        }
    }
    /// An episode's markers as the live server actually returns them (2026-07-29): an intro and a
    /// `final` credits marker, in that wire order — credits FIRST, which is why nothing here may
    /// assume the array is sorted by time.
    fn episode_markers() -> Vec<Marker> {
        convert_markers(&[
            wire("credits", 3_065_648, 3_130_720, true),
            wire("intro", 990, 99_625, false),
        ])
    }

    #[test]
    fn only_the_kinds_the_player_acts_on_survive_parsing() {
        let m = episode_markers();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].kind, MarkerKind::Credits);
        assert!(m[0].final_seg);
        assert_eq!(m[1].kind, MarkerKind::Intro);
        assert!(!m[1].final_seg);
        // `commercial` (PMS emits it on recorded content) has no behaviour — it must be DROPPED,
        // not defaulted into one of the two, or the pill would offer to skip an ad break as an intro.
        assert!(convert_markers(&[wire("commercial", 10, 20, false)]).is_empty());
        assert!(convert_markers(&[wire("", 10, 20, false)]).is_empty());
    }

    #[test]
    fn a_degenerate_range_is_dropped_rather_than_offered() {
        // A zero-length or inverted marker would produce a prompt that seeking to `end_ms` can
        // never satisfy — the pill would sit there and the press would do nothing.
        assert!(convert_markers(&[wire("intro", 500, 500, false)]).is_empty());
        assert!(convert_markers(&[wire("intro", 900, 100, false)]).is_empty());
        assert!(convert_markers(&[wire("credits", -5, 100, true)]).is_empty());
    }

    #[test]
    fn the_playhead_selects_the_segment_it_is_inside() {
        let m = episode_markers();
        assert!(marker_at(&m, 0).is_none(), "before the intro starts (it begins at 990ms)");
        assert_eq!(marker_at(&m, 990).unwrap().kind, MarkerKind::Intro, "inclusive at the start");
        assert_eq!(marker_at(&m, 50_000).unwrap().kind, MarkerKind::Intro);
        assert!(marker_at(&m, 99_625).is_none(), "EXCLUSIVE at the end: skipping to it clears the pill");
        assert!(marker_at(&m, 2_000_000).is_none(), "the long middle of the episode");
        assert_eq!(marker_at(&m, 3_065_648).unwrap().kind, MarkerKind::Credits);
        assert!(marker_at(&[], 1234).is_none());
    }

    /// The Plex-Pass-free tail: a server without credits detection ships no markers, so the last
    /// [`TAIL_WINDOW_MS`] of an episode stands in as the credits segment (final, ending at the
    /// duration) — the geometry Up Next arms from. Short items never grow one: a 60s clip
    /// offering "next" for half its runtime is worse than no offer.
    #[test]
    fn the_tail_window_stands_in_for_undetected_credits() {
        let dur = 22 * 60 * 1000; // a 22-minute episode
        assert!(tail_marker(0, dur).is_none(), "the open");
        assert!(tail_marker(dur - TAIL_WINDOW_MS - 1, dur).is_none(), "one ms before the window");
        let m = tail_marker(dur - TAIL_WINDOW_MS, dur).expect("inclusive at the window edge");
        assert_eq!((m.kind, m.final_seg), (MarkerKind::Credits, true));
        assert_eq!((m.start_ms, m.end_ms), (dur - TAIL_WINDOW_MS, dur));
        assert!(tail_marker(dur - 1, dur).is_some(), "the last frame still offers it");
        assert!(tail_marker(80_000, 89_999).is_none(), "short items never grow a tail");
        assert!(tail_marker(0, 0).is_none(), "no duration published (a transcode mid-probe)");
    }

    #[test]
    fn a_final_credits_marker_holds_past_its_stated_end() {
        // PMS sets a `final` marker's end to the CONTAINER duration, but our playhead is the
        // decoder's and routinely stops short of it — an exclusive end there made the pill blink
        // out over the last frames, exactly when it is being reached for.
        let m = episode_markers();
        assert!(marker_at(&m, 3_130_720).is_some(), "at the stated end");
        assert!(marker_at(&m, 3_130_720 + 5_000).is_some(), "and past it");

        // A NON-final credits marker (credits before a post-credits scene) must still end, or
        // playback past it would keep offering a skip for a segment already behind the playhead.
        let mid = convert_markers(&[wire("credits", 1000, 2000, false)]);
        assert!(marker_at(&mid, 1500).is_some());
        assert!(marker_at(&mid, 2000).is_none(), "a non-final segment ends where it says it does");
    }
}

#[cfg(test)]
mod episode_tests {
    use super::*;

    /// One `/library/metadata/{season}/children` row, shaped the way PMS actually sends one — the
    /// counters STRING-encoded, which is the form `models.rs`'s lenient `de_i64` exists for. Goes
    /// through serde on purpose rather than hand-building a `Metadata`: the DTO field and the
    /// mapping are the two halves of this gap, and a hand-built struct would only ever exercise
    /// the half that was already right.
    fn row(extra: &str) -> crate::plex::Metadata {
        let json = format!(
            r#"{{"type":"episode","ratingKey":"1804","index":"3","parentIndex":"2",
                 "title":"Ep","duration":"3000000"{extra}}}"#
        );
        serde_json::from_str(&json).expect("a /children row parses")
    }

    /// The gap this closes: `viewCount` was parsed at the DTO and then never copied onto
    /// [`Episode`], so a fully-watched episode and one never started carried identical values all
    /// the way to the filmstrip. No `testlock` here — `convert_episode` is pure and reads no
    /// crate global.
    #[test]
    fn view_count_on_the_wire_becomes_the_episode_watched_flag() {
        // ABSENT is the unwatched case: PMS omits the key entirely rather than sending 0, which is
        // why the flag can be a presence test and why a missing field must default to false.
        let e = convert_episode(&row(""));
        assert!(!e.watched, "an absent viewCount is unwatched");
        assert_eq!(e.resume_ms, 0, "and carries no resume point");
        assert_eq!((e.rk.as_str(), e.index, e.season), ("1804", 3, 2), "the rest still maps");

        // …and a literal 0 is unwatched too. PMS omits the key rather than sending this, but
        // `de_i64` would deliver a real 0, so the flag must be a THRESHOLD and not a presence test
        // on the JSON — the two only agree while the server keeps omitting.
        assert!(!convert_episode(&row(r#","viewCount":0"#)).watched, "an explicit 0 is unwatched");

        assert!(convert_episode(&row(r#","viewCount":"1""#)).watched, "watched once");
        assert!(convert_episode(&row(r#","viewCount":4"#)).watched, "and re-watched, sent numeric");

        // Watched AND resuming is a real server state — finished, then started again. Both must
        // survive the mapping: the mutual exclusion is a rule of the DRAW site (which shows the
        // resume bar over the check), so collapsing it here would silently lose the resume point.
        let both = convert_episode(&row(r#","viewCount":"1","viewOffset":"120000""#));
        assert!(both.watched, "a re-started episode is still watched");
        assert_eq!(both.resume_ms, 120_000, "and keeps the resume point the player needs");
    }
}

#[cfg(test)]
mod rating_tests {
    use super::*;

    /// one `Rating[]` row on the wire
    fn wire(image: &str, value: f64, kind: &str) -> crate::plex::Rating {
        crate::plex::Rating { image: image.to_string(), value, kind: kind.to_string() }
    }
    fn arts(v: &[Rating]) -> Vec<RatingArt> {
        v.iter().map(|r| r.art).collect()
    }

    /// The `image` string picks the ARTWORK — provider AND state — and the score never does. Both
    /// halves matter: a threshold would put a fresh tomato on 6.0 (on the live server one item is
    /// 4.0 and ROTTEN while another is 6.0 and RIPE), and a provider read off `type`
    /// would be wrong for IMDb and TMDB, which both arrive as `audience`.
    #[test]
    fn image_string_picks_the_provider_and_the_state() {
        use RatingArt::*;
        for (image, want) in [
            ("rottentomatoes://image.rating.ripe", TomatoFresh),
            ("rottentomatoes://image.rating.certified", TomatoCertified),
            ("rottentomatoes://image.rating.rotten", TomatoRotten),
            ("rottentomatoes://image.rating.upright", PopcornUpright),
            ("rottentomatoes://image.rating.spilled", PopcornSpilled),
            ("imdb://image.rating", Imdb),
            ("themoviedb://image.rating", Tmdb),
        ] {
            assert_eq!(RatingArt::from_image(image), Some(want), "{image}");
        }
        // the negative variants are a different MARK, not the same mark recoloured, so nothing may
        // collapse ripe→rotten or upright→spilled
        assert_ne!(RatingArt::from_image("rottentomatoes://image.rating.ripe"), RatingArt::from_image("rottentomatoes://image.rating.rotten"));
        assert_ne!(RatingArt::from_image("rottentomatoes://image.rating.upright"), RatingArt::from_image("rottentomatoes://image.rating.spilled"));
    }

    /// All FIVE Rotten Tomatoes states are distinct art, and `certified` is not an alias for
    /// `ripe`: Certified Fresh is a rarer, higher bar that the server takes the trouble to name,
    /// and it has its own mark (the wreathed tomato). It stays in the critic SLOT, so its rank
    /// still collides with the other tomatoes and `convert_ratings` cannot draw two of them.
    #[test]
    fn certified_fresh_is_its_own_mark_in_the_tomato_slot() {
        let five = [
            RatingArt::from_image("rottentomatoes://image.rating.ripe"),
            RatingArt::from_image("rottentomatoes://image.rating.certified"),
            RatingArt::from_image("rottentomatoes://image.rating.rotten"),
            RatingArt::from_image("rottentomatoes://image.rating.upright"),
            RatingArt::from_image("rottentomatoes://image.rating.spilled"),
        ];
        for (i, a) in five.iter().enumerate() {
            assert!(a.is_some(), "state {i} unparsed");
            for (j, b) in five.iter().enumerate() {
                assert_eq!(i == j, a == b, "states {i} and {j} must differ");
            }
        }
        assert_eq!(RatingArt::TomatoCertified.rank(), RatingArt::TomatoFresh.rank());
        // …so an item that somehow carried both critic tomatoes still badges exactly one
        let it = crate::plex::Metadata {
            ratings: vec![
                wire("rottentomatoes://image.rating.certified", 9.4, "critic"),
                wire("rottentomatoes://image.rating.ripe", 9.4, "critic"),
            ],
            ..Default::default()
        };
        assert_eq!(arts(&convert_ratings(&it)), [RatingArt::TomatoCertified]);
    }

    /// The state is the LAST dot-separated segment, the way Plex's own bundle reads it
    /// (`t.substr(t.lastIndexOf(".") + 1)`) — not a prefix or a `contains`. A mark chosen by
    /// substring would answer `…rating.ripeness` (or a future `…rating.rotten_v2`) with art the
    /// server never asked for, and this parse is the ONLY thing standing between the server's
    /// verdict and the wrong tomato on the hero.
    #[test]
    fn the_state_is_the_last_dot_segment_only() {
        for image in [
            "rottentomatoes://image.rating.ripeness", // ripe is a PREFIX of it, not the segment
            "rottentomatoes://image.ripe.rating",     // right word, wrong (non-final) position
            "rottentomatoes://ripe",                  // no dot at all → no segment to read
            "rottentomatoes://image.rating.CERTIFIED", // states arrive lower-case; no case-folding
        ] {
            assert_eq!(RatingArt::from_image(image), None, "{image}");
        }
        // a deeper path still resolves on its final segment
        assert_eq!(
            RatingArt::from_image("rottentomatoes://image.rating.tomato.spilled"),
            Some(RatingArt::PopcornSpilled)
        );
    }

    /// Anything we cannot attribute is dropped rather than guessed at: an unknown provider, a
    /// Rotten Tomatoes string with no state (tomato or popcorn? the string does not say), and
    /// junk that is not a `scheme://path` at all.
    #[test]
    fn an_unattributable_image_yields_no_badge() {
        for image in [
            "metacritic://image.rating",
            "rottentomatoes://image.rating",
            "rottentomatoes://image.rating.mouldy",
            "imdb", // no "://" — a truncated/blank field must not panic or match
            "",
            "://image.rating",
        ] {
            assert_eq!(RatingArt::from_image(image), None, "{image}");
        }
    }

    /// `Rating[]` wins whenever present — it is the superset and the only form that names each
    /// score's provider — and the row is ordered critic-tomato → audience-popcorn → IMDb → TMDB
    /// regardless of the wire order (PMS sends it alphabetically by provider).
    #[test]
    fn the_array_wins_over_the_flat_pair_and_orders_the_row() {
        // Luca, verbatim off the live server 2026-07-29
        let it = crate::plex::Metadata {
            ratings: vec![
                wire("imdb://image.rating", 7.4, "audience"),
                wire("rottentomatoes://image.rating.ripe", 9.1, "critic"),
                wire("rottentomatoes://image.rating.upright", 8.5, "audience"),
                wire("themoviedb://image.rating", 7.8, "audience"),
            ],
            // the flat pair is also on the wire for this item; the array must win
            rating: 9.1,
            rating_image: "rottentomatoes://image.rating.ripe".to_string(),
            audience_rating: 8.5,
            audience_rating_image: "rottentomatoes://image.rating.upright".to_string(),
            ..Default::default()
        };
        let got = convert_ratings(&it);
        use RatingArt::*;
        // IMDb leads, then RT's critic tomato, its audience popcorn, and TMDB — see `RatingArt::rank`
        assert_eq!(arts(&got), [Imdb, TomatoFresh, PopcornUpright, Tmdb]);
        assert_eq!(got[0].value, 7.4, "IMDb's score, on its own /10 scale");
        assert!(got[1].critic, "the tomato is the critic score");
        assert!(!got[0].critic, "IMDb arrives as an audience score, not a critic one");
    }

    /// The flat pair is the fallback for the OTHER response shape — a section listing carries it
    /// and no `Rating[]` at all (verified live 2026-07-29). Nothing calls `convert_ratings` on that
    /// shape yet, so this test is currently the branch's only exercise; it is here so the fallback
    /// cannot rot before the first grid-side caller arrives.
    #[test]
    fn the_flat_pair_is_used_when_the_array_is_absent() {
        let it = crate::plex::Metadata {
            rating: 4.0,
            rating_image: "rottentomatoes://image.rating.rotten".to_string(),
            audience_rating: 8.3,
            audience_rating_image: "rottentomatoes://image.rating.upright".to_string(),
            ..Default::default()
        };
        let got = convert_ratings(&it);
        use RatingArt::*;
        assert_eq!(arts(&got), [TomatoRotten, PopcornUpright]);
        assert!(got[0].critic && !got[1].critic);
    }

    /// A score PMS never sent defaults to 0.0, which means ABSENT — badging it as "0%" would
    /// invent a review. An unattributable row must also not take its neighbours down with it.
    #[test]
    fn absent_scores_and_unknown_providers_drop_out() {
        let it = crate::plex::Metadata {
            ratings: vec![
                wire("rottentomatoes://image.rating.ripe", 0.0, "critic"), // absent
                wire("metacritic://image.rating", 8.8, "critic"),          // unknown provider
                wire("imdb://image.rating", 7.4, "audience"),              // the one real row
            ],
            ..Default::default()
        };
        assert_eq!(arts(&convert_ratings(&it)), [RatingArt::Imdb]);

        // nothing usable at all → an empty row, and the hero simply draws no badges
        let empty = crate::plex::Metadata { rating: 9.1, ..Default::default() }; // score, no image
        assert!(convert_ratings(&empty).is_empty());
    }

    /// One badge per slot. Two rows behind the same mark cannot both be drawn — the row would show
    /// one provider twice with two different numbers — so the second is dropped after the
    /// critic-first sort has decided which one that is.
    #[test]
    fn a_slot_is_only_badged_once() {
        let it = crate::plex::Metadata {
            ratings: vec![
                wire("rottentomatoes://image.rating.upright", 8.5, "audience"),
                // a contradictory second critic row: ripe AND rotten for the same item
                wire("rottentomatoes://image.rating.rotten", 4.0, "critic"),
                wire("rottentomatoes://image.rating.ripe", 9.1, "critic"),
            ],
            ..Default::default()
        };
        let got = convert_ratings(&it);
        assert_eq!(arts(&got), [RatingArt::TomatoRotten, RatingArt::PopcornUpright]);
        assert_eq!(got[0].value, 4.0, "wire order decides between two equally-ranked critic rows");
    }

    /// PMS normalises every provider onto 0–10; the badge puts the number back into the units its
    /// provider actually publishes, or a 9.1 tomato reads as a 9.1% score.
    #[test]
    fn a_score_is_formatted_in_its_provider_s_own_units() {
        use crate::ui::fmt::rating_score;
        assert_eq!(rating_score(RatingArt::TomatoFresh, 9.1), "91%");
        assert_eq!(rating_score(RatingArt::PopcornSpilled, 4.05), "41%"); // rounded, not truncated
        assert_eq!(rating_score(RatingArt::Tmdb, 7.8), "78%");
        assert_eq!(rating_score(RatingArt::Imdb, 7.4), "7.4");
        assert_eq!(rating_score(RatingArt::TomatoFresh, 10.0), "100%");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    // ---- convert_streams: the Dolby Vision record's survival ------------------------------

    fn video_stream(dovi: Option<(i64, i64, i64)>) -> crate::plex::Stream {
        let (present, profile, compat, el) = match dovi {
            Some((profile, compat, el)) => (1, profile, compat, el),
            None => (0, 0, 0, 0),
        };
        crate::plex::Stream {
            stream_type: 1,
            codec: "hevc".into(),
            dovi_present: present,
            dovi_profile: profile,
            dovi_bl_compat_id: compat,
            dovi_el_present: el,
            ..Default::default()
        }
    }

    /// **A Dolby Vision record must survive a second video stream that has none.** `fps` and `hdr`
    /// take the LAST `streamType: 1` stream in the part and that is harmless for both; the DV
    /// record is read by `route::video_direct_plays`, so blanking it back to the all-zero default
    /// re-opens the direct-play gate and the file plays in the wrong colours — the exact bug the
    /// gate exists for. No part on the dev server carries two video streams today (all 540 leaves
    /// swept 2026-08-21), which is precisely why this is a test and not a measurement: embedded
    /// cover art is an ordinary thing for a library to contain and nothing else here would notice.
    #[test]
    fn a_dolby_vision_record_is_not_erased_by_a_later_video_stream() {
        let p5 = video_stream(Some((5, 0, 0)));
        let cover = video_stream(None);
        let dovi = convert_streams(&[p5, cover]).dovi;
        assert!(dovi.present, "the P5 record must outlive a second video stream");
        assert_eq!(dovi.profile, 5);
        assert!(dovi.base_layer_unusable(), "and must still forbid a server-side COPY of it");
        // …and, undeclared, must still refuse direct play — the record surviving is what both of
        // those turn on, so the cover-art stream must not be able to blank it
        assert_eq!(
            dovi.presentation(false),
            crate::metadata::DvPresentation::Refuse("no cross-compatible base layer")
        );
        assert_eq!(dovi.presentation(true).declared().map(|n| n.profile_id), Some(5));
    }

    /// The ordinary single-video-stream shapes, so the guard above cannot be read as "any DV
    /// record anywhere wins": a part with no Dolby Vision at all still produces the all-zero
    /// record that refuses nothing.
    #[test]
    fn a_part_with_no_dolby_vision_reports_no_record() {
        let s = convert_streams(&[video_stream(None)]);
        let (hdr, dovi) = (s.hdr, s.dovi);
        assert_eq!(dovi, Dovi::default());
        assert!(!dovi.base_layer_unusable());
        assert!(!hdr, "no DV and no PQ/HLG transfer is not HDR");
    }

    /// post through the REAL mailbox write, so the monotone guard is under test rather than
    /// bypassed (an unconditional store here would make the "older lands late" case vacuous)
    fn landing(gen: u32, rk: &str) {
        land_detail(gen, Some(Detail { rk: rk.to_string(), ..Default::default() }));
    }
    fn slot_rk() -> Option<String> {
        DETAIL_SLOT.lock().unwrap().as_ref().and_then(|r| r.d.as_ref()).map(|d| d.rk.clone())
    }
    fn cur_rk() -> Option<String> {
        current().map(|d| d.rk.clone())
    }

    /// **The cross-source projection, on the real measured shape.** A `/library/all?guid=…` answer
    /// is the OTHER server's own row: its own `ratingKey`, its own library, and — measured against
    /// this household's two servers on 2026-08-14 — its own localized title for the same film.
    /// Everything a row needs must come off that answer, because nothing about the page we are on
    /// describes the copy over there.
    ///
    /// Pure: no statics, no socket, so no serial lock. It grades `resolve_alt_sources`'s projection
    /// by feeding the container directly, which is the half that decides whether the panel offers
    /// the right film.
    #[test]
    fn a_guid_answer_projects_the_other_servers_own_key_library_and_class() {
        let body = r#"{"MediaContainer":{"size":1,"Metadata":[{
            "ratingKey":"5274","type":"movie","title":"another title entirely",
            "guid":"plex://movie/6856893830a4aaafd5c4291d","librarySectionTitle":"Film Club",
            "duration":7020000,"Media":[{"videoResolution":"1080","width":1920,"height":1080}]}]}}"#;
        let mc = serde_json::from_str::<crate::plex::Envelope>(body).expect("parses").media_container;

        let m = mc.metadata.first().expect("one row");
        assert_eq!(m.guid, "plex://movie/6856893830a4aaafd5c4291d", "the portable identity is read");
        assert_eq!(m.rating_key, "5274", "…and the key is THEIRS, not ours");
        assert_eq!(m.library_section_title, "Film Club", "the library names the row, not the machine");
        assert_eq!(m.duration, 7_020_000);
        assert_eq!(m.media.first().map(|x| x.video_resolution.as_str()), Some("1080"));
    }

    /// **The Related shelf's rows carry the watch state that was on the wire all along.**
    ///
    /// The reported bug — a long press on a Related tile doing nothing — was explained by "a Related
    /// row has no `(ratingKey, watched)` pair to build menu rows from", and that was true of the
    /// old three-field struct while being false of the response. This is the test that keeps the two
    /// from drifting apart again: it feeds `/related`'s REAL shape and asserts the fields the shelf's
    /// tick, its resume bar and its context menu are each built from.
    ///
    /// Three details are deliberately in the fixture rather than idealised away:
    /// * `viewOffset`/`duration` arrive as JSON **strings**, which PMS really does (see
    ///   `plex/CLAUDE.md` — a non-lenient adapter fails the WHOLE container, not one field);
    /// * `viewCount` is **absent** on an unwatched row rather than `0`;
    /// * the show is **part-watched** (`viewedLeafCount < leafCount`), the state that is neither
    ///   watched nor unwatched and the one a `viewCount > 0` shortcut gets wrong.
    #[test]
    fn related_rows_carry_the_watch_state_the_wire_already_had() {
        let body = r#"{"MediaContainer":{"Hub":[{"title":"Similar Movies","Metadata":[
            {"ratingKey":"11","type":"movie","title":"finished","duration":"7020000","viewCount":2,
             "Media":[{"Part":[{"key":"/library/parts/11/file.mkv"}]}]},
            {"ratingKey":"12","type":"movie","title":"halfway","duration":"7020000","viewOffset":"3510000"},
            {"ratingKey":"13","type":"movie","title":"never started","duration":7020000},
            {"ratingKey":"14","type":"show","title":"three in","leafCount":10,"viewedLeafCount":3}
        ]}]}}"#;
        let mc = serde_json::from_str::<crate::plex::Envelope>(body).expect("parses").media_container;
        let rows = related_rows(&mc, SRV_B);
        assert_eq!(rows.len(), 4, "every hub row with a key becomes a tile");

        // …and every row is stamped with the server it was FETCHED from. A related item is a key on
        // the page's own server, and both servers number from 1, so this is the field that keeps the
        // art request, the menu's SID and the scrobble off the wrong machine.
        assert!(rows.iter().all(|m| m.sid == SRV_B), "the row's server is the one that answered");

        // finished: the tick, and no bar (`resume_frac` is None with no viewOffset)
        assert!(rows[0].watched && !rows[0].unwatched);
        assert_eq!(rows[0].resume_frac(), None);
        assert_eq!(rows[0].part, "/library/parts/11/file.mkv", "Play from Start needs the part id");

        // halfway: the bar, at the fraction the wire's STRING-encoded numbers give
        assert!(!rows[1].watched && rows[1].unwatched, "a resume point is not a view count");
        assert_eq!(rows[1].resume_frac(), Some(0.5), "the amber bar's fraction, off duration + viewOffset");

        // never started: neither mark — and `viewCount` was absent, not zero
        assert!(!rows[2].watched && rows[2].unwatched);
        assert_eq!(rows[2].resume_frac(), None);

        // the part-watched SHOW: NEITHER flag, which is the state the menu turns into both verbs
        assert_eq!(rows[3].kind, 1, "the item KIND decides the menu's leaf/container rule");
        assert!(!rows[3].watched, "3 of 10 leaves is not done");
        assert!(!rows[3].unwatched, "…and it is not untouched either");
    }

    /// The two bounds on the shelf, which are one function's job and were easy to lose in the move
    /// to the shared row mapping.
    ///
    /// **De-duplication is across the whole response, not per hub** — PMS's related hubs overlap
    /// heavily, so the same film is routinely in two of them and a flattened strip would draw it
    /// twice side by side. **The cap counts kept rows**, so a response padded with duplicates cannot
    /// spend the budget on tiles that were never added.
    #[test]
    fn related_rows_dedupe_across_hubs_and_cap_the_shelf() {
        let hub = |keys: &[i32]| {
            let rows: Vec<String> = keys
                .iter()
                .map(|k| format!(r#"{{"ratingKey":"{k}","type":"movie","title":"t{k}"}}"#))
                .collect();
            format!(r#"{{"Metadata":[{}]}}"#, rows.join(","))
        };
        // the same three keys in two hubs, plus one the second hub alone has
        let body = format!(
            r#"{{"MediaContainer":{{"Hub":[{},{}]}}}}"#,
            hub(&[1, 2, 3]),
            hub(&[2, 3, 4])
        );
        let mc = serde_json::from_str::<crate::plex::Envelope>(&body).expect("parses").media_container;
        let rows = related_rows(&mc, SRV_A);
        let keys: Vec<&str> = rows.iter().map(|m| m.rk.as_str()).collect();
        assert_eq!(keys, ["1", "2", "3", "4"], "one tile per title, in first-seen order");

        // a row PMS sent no key for is not a tile — it addresses nothing
        let body = r#"{"MediaContainer":{"Hub":[{"Metadata":[{"type":"movie","title":"keyless"}]}]}}"#;
        let mc = serde_json::from_str::<crate::plex::Envelope>(body).expect("parses").media_container;
        assert!(related_rows(&mc, SRV_A).is_empty(), "no ratingKey, no tile");

        // the cap, counted in KEPT rows: 30 distinct keys, each repeated twice
        let many: Vec<i32> = (0..30).collect();
        let body = format!(r#"{{"MediaContainer":{{"Hub":[{},{}]}}}}"#, hub(&many), hub(&many));
        let mc = serde_json::from_str::<crate::plex::Envelope>(&body).expect("parses").media_container;
        let rows = related_rows(&mc, SRV_A);
        assert_eq!(rows.len(), RELATED_MAX, "the shelf is capped");
        assert_eq!(rows.last().map(|m| m.rk.as_str()), Some("19"), "…at the 20th DISTINCT title");
    }

    /// A server that answers "I do not have it" contributes no row — and is not confused with one
    /// that did not answer. Both yield nothing here; only the client keeps them apart (see
    /// `find_by_guid`), which is what lets a later revision say "not reachable" in the panel.
    #[test]
    fn a_server_without_the_film_contributes_no_row() {
        let mc = serde_json::from_str::<crate::plex::Envelope>(r#"{"MediaContainer":{"size":0}}"#)
            .expect("parses")
            .media_container;
        assert!(mc.metadata.is_empty(), "size=0 is an answer, and it is an empty one");
    }

    /// **The cross-source resolve carries its SERVER through the mailbox, and the pump hands both
    /// halves to the panel.** The generation guard beside it cannot stand in for this: `ALT_GEN`
    /// only moves when a DETAIL lands, so a page opened while a resolve is out — the whole reason
    /// this is asynchronous, since one dead share costs a `connect(2)` timeout — is a page whose
    /// own detail is still in flight, and the landing sails through the generation test. The rk
    /// then matched too, because both servers number their items from 1: the panel listed the
    /// other machine's copies and OK on one opened a different film.
    ///
    /// Drives the real `pump_alt_sources` (the mailbox is filled directly, as the detail test does
    /// for its failure case — there is no `land_alt` for a test to reach) and grades the panel's
    /// own gate, which is the thing a user would see appear or not appear.
    #[test]
    fn an_alt_sources_landing_for_another_servers_copy_with_the_same_key_is_refused() {
        let _serial = crate::testlock::serial();
        use crate::ui::alt_sources::AltCopy;
        // two copies on two sources — enough for `is_available`, which counts distinct SOURCES
        let copies = || {
            vec![
                AltCopy { sid: SRV_A, rk: "4".into(), ..Default::default() },
                AltCopy { sid: SRV_B, rk: "318".into(), ..Default::default() },
            ]
        };
        let land = |gen: u32, sid: crate::plex::ServerId, rk: &str| {
            ALT_ROSTER_GEN.store(crate::plex::server_roster_gen(), Ordering::SeqCst);
            *ALT_SLOT.lock().unwrap() = Some(AltResult {
                gen,
                roster_gen: crate::plex::server_roster_gen(),
                sid,
                rk: rk.to_string(),
                list: copies(),
            });
        };

        // our film 4 is the mounted page and its resolve is out…
        crate::ui::alt_sources::reset(SRV_A, "4");
        let gen = ALT_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        // …and while it is out the user lands on the SHARE's film 4
        crate::ui::alt_sources::reset(SRV_B, "4");
        land(gen, SRV_A, "4");
        pump_alt_sources();
        assert!(!crate::ui::alt_sources::is_available(), "our copies are not news about the share's film");

        // the control: the very same landing DOES reach the panel while the page is still ours
        crate::ui::alt_sources::reset(SRV_A, "4");
        let gen = ALT_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        land(gen, SRV_A, "4");
        pump_alt_sources();
        assert!(crate::ui::alt_sources::is_available(), "the awaited landing installs");

        // …and a SUPERSEDED landing is still dropped one layer earlier, by the generation
        let stale = ALT_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        ALT_GEN.fetch_add(1, Ordering::SeqCst);
        crate::ui::alt_sources::reset(SRV_A, "4");
        land(stale, SRV_A, "4");
        pump_alt_sources();
        assert!(!crate::ui::alt_sources::is_available(), "a landing from a superseded resolve is dropped");

        crate::ui::alt_sources::reset(crate::plex::ServerId::UNSET, "");
    }

    #[test]
    fn an_alt_source_from_a_revoked_slot_is_pruned_and_its_inflight_result_is_discarded() {
        let _serial = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let a = crate::plex::register_for_test("alt-a", "127.0.0.1", 1, "a", "cid");
        let b = crate::plex::register_for_test("alt-b", "127.0.0.1", 2, "b", "cid");
        let copies = vec![
            crate::ui::alt_sources::AltCopy { sid: a, rk: "4".into(), ..Default::default() },
            crate::ui::alt_sources::AltCopy { sid: b, rk: "9".into(), ..Default::default() },
        ];
        crate::ui::alt_sources::reset(a, "4");
        crate::ui::alt_sources::install(a, "4", copies.clone());
        assert!(crate::ui::alt_sources::is_available());

        let old_roster = crate::plex::server_roster_gen();
        ALT_ROSTER_GEN.store(old_roster, Ordering::SeqCst);
        let gen = ALT_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        *ALT_SLOT.lock().unwrap() = Some(AltResult { gen, roster_gen: old_roster, sid: a, rk: "4".into(), list: copies });
        crate::plex::revoke_for_profile_switch();
        crate::plex::register_for_test("alt-c", "127.0.0.1", 3, "c", "cid");

        pump_alt_sources();
        assert!(!crate::ui::alt_sources::is_available(), "the removed source neither stays cached nor re-lands");

        crate::ui::alt_sources::reset(crate::plex::ServerId::UNSET, "");
        crate::plex::reset_servers_for_test();
    }

    /// The whole detail mailbox in one serial test — the statics are global, so splitting this
    /// into parallel #[test]s would have them racing each other rather than the code.
    #[test]
    fn a_detail_landing_only_installs_while_it_is_still_the_one_being_awaited() {
        let _serial = crate::testlock::serial();
        // idle: nothing requested, nothing loading, nothing to pump
        assert!(!detail_loading(), "a fresh process is not loading anything");
        assert!(!pump_detail(), "an empty mailbox pumps nothing");

        // a request is in flight until its landing is pumped
        let gen = DETAIL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(detail_loading(), "a bumped generation with DONE behind it reads as in flight");
        landing(gen, "movie-1");
        assert!(pump_detail(), "the awaited landing installs");
        assert_eq!(cur_rk().as_deref(), Some("movie-1"));
        assert!(!detail_loading(), "pumping the landing settles the spinner");

        // SUPERSEDED: a second request means the first one's landing is stale and must be dropped
        let old = DETAIL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        let new = DETAIL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        landing(old, "stale-show");
        assert!(!pump_detail(), "a landing from a superseded generation is discarded");
        assert_eq!(cur_rk().as_deref(), Some("movie-1"), "and it must not touch CURRENT");
        assert!(detail_loading(), "the NEWER request is still in flight");

        // MONOTONE mailbox: with the newer result already sitting unconsumed, the OLDER fetch
        // finally returns — it must not overwrite it. (This is the case that wedged the season
        // mailbox before its guard existed: losing the newest result stalled the spinner on.)
        landing(new, "fresh-show");
        landing(old, "stale-show");
        assert_eq!(slot_rk().as_deref(), Some("fresh-show"), "the late older landing is refused");
        assert!(pump_detail());
        assert_eq!(cur_rk().as_deref(), Some("fresh-show"));

        // a FAILED fetch (None) settles the spinner but keeps the previously loaded item
        let g = DETAIL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        *DETAIL_SLOT.lock().unwrap() = Some(DetailResult { gen: g, d: None });
        assert!(!pump_detail(), "a failed fetch reports no fresh item");
        assert_eq!(cur_rk().as_deref(), Some("fresh-show"), "and leaves the page as it was");
        assert!(!detail_loading(), "but it does settle the spinner");

        // CLOSING THE PAGE supersedes: a load requested on the way in must not repopulate
        // CURRENT behind whatever screen is mounted now.
        let inflight = DETAIL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        clear();
        assert!(!detail_loading(), "clear() settles the in-flight fetch");
        landing(inflight, "arrived-after-close");
        assert!(!pump_detail(), "a landing after close is dropped");
        assert_eq!(cur_rk(), None, "the page stays closed");
    }

    // ---- the season mailbox -----------------------------------------------------------------

    /// A two-season show with a populated episode row, as a landed detail fetch leaves it. Written
    /// straight into CURRENT rather than through `pump_detail` — that pump is the other test's
    /// subject, and routing through it would couple the two.
    /// Two registry slots — plain values, so the identity rules are gradeable without a registry.
    /// `SRV_A` stands in for the signed-in user's own server, `SRV_B` for a share.
    const SRV_A: crate::plex::ServerId = crate::plex::ServerId::from_raw(0);
    const SRV_B: crate::plex::ServerId = crate::plex::ServerId::from_raw(1);

    fn install_show(rk: &str, cur: usize, eps: &[&str]) {
        install_show_on(SRV_A, rk, cur, eps);
    }
    fn install_show_on(sid: crate::plex::ServerId, rk: &str, cur: usize, eps: &[&str]) {
        unsafe {
            *addr_of_mut!(CURRENT) = Some(Detail {
                sid,
                rk: rk.to_string(),
                is_show: true,
                seasons: vec![
                    Season { rk: "sk1".to_string(), index: 1, title: "Season 1".to_string(), leaf_count: 0, viewed_leaf_count: 0 },
                    Season { rk: "sk2".to_string(), index: 2, title: "Season 2".to_string(), leaf_count: 0, viewed_leaf_count: 0 },
                ],
                episodes: eps.iter().map(|e| episode(e)).collect(),
                cur_season: cur,
                ..Default::default()
            })
        };
    }
    fn episode(rk: &str) -> Episode {
        Episode { rk: rk.to_string(), ..Default::default() }
    }
    fn listed_eps() -> Vec<String> {
        current().map(|d| d.episodes.iter().map(|e| e.rk.clone()).collect()).unwrap_or_default()
    }
    /// which season tab reads *selected* — the tabs pill `d.cur_season`; the focus ring is a
    /// separate, view-local column
    fn selected_tab() -> usize {
        current().map(|d| d.cur_season).unwrap_or(usize::MAX)
    }
    /// arm a season switch exactly as `load_season` does — flip the tab optimistically, then take
    /// the generation. Hands back what the worker carries to `land_season`.
    fn begin_switch(to: usize) -> (u32, usize) {
        let prev = selected_tab();
        unsafe {
            if let Some(d) = (*addr_of_mut!(CURRENT)).as_mut() {
                d.cur_season = to;
            }
        }
        (SEASON_GEN.fetch_add(1, Ordering::SeqCst) + 1, prev)
    }

    /// The whole season mailbox in one serial test — same shape and same reason as the detail one
    /// above: the statics are global, so splitting this into parallel `#[test]`s would have them
    /// racing each other rather than the code.
    ///
    /// The FIRST block is the audit finding: `fetch_episodes` returned an empty Vec for BOTH "this
    /// season has no episodes" and "the `/children` GET failed", and `pump_season` installed it
    /// either way — so one transient PMS failure blanked a populated episode row, with no spinner
    /// and no error, onto a tab that could then never be asked again. The blocks after it cover the
    /// supersede / monotone / wrong-item guards this change rewrites; on their own they would pass
    /// before and after, which is why they live inside the failing test rather than beside it.
    #[test]
    fn a_season_landing_only_installs_while_it_is_still_the_one_being_awaited() {
        let _serial = crate::testlock::serial();

        // A FAILED /children GET. It must not be mistaken for a season with no episodes.
        install_show("show-1", 0, &["s1e1", "s1e2"]);
        let (gen, prev) = begin_switch(1);
        assert_eq!(selected_tab(), 1, "the tab flips optimistically while the fetch is in flight");
        assert!(season_loading(), "a bumped generation with DONE behind it reads as in flight");
        land_season(gen, SRV_A, "show-1".to_string(), 1, prev, None);
        assert!(!pump_season(), "a failed fetch is not a new episode list");
        assert_eq!(listed_eps(), ["s1e1", "s1e2"], "the populated row survives the failure");
        assert_eq!(selected_tab(), 0, "the failed tab is released, so focusing it again refetches");
        assert!(!season_loading(), "the episode row must still come out of its loading state");

        // A season that GENUINELY has no episodes is a SUCCESS: the row clears. This is why the
        // discriminant is an Option and not an `is_empty()` check — a "keep the old list whenever
        // the new one is empty" fix passes the block above and leaves THIS one showing the
        // previous season's episodes under the new season's tab.
        let (gen, prev) = begin_switch(1);
        land_season(gen, SRV_A, "show-1".to_string(), 1, prev, Some(Vec::new()));
        assert!(pump_season(), "an empty season is a successful fetch — the row did change");
        assert!(listed_eps().is_empty(), "and the previous season's episodes are gone");
        assert_eq!(selected_tab(), 1, "the tab stays on the season that answered");

        // the ordinary success path
        let (gen, prev) = begin_switch(0);
        land_season(gen, SRV_A, "show-1".to_string(), 0, prev, Some(vec![episode("s1e1")]));
        assert!(pump_season());
        assert_eq!(listed_eps(), ["s1e1"]);
        assert_eq!(selected_tab(), 0);

        // SUPERSEDED: a blocking `load_season_now`, or a new item's `request_detail`, bumps the
        // generation — the fetch that was in flight for the old tab is dropped, not applied.
        let (old, prev) = begin_switch(1);
        supersede_season();
        land_season(old, SRV_A, "show-1".to_string(), 1, prev, Some(vec![episode("s2e1")]));
        assert!(!pump_season(), "a landing from a superseded generation is discarded");
        assert_eq!(listed_eps(), ["s1e1"], "and it must not touch the episode row");

        // MONOTONE mailbox: with a newer result sitting unconsumed, an older fetch finally
        // returning must not overwrite it. Losing the newest season that way also lost its
        // SEASON_DONE catch-up, which wedged the loading spinner on.
        let (old, prev) = begin_switch(1);
        let (new, _) = begin_switch(1);
        land_season(new, SRV_A, "show-1".to_string(), 1, prev, Some(vec![episode("fresh")]));
        land_season(old, SRV_A, "show-1".to_string(), 1, prev, Some(vec![episode("stale")]));
        assert!(pump_season(), "the newest season lands");
        assert_eq!(listed_eps(), ["fresh"], "the late older landing was refused");

        // A LANDING FOR ANOTHER ITEM: the page can move (Related -> a new detail) while a season
        // fetch is in flight, and those episodes belong to nobody on screen. It must still settle
        // the spinner — nothing else is going to.
        let (gen, prev) = begin_switch(1);
        install_show("show-2", 0, &["other-e1"]);
        land_season(gen, SRV_A, "show-1".to_string(), 1, prev, Some(vec![episode("s2e1")]));
        assert!(!pump_season(), "a landing for a different item reports no change");
        assert_eq!(listed_eps(), ["other-e1"], "and leaves the item now on screen alone");
        assert!(!season_loading(), "but it still settles the spinner");

        clear();
    }

    /// The SAME landing, refused because the page moved to the OTHER SERVER's show with the same
    /// ratingKey. Nothing else can see it: the hop bumps no generation that distinguishes them (a
    /// `request_detail` for a different item does, but this is a page mounted from the trail or a
    /// merged shelf, and the rk test — the only ownership test there was — passes.) So the share's
    /// show would have been listing our show's episodes, silently.
    #[test]
    fn a_season_landing_for_another_servers_show_with_the_same_key_is_refused() {
        let _serial = crate::testlock::serial();

        // our server's show 42, one season switch in flight
        install_show_on(SRV_A, "42", 0, &["ours-e1"]);
        let (gen, prev) = begin_switch(1);
        // …and while it is out, the user lands on the SHARE's show 42
        install_show_on(SRV_B, "42", 0, &["theirs-e1"]);
        land_season(gen, SRV_A, "42".to_string(), 1, prev, Some(vec![episode("ours-s2e1")]));

        assert!(!pump_season(), "our episodes are not news about the share's show");
        assert_eq!(listed_eps(), ["theirs-e1"], "the page on screen keeps its own list");
        assert!(!season_loading(), "…and the spinner still settles, as for any foreign landing");

        // the control: the very same landing DOES install when the page is still ours
        install_show_on(SRV_A, "42", 0, &["ours-e1"]);
        let (gen, prev) = begin_switch(1);
        land_season(gen, SRV_A, "42".to_string(), 1, prev, Some(vec![episode("ours-s2e1")]));
        assert!(pump_season());
        assert_eq!(listed_eps(), ["ours-s2e1"]);

        clear();
    }

    /// The OPTIMISTIC half of a view-state write (`crate::viewstate`): the page must show the press
    /// on the frame it happens, because the write that justifies it is now on a worker and the
    /// item's server may be a share that takes seconds to answer — or never answers at all.
    ///
    /// Three things have to move together, and the season count is the one that is easy to forget:
    /// the tab's tick is derived from `viewedLeafCount` ([`Season::watched`]), so a tick left saying
    /// the opposite of the episode row under it is the same "one item, two answers on one screen"
    /// this page refuses everywhere else.
    #[test]
    fn an_optimistic_watch_flip_reaches_the_item_its_episodes_and_the_season_tabs_count() {
        let _serial = crate::testlock::serial();

        // the loaded item itself — the hero's own toggle
        set_current_for_test(Some(Detail { sid: SRV_A, rk: "42".into(), resume_ms: 900_000, ..Default::default() }));
        assert!(set_watched_local(SRV_A, "42", true));
        assert!(current().unwrap().watched);
        assert_eq!(current().unwrap().resume_ms, 0, "a watched item stops offering to resume");

        // …and the SHARE's 42 is a different film, so neither its press nor ours reaches the other
        assert!(!set_watched_local(SRV_B, "42", false), "another server's key names nothing here");
        assert!(current().unwrap().watched, "and leaves this page exactly as it was");

        // an EPISODE of the loaded show — the filmstrip's context menu
        set_current_for_test(Some(Detail {
            sid: SRV_A,
            rk: "show".into(),
            is_show: true,
            cur_season: 1,
            seasons: vec![
                Season { rk: "sk1".into(), index: 1, title: "S1".into(), leaf_count: 3, viewed_leaf_count: 3 },
                Season { rk: "sk2".into(), index: 2, title: "S2".into(), leaf_count: 3, viewed_leaf_count: 1 },
            ],
            episodes: vec![
                Episode { rk: "e1".into(), watched: true, ..Default::default() },
                Episode { rk: "e2".into(), resume_ms: 60_000, ..Default::default() },
            ],
            ..Default::default()
        }));

        assert!(set_watched_local(SRV_A, "e2", true), "an episode of the loaded season");
        let d = current().unwrap();
        assert!(d.episodes[1].watched);
        assert_eq!(d.episodes[1].resume_ms, 0, "…and its still stops drawing a resume bar");
        assert!(!d.watched, "marking one episode does not finish the show");
        assert_eq!(d.seasons[1].viewed_leaf_count, 2, "the BROWSED season's count moves with it");
        assert_eq!(d.seasons[0].viewed_leaf_count, 3, "and no other season's does");

        // idempotent: pressing watched on an already-watched episode must not double-count the
        // season, which would make a part-watched season read as finished
        assert!(set_watched_local(SRV_A, "e2", true));
        assert_eq!(current().unwrap().seasons[1].viewed_leaf_count, 2, "the count follows the FLIP");

        // …and the reverse, clamped at zero rather than going negative
        for _ in 0..5 {
            assert!(set_watched_local(SRV_A, "e2", false));
            assert!(set_watched_local(SRV_A, "e1", false));
        }
        assert_eq!(current().unwrap().seasons[1].viewed_leaf_count, 0, "never a negative remainder");

        assert!(!set_watched_local(SRV_A, "not-here", true), "an rk on neither the item nor its row");
        clear();
    }

    /// The THIRD store this page holds, and the one the two arms above cannot reach: a **Related
    /// tile**, which is a different item entirely.
    ///
    /// Since 2026-08-21 that shelf has a context menu, so the detail page can mark an item that is
    /// neither the loaded one nor a leaf of it. Without this pass the press wrote correctly to the
    /// server and the tile under the user's thumb kept its old tick and its old resume bar until a
    /// refetch — which reads as the row having done nothing, the exact failure the optimistic edit
    /// exists to prevent.
    ///
    /// Three properties, and each is a way the walk can be written wrong:
    /// * it must run BEFORE (and outside) the loaded-item / episode arms, both of which return
    ///   early — chained under either one, a Related hit on a page whose own rk did not match would
    ///   never be reached;
    /// * it must match on the ROW's `sid`, not the page's. Both servers number their ratingKeys
    ///   from 1, so a bare-key walk would flip a tile because a *share's* item happened to share its
    ///   number (`docs/shared-servers.md` §2);
    /// * and the verdict must survive the arms below it, or the function reports "nothing here was
    ///   about that item" having just edited a tile.
    #[test]
    fn an_optimistic_watch_flip_reaches_the_related_shelf_the_menu_was_opened_on() {
        let _serial = crate::testlock::serial();
        let rel = |sid, rk: &str| Related {
            sid,
            rk: rk.into(),
            dur_ns: 7_020_000 * 1_000_000,
            resume_ms: 3_510_000,
            unwatched: true,
            ..Default::default()
        };
        // a SHOW page, so the loaded item and its episodes are both populated and both must be left
        // exactly as they were by a press on a tile that is neither
        set_current_for_test(Some(Detail {
            sid: SRV_A,
            rk: "show".into(),
            is_show: true,
            episodes: vec![Episode { rk: "e1".into(), ..Default::default() }],
            related: vec![rel(SRV_A, "r0"), rel(SRV_A, "r1")],
            ..Default::default()
        }));

        // …and the tile is reached even though the page's own rk did not match and the rk is on no
        // episode — the two arms that both return early
        assert!(set_watched_local(SRV_A, "r1", true), "the Related tile is a hit, not a miss");
        let d = current().unwrap();
        assert!(d.related[1].watched && !d.related[1].unwatched, "the tick the menu just promised");
        assert_eq!(d.related[1].resume_ms, 0, "…and the bar it was wearing, or the tile shows both");
        assert!(d.related[0].resume_frac().is_some(), "no other tile moved");
        assert!(!d.watched, "the page's own item is not what was pressed");
        assert!(!d.episodes[0].watched, "…nor is any episode of it");

        // the way back, from the second row a part-watched tile offers
        assert!(set_watched_local(SRV_A, "r1", false));
        let d = current().unwrap();
        assert!(d.related[1].unwatched && !d.related[1].watched);

        // A SHARE's `r0` is a different film that happens to carry the same number. The row's own
        // `sid` is what keeps the press off it — a bare-key walk would flip the tile here.
        assert!(!set_watched_local(SRV_B, "r0", true), "another server's key names nothing on this shelf");
        assert!(current().unwrap().related[0].resume_frac().is_some(), "…and the tile is untouched");

        clear();
    }

    /// `cached_playing` is the fast path that SKIPS the PMS fetch, so a false hit is the worst of
    /// the five collisions: the whole `PlayingItem` — the `Stream.id`s that get PUT to a server, the
    /// frame size the direct-play gate reasons about, the fps, the chapters, the markers — would be
    /// the loaded page's item rather than the one about to play, with nothing on screen to say so.
    #[test]
    fn the_playing_item_cache_hits_only_for_the_same_item_on_the_same_server() {
        let _serial = crate::testlock::serial();
        let audio = vec![Stream { id: 7, ..Default::default() }];
        set_current_for_test(Some(Detail {
            sid: SRV_A,
            rk: "42".into(),
            audio: audio.clone(),
            width: 3840,
            height: 2160,
            ..Default::default()
        }));

        let hit = cached_playing(SRV_A, "42").expect("the loaded page IS this item");
        assert_eq!((hit.sid, hit.rk.as_str()), (SRV_A, "42"), "the store records where it came from");
        assert_eq!(hit.audio.first().map(|s| s.id), Some(7));

        assert!(cached_playing(SRV_B, "42").is_none(), "the SHARE's 42 is a different film");
        assert!(cached_playing(SRV_A, "43").is_none());
        assert!(cached_playing(crate::plex::ServerId::UNSET, "42").is_none(), "unscoped names neither");

        // …and the pre-existing rule is untouched: a page with no streams is not a usable cache
        // entry, whatever its identity says (it would hand playback an empty track list).
        set_current_for_test(Some(Detail { sid: SRV_A, rk: "42".into(), ..Default::default() }));
        assert!(cached_playing(SRV_A, "42").is_none(), "no streams loaded yet — go and fetch");
        clear();
    }

    /// The season-scope watched rule. Pure (no crate global, so no `testlock` here) and worth its
    /// own test because two very different call sites depend on it — the season tab draws a tick
    /// off it, and "Mark Season Watched" will decide which way to scrobble off it. The counts are
    /// the ones a live `/library/metadata/{show}/children` returned: `idx=1 leaves=10 viewed=10`
    /// and `idx=2 leaves=10 viewed=1`.
    #[test]
    fn a_season_is_watched_only_when_the_server_counted_episodes_and_all_of_them_are_seen() {
        let season = |leaf: i64, viewed: i64| Season {
            rk: String::new(),
            index: 0,
            title: String::new(),
            leaf_count: leaf,
            viewed_leaf_count: viewed,
        };
        assert!(season(10, 10).watched(), "every episode seen");
        assert!(!season(10, 1).watched(), "one episode in is not watched");
        assert!(!season(10, 0).watched(), "never started");
        // A season the server sent no counts for is 0 >= 0 — the `leaf_count > 0` half of the rule
        // is the only thing keeping "we don't know" from reporting as "fully watched".
        assert!(!season(0, 0).watched(), "no counts is not a watched season");
        // viewedLeafCount can lead leafCount right after a scrobble of a season being re-indexed;
        // more-watched-than-exists is still watched, never a negative remainder.
        assert!(season(10, 11).watched(), "an over-count is still watched");
    }
    // ---- credits (Cast & Crew) ----------------------------------------------------------------

    /// The crew fold, parsed from the shape PMS actually sends (verified live 2026-07-29): the
    /// `Director[]`/`Writer[]` rows are `Role[]` rows MINUS the `role` attribute, so the job — the
    /// only thing left to caption a crew tile with — exists nowhere but the array name.
    ///
    /// Deliberately driven through serde rather than a hand-built `Metadata`, because the parse is
    /// half the claim: if the DTO ever stops carrying `Director[]`, the tiles vanish silently.
    #[test]
    fn crew_credits_fold_both_job_arrays_into_one_deduplicated_shelf_list() {
        let body = br#"{
            "Director": [
                { "id": 161, "filter": "director=161", "tag": "Jane Doe",
                  "tagKey": "5d77682a", "count": 3,
                  "thumb": "https://metadata-static.plex.tv/c/people/c.jpg" },
                { "id": 162, "filter": "director=162", "tag": "" }
            ],
            "Writer": [
                { "id": 163, "filter": "writer=163", "tag": "Jane Doe",
                  "tagKey": "5d77682a", "count": 3,
                  "thumb": "https://metadata-static.plex.tv/c/people/c.jpg" },
                { "id": 164, "filter": "writer=164", "tag": "Sam Scribe" }
            ]
        }"#;
        let it: crate::plex::Metadata = serde_json::from_slice(body).expect("the live crew shape parses");
        assert_eq!(it.director.len(), 2, "Director[] is on the DTO");
        assert_eq!(it.writer.len(), 2, "and so is Writer[]");
        assert!(it.director[0].role.is_empty(), "crew rows carry no role — the JOB is the caption");

        let crew = crew_credits(&it);
        let got: Vec<(&str, &str)> = crew.iter().map(|c| (c.tag.as_str(), c.role.as_str())).collect();
        assert_eq!(
            got,
            [("Jane Doe", "Director, Writer"), ("Sam Scribe", "Writer")],
            "directors first, and the writer-director is ONE tile listing both jobs — not two \
             identical headshots side by side"
        );
        assert_eq!(crew[0].thumb, "https://metadata-static.plex.tv/c/people/c.jpg", "the headshot rides along");
        assert!(crew[1].thumb.is_empty(), "a crew member with no headshot is still a credit");
    }

    /// The shelf's flat index space: the screen addresses one row of tiles, so `credit(i)` must run
    /// the actors out first and then the crew, and refuse an index past the end rather than panic —
    /// the focus column outlives the item it was set on (a Related jump reloads underneath it).
    #[test]
    fn the_credit_index_space_runs_every_actor_then_every_crew_member() {
        let person = |t: &str, r: &str| Cast {
            tag: t.to_string(),
            role: r.to_string(),
            thumb: String::new(),
            id: 0,
            tag_key: String::new(),
        };
        let d = Detail {
            cast: vec![person("Actor A", "Hero"), person("Actor B", "Villain")],
            crew: vec![person("Jane Doe", "Director")],
            ..Default::default()
        };
        assert_eq!(d.credits_len(), 3, "the shelf is as long as the two lists together");
        let seen: Vec<(&str, &str)> =
            (0..d.credits_len()).filter_map(|i| d.credit(i)).map(|c| (c.tag.as_str(), c.role.as_str())).collect();
        assert_eq!(seen, [("Actor A", "Hero"), ("Actor B", "Villain"), ("Jane Doe", "Director")]);
        assert!(d.credit(3).is_none(), "one past the end is None, not a panic");
        assert!(d.credit(usize::MAX).is_none(), "and so is a wildly stale focus column");

        let crew_only = Detail { crew: vec![person("Jane Doe", "Director")], ..Default::default() };
        assert_eq!(crew_only.credits_len(), 1, "a crew-only item still fills the shelf");
        assert_eq!(crew_only.credit(0).map(|c| c.tag.as_str()), Some("Jane Doe"), "and its first tile is the crew");
    }
}
