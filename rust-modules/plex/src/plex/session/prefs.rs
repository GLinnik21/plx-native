//! **The preference keys, declared once.**
//!
//! [`Preferences`] is the ten player scalars and the hero trailer autoplay switch (PER PROFILE
//! since the seat work: each is read from the seated profile's own record in `profile_prefs`,
//! falling back to the frozen install-wide key beside it, see [`profile_prefs`]), and since A5
//! every other preference key of the
//! session (`auto_sign_in`, `last_hero_blur`, `ui_resolution`, `hold_hint_taught`, the two Account-scope lists, the
//! two per-profile library lists and the two memory-door lists, `track_memory` and
//! `subtitle_offsets`), that used to be spelled, field for field, in
//! [`Session`] AND in the canonical (DB8) preference record: the struct field, the manual
//! `Default`, the copy in and out of the canonical form, the getter and a `with_<x>` builder, ten
//! sites per setting. The one key that remains outside it is `language` (its two forms differ on
//! purpose). It is flattened into BOTH on-disk forms (`#[serde(flatten)]`, declared before
//! each form's own `extensions` so that a flattened struct takes its keys first and the unknown-key
//! map still sees only keys nobody models), so the legacy file and the canonical public
//! preferences carry exactly the keys they always did, under exactly the attributes they always had.
//!
//! **Every serde attribute below is copied character for character from where it used to sit**,
//! and no two parse policies were unified: each row keeps the soft parse it was written with
//! (`Row::PARSE` names it, and `prefs_tests::t_parse_each_row_keeps_its_declared_policy` drives it). A reader of the wire format
//! never sees this module.
//!
//! One [`Row`] impl per field, keyed by a MARKER type (never by the value type: two settings
//! of one type, or a stored `Option` read as a plain value, would collide). It carries the key,
//! the [`Scope`], whether the row is account-bound, whether the media layer applies it live, how a
//! Settings row saves it, and its parse policy. A row whose value is `Copy` is also a [`Setting`]
//! (the `Stored` and `Value` types and the accessors); a list row (`Vec`) is a [`ListRow`], which
//! only names its items and its field. [`DESCRIPTORS`] lists them all, and the tests hold that table
//! to the struct and to platform's account-bound list, both ways, so a field without a row (or a row
//! without a field) fails instead of drifting.
//!
//! **The track memory row is a LIST that is not a [`ListRow`]**: `track_memory`, each profile's
//! remembered audio and subtitle picks per show ([`TrackMemoryEntry`], in the memory door below).
//! It is a [`Row`] only, so it has no whole-list `items_mut`: its writes go through
//! [`ProfileMemory::upsert_with`], which merges, prunes and budgets, never through a `Vec`
//! assignment. `subtitle_offsets` is the other tenant of the door and is declared the same way
//! ([`SubtitleOffsetsPref`]).
use super::{
    DolbyVisionMode, InstantDpadSeeking, DeckPress, DirectPlayMode, HdrOverlayBrightness, LastLibrary, LibraryViews, NativeAudioRefusal, NextEpisodeMode, OpaqueExtensions,
    PlaintextConsent, HoldHintTaught, PlaybackQuality, ServerKeyPin, Session, SkipInterval, SubtitleOffsetEntry, SubtitleOffsets, SubtitlePosition,
    SubtitleSize, SubtitleTone, UiResolution, UnderPosters,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

mod profile_prefs;
#[cfg(test)]
mod profile_prefs_tests;
mod track_memory;
#[cfg(test)]
mod track_memory_document_tests;
#[cfg(test)]
mod track_memory_tests;
pub use profile_prefs::{
    effective, own_prefs, persist_profile_pref, put_own, stored_effective, ProfilePrefs, ProfilePrefsPref,
    ProfilePrefsRecord, ProfileSetting, ProfileWrite,
};
pub use track_memory::{
    AudioMemo, SubtitleMemo, SubtitleTrackMemo, TrackMemoryEntry, TrackMemoryPref, CAP as TRACK_MEMORY_CAP,
};

/// Every preference key but `language`, flattened into [`Session`] and into
/// the canonical preference record. Defaults are each type's own `Default`, which A0 pins as equal
/// to the soft parse's fallback. The exception is `trailer_autoplay`: this struct's (and
/// [`Session`]'s) derived default is off, while a parsed record whose key is absent reads on; the
/// canonical record's own `Default` therefore sets it explicitly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Preferences {
    /// The playback-quality preference every profile reads until it picks its own (per profile in
    /// `profile_prefs`; this top-level key is the FROZEN install-wide fallback, which a build with
    /// per-profile settings never writes, except that session creation sets it to the install
    /// default, [`PlaybackQuality::fresh_default`]). `None` is deliberately distinct from an
    /// explicit value: every session written before this field existed lands there and must keep
    /// the old **Original** behaviour rather than being migrated onto automatic playback.
    ///
    /// A newly-created session writes an explicit default through [`PlaybackQuality::fresh_default`].
    /// That default may become Auto only when the playback owner exposes a positive readiness
    /// gate. The integrated HLS prime/swap path opens it for fresh installs; old files remain
    /// Original because their absent field is not reinterpreted. Unknown or malformed future
    /// values soften to `None`, and therefore Original,
    /// instead of making the credentials file fail to parse.
    #[serde(default, deserialize_with = "de_soft_playback_quality")]
    pub playback_quality: Option<PlaybackQuality>,
    /// Original-stream override every profile reads until it picks its own (the frozen install-wide
    /// fallback of `profile_prefs`); malformed/future values remain automatic.
    #[serde(default, deserialize_with = "de_soft_direct_play_mode")]
    pub direct_play_mode: DirectPlayMode,
    /// **How bright the client-rendered subtitles are drawn** — white, or a rung of the grey
    /// ladder under it. A frozen install-wide fallback like [`Preferences::playback_quality`]; each
    /// profile keeps its own pick in `profile_prefs`. (It was install-wide because an HDR picture maps
    /// graphics white to a searing level, a fact about the PANEL; the owner ruled it a habit of the
    /// viewer instead.) Absence is white, which is what every build before the field drew.
    /// Soft-parsed: an unknown spelling costs the preference, never the credentials.
    #[serde(default, deserialize_with = "de_soft_subtitle_tone")]
    pub subtitle_tone: SubtitleTone,
    /// **The client-rendered subtitle caption's text size.** A frozen fallback like
    /// [`Preferences::subtitle_tone`]. Absence is Medium — see [`SubtitleSize`]'s own doc. Skipped at
    /// the default so a session predating this field — committed replay fixtures included —
    /// serializes exactly as it did before.
    #[serde(default, deserialize_with = "de_soft_subtitle_size", skip_serializing_if = "is_default_subtitle_size")]
    pub subtitle_size: SubtitleSize,
    /// **The client-rendered subtitle caption's vertical placement.** A frozen fallback like
    /// [`Preferences::subtitle_tone`]. Absence is Low — see [`SubtitlePosition`]'s own doc. Skipped at
    /// the default for the same reason as [`Preferences::subtitle_size`].
    #[serde(default, deserialize_with = "de_soft_subtitle_position", skip_serializing_if = "is_default_subtitle_position")]
    pub subtitle_position: SubtitlePosition,
    /// **How bright the player's overlay and captions are while the picture is HDR** — see
    /// [`HdrOverlayBrightness`]. A frozen fallback like [`Preferences::subtitle_tone`]. Absence is
    /// ThreeQuarters (the default dim). Skipped at the default for the same reason as [`Preferences::subtitle_size`].
    #[serde(default, deserialize_with = "de_soft_hdr_overlay_brightness", skip_serializing_if = "is_default_hdr_overlay_brightness")]
    pub hdr_overlay_brightness: HdrOverlayBrightness,
    /// **What the player does when an episode with a successor reaches its credits** — see
    /// [`NextEpisodeMode`]. A frozen fallback like [`Preferences::playback_quality`]. Absence is
    /// Countdown, which is what every build before the field did. Skipped at the default so a
    /// session predating this field — committed replay fixtures included — serializes exactly as
    /// it did before.
    #[serde(default, deserialize_with = "de_soft_next_episode_mode", skip_serializing_if = "is_default_next_episode_mode")]
    pub next_episode_mode: NextEpisodeMode,
    /// **How far one Left/Right press jumps in the player** — see [`SkipInterval`]. A frozen
    /// fallback like [`Preferences::playback_quality`], one value for both directions. Absence is 10 s, the
    /// hop every build before the field made. Skipped at the default so a session predating this
    /// field — committed replay fixtures included — serializes exactly as it did before.
    #[serde(default, deserialize_with = "de_soft_skip_interval", skip_serializing_if = "is_default_skip_interval")]
    pub skip_interval: SkipInterval,
    /// Hidden-HUD D-pad jumps; off by default.
    #[serde(default, deserialize_with = "de_soft_instant_dpad_seeking", skip_serializing_if = "InstantDpadSeeking::is_default")]
    pub instant_dpad_seeking: InstantDpadSeeking,
    /// Dolby Vision override (`Auto` or `Off`); `Auto` by default.
    #[serde(default, deserialize_with = "de_soft_dolby_vision_mode", skip_serializing_if = "DolbyVisionMode::is_default")]
    pub dolby_vision_mode: DolbyVisionMode,
    /// **What OK does on a Continue Watching card** — see [`DeckPress`]. A frozen fallback like
    /// [`Preferences::playback_quality`]: each profile keeps its own pick in `profile_prefs`, and a
    /// profile that has not picked reads this top-level key. Absence is Details, so a session written
    /// before the field existed opens the page on OK. Skipped at the default so a session predating this field —
    /// committed replay fixtures included — serializes exactly as it did before.
    #[serde(default, deserialize_with = "de_soft_deck_press", skip_serializing_if = "is_default_deck_press")]
    pub deck_press: DeckPress,
    /// **What sits under the focused poster** — see [`UnderPosters`]. A frozen fallback like
    /// [`Preferences::deck_press`]: each profile keeps its own pick in `profile_prefs`. Absence is no
    /// title and the year and length. Skipped at the default so a session predating this field —
    /// committed replay fixtures included — serializes exactly as it did before.
    #[serde(default, deserialize_with = "de_soft_under_posters", skip_serializing_if = "is_default_under_posters")]
    pub under_posters: UnderPosters,
    /// Plex Pass per-track transcoder DSP preference (issue #266): dialog boost / loudness
    /// normalization. `NONE` by default and skipped on write while `NONE`, so every session
    /// persisted before the field existed — and every viewer who never opted in — round-trips
    /// byte-identical to what it wrote before. A frozen fallback like [`Preferences::playback_quality`]:
    /// the seated profile's value is restored into `player::audio_enhancements` when the seat changes;
    /// written (per profile) by `player::set_audio_enhancements` through the media door (`route::choose`, which saves via
    /// [`persist_profile_pref`]). Soft-parsed
    /// like every preference in this struct: an unknown shape costs the preference, never the
    /// credentials.
    #[serde(default, deserialize_with = "de_soft_audio_enhancements", skip_serializing_if = "crate::plex::AudioEnhancements::is_none")]
    pub audio_enhancements: crate::plex::AudioEnhancements,

    // ---- A5: the rest of the preference keys, in the order the legacy file always wrote them ----
    //
    // Every attribute below is copied from where the field sat in `Session`. The one key whose two
    // hosts differed is `plaintext_consent` (the legacy file writes `[]`, the canonical record omits
    // it while empty); it keeps the legacy attributes here and `split_public` drops the empty key.
    // Two defaults are not the derived one: `trailer_autoplay` reads ON when its key is absent
    // (`default_true`) although `Preferences::default()`, like `Session::default()`, is off.

    /// **Automatically Sign In** — skip the boot who's-watching picker and enter as
    /// [`Session::user`]. Install-wide, not per profile: the boot gate reads it before anyone is
    /// seated this run. Absence is **off**, which is today's picker. Enabling it from Settings
    /// while a profile is already active is an explicit opt-in to skip that profile's PIN on the
    /// next launch; BACK out of the picker still refuses a protected resume when this is off.
    ///
    /// Soft-parsed so a hand-edited or future-shaped value costs the preference, never the
    /// credentials.
    #[serde(default, deserialize_with = "super::de_soft_bool")]
    pub auto_sign_in: bool,
    /// **Interface resolution** — the window the NEXT launch asks for ([`UiResolution`]).
    /// Install-wide, not per profile: `app::boot::construct` reads it before `SDL_CreateWindow`,
    /// before anyone is seated. Absence (every session written before the field) is Automatic, the
    /// native 1920x1080 canvas; skipped at the default so committed replay fixtures serialize
    /// unchanged. Soft-parsed: a spelling this build does not know costs the preference only.
    #[serde(default, deserialize_with = "de_soft_ui_resolution", skip_serializing_if = "is_default_ui_resolution")]
    pub ui_resolution: UiResolution,
    /// **Which "Hold [OK] for options" hints this install has been taught** ([`HoldHintTaught`],
    /// `plx_ui::hold_hint`): the self-appearing hint stands once per install per surface, so the
    /// flags survive relaunches. Install-wide, kept across sign-out, shared by every profile (it is
    /// about the viewer's thumb, not an account). No Settings row: the application seeds the hint's
    /// latches from it at boot and writes it on the first read showing. Absence is "not taught";
    /// skipped at the default so a session predating this field — committed replay fixtures
    /// included — serializes exactly as it did before. Soft-parsed: garbage costs the lesson,
    /// never the credentials.
    #[serde(default, deserialize_with = "de_soft_hold_hint_taught", skip_serializing_if = "is_default_hold_hint_taught")]
    pub hold_hint_taught: HoldHintTaught,
    /// **Hero trailer autoplay** — the install-wide FALLBACK every profile reads until it picks its
    /// own (per profile in `profile_prefs`; this top-level key is frozen, which a build with
    /// per-profile settings never writes, except that session creation sets it on, as it always
    /// has). Detail default is on. Absence is on, so a session written
    /// before this field existed does not silently lose the preview. Explicit `false` stays off.
    /// Soft-parsed to on rather than failing the credentials file. The bound Starfish surface
    /// has no mute, so this is also the only sound control.
    #[serde(default = "super::default_true", deserialize_with = "super::de_soft_bool_on")]
    pub trailer_autoplay: bool,
    /// **Retired: nothing reads or writes it any more.** It was the last hero `UltraBlurColors`
    /// envelope Home rendered, which seeded the first-run routes' frozen ground; those routes now
    /// paint the authored default ambient (`theme::ROUTE_GROUND_FALLBACK`), because a hero-keyed
    /// ground read as a rainbow on servers whose art carries saturated corners. The field stays
    /// so a stored file that carries it still parses and round-trips unchanged.
    ///
    /// Not keyed by profile: it says nothing about content history, only about what colour light
    /// this SET last showed, which is why it lives beside `client_id` rather than in a per-profile
    /// section like [`Session::home_pins`].
    #[serde(default, deserialize_with = "super::de_soft_hero_blur")]
    pub last_hero_blur: Option<[[f32; 3]; 4]>,
    /// **The person's answer to "Connect without encryption?"**, one entry per server
    /// (`machineIdentifier`) they were asked about. The account half of the key is this file: it
    /// is the signed-in account's, and it is cleared with the credentials on sign-out, so an
    /// answer never outlives the account that gave it.
    ///
    /// This is a CHOICE, never a transport grant: nothing here says which address was used or
    /// lets a plaintext origin be reactivated from disk. A credential goes to a plaintext origin
    /// only under a live `plex::grant::PlaintextGrant`, minted in-process from a FRESH eligible
    /// probe and this answer together (`docs/shared-servers.md`). An absent entry is "never
    /// asked".
    ///
    /// Soft-parsed for the reason every list in this struct is: a hand-edited entry costs that
    /// entry — and an entry that cannot be read is "never asked", the closed direction.
    #[serde(default, deserialize_with = "super::de_soft_vec")]
    pub plaintext_consent: Vec<PlaintextConsent>,
    /// **Each server's remembered public key** (issue #380, for the offline fallback of #378), one
    /// entry per `machineIdentifier`: the pin of the leaf certificate the server presented the last
    /// time a connection to it passed STRICT verification and its `/identity` named that machine.
    /// A television cold-booted with no internet has a wrong clock, so the server's own valid
    /// certificate fails its date check; the key it had is what lets the next layer tell "the
    /// server I know" from a stranger. [`Session::server_key_pin`] is the accessor and
    /// [`project_server_keys`](super::project_server_keys) its reader: `net::keypin` uses the key
    /// to recognise the server when only the certificate's dates fail (issue #378).
    ///
    /// Session-level, not part of [`ServerRef`](super::ServerRef), [`SourceRef`](super::SourceRef) or
    /// [`ProfileCreds`](super::ProfileCreds): those are cloned per profile, while a key is a fact
    /// about one machine seen from this television and this account. It sits in the PUBLIC
    /// preferences, so a pin write is a public-only edit with no credential reseal; the price is
    /// that sign-out does not take it with the credentials by itself. The native
    /// `Mutation::ClearTenure` retains public preferences whole EXCEPT the keys in
    /// `storage::state::ACCOUNT_BOUND_PREFERENCES`, and this field's key is one: sign-out, and so
    /// "Delete all local data" which follows it, leave the stored record with no learned key. The
    /// legacy file store drops it with the file. Skipped on write while empty, so a session that
    /// never learned one round-trips byte-identical to what it wrote before. Soft-parsed: an
    /// unreadable entry costs that entry.
    #[serde(default, deserialize_with = "super::de_soft_vec", skip_serializing_if = "Vec::is_empty")]
    pub server_key_pins: Vec<ServerKeyPin>,
    /// **Native audio the pipeline of this television was watched refusing** — firmware family,
    /// codec and channel count ([`NativeAudioRefusal`]). The player learns one when a native Load
    /// ends in `type=18 num=200 Audio Codec Not Supported` (applied at once), or counts one proof each
    /// time a refused-format open was rescued by a facet-clean conversion (applied once it holds
    /// `LEARNED_REFUSAL_PROOFS`); the next play's start value (`devcaps::start_caps`,
    /// from the rows in force, [`NativeAudioRefusal::applies`]) no longer announces the combination, so
    /// that play is converted. The boot snapshot (`devcaps::caps`) is never rewritten.
    /// An INSTALL fact: a property of this television's firmware, kept across sign-out and
    /// across profiles. Skipped while empty, soft-parsed per entry.
    #[serde(default, deserialize_with = "super::de_soft_vec", skip_serializing_if = "Vec::is_empty")]
    pub native_audio_refused: Vec<NativeAudioRefusal>,
    /// **The library each top tab was last browsed in**, per profile — so a household with two TV
    /// libraries opens the one it actually watches instead of whichever the server happens to list
    /// first.
    ///
    /// Without it the tab resolves through `browse::section_of_kind` every launch: owned first,
    /// then table order, which is the server's order and is not a preference anybody expressed.
    /// Issue #68 is what that costs when the two are not the same library — the reporter's own
    /// libraries opened in the order their server listed them, every time.
    ///
    /// **Per TYPE, not one entry per profile.** The Movies tab and the TV Shows tab are two
    /// choices; one slot would make picking a film library forget which shows you browse.
    ///
    /// Keyed by profile like [`super::RecentSearches`] and [`super::HomePins`], and by (machine id, section key)
    /// like [`super::PinnedLib`] — never a section INDEX, which the table renumbers on every
    /// `browse::reset` and on a re-discovery that appends.
    ///
    /// Cleared with the credentials at sign-out (`storage::state::ACCOUNT_BOUND_PREFERENCES`), so a
    /// profile's choice does not outlive the account that made it. Its records are not stamped, so
    /// the one login that is not a sign-out (a stored session that could not be activated, boot.rs)
    /// leaves them to the next account.
    ///
    /// Soft-parsed for the reason every list in this struct is: one hand-edited entry costs that
    /// entry, never the credentials.
    #[serde(default, deserialize_with = "super::de_soft_vec")]
    pub last_library: Vec<LastLibrary>,
    /// **How each library was last browsed**, per profile — the sort and its direction, the
    /// Unwatched switch, the genre and the listing type (Movies / Collections, Shows / Seasons /
    /// Episodes / Collections) — so a library the viewer sorted by Plays, narrowed to Unwatched
    /// Action films and left there opens that way again after a restart instead of falling back
    /// to the server's title order with every filter off (GitHub #278 for the sort, #441 for the
    /// rest).
    ///
    /// **Stored on disk as `library_sorts`**, the name it had when it held only the sort: the key
    /// is what an older build reads back on a rollback, so it stays ([`super::Session::home_pins`] set
    /// the precedent) and each entry grows fields beside the old ones instead of replacing them.
    ///
    /// Keyed like [`Preferences::last_library`] and for its reasons: by profile, because a view is a
    /// person's habit rather than the television's, and by (machine id, section key), never a
    /// section INDEX. The sort is recorded by its KEY, never by its menu position: the menu is
    /// the server's (`Meta.Type[].Sort`) and a PMS update may reorder it. `browse` applies a
    /// recorded sort or genre only once the server has offered it again, so one a server stopped
    /// advertising falls silently back to the default rather than being sent blind.
    ///
    /// Bounded per profile ([`super::LibraryViews::CAP`], most recent kept), and choosing a library's
    /// DEFAULT view back removes its entry rather than recording it, so the list holds only the
    /// libraries somebody actually changed.
    ///
    /// Cleared with the credentials at sign-out (`storage::state::ACCOUNT_BOUND_PREFERENCES`) and,
    /// like [`Preferences::last_library`], not stamped.
    ///
    /// Soft-parsed for the reason every list in this struct is; omitted while empty so a session
    /// that never changed a view serializes exactly as it did before the field existed.
    #[serde(rename = "library_sorts", default, deserialize_with = "super::de_soft_vec",
        skip_serializing_if = "Vec::is_empty")]
    pub library_views: Vec<LibraryViews>,
    // ---- per-profile memory lists (written only through the memory door, `ProfileMemory`) ----
    /// **Each profile's remembered audio and subtitle picks, per show** — see [`TrackMemoryEntry`].
    /// A [`ProfileList`] per profile, soft-parsed per element so one junk record or entry costs
    /// only itself, and skipped while empty so a session that never remembered anything serializes
    /// exactly as it did before the field existed. Account-bound: the record carries the account
    /// stamp, a stamp that is not the signed-in account's reads as nothing, and sign-out drops the
    /// whole key (`storage::state::ACCOUNT_BOUND_PREFERENCES`). Written only through
    /// [`ProfileMemory::upsert_with`], which prunes foreign stamps and holds the household budget.
    #[serde(default, deserialize_with = "super::de_soft_vec", skip_serializing_if = "Vec::is_empty")]
    pub track_memory: Vec<ProfileList<TrackMemoryEntry>>,
    /// **The sync-timing correction a profile last tuned for an item**, per profile, keyed by
    /// (machine id, ratingKey) — never a track id: within one playback a track CHANGE still zeros
    /// the live offset (`route::commit_subtitle_selection`'s own doc), so by the time this is
    /// restored at the next resume only the item's own correction is left to remember. This is
    /// deliberately narrower than a raw in-memory offset would suggest: `crate::player::subtitle_offset_ms`
    /// is about a single live playback (never persisted whole, on purpose — see
    /// `session_compat_tests::the_session_file_carries_no_subtitle_offset`), while this is a
    /// per-item memory a profile builds up across resumes of the SAME file, the same way
    /// [`Preferences::library_views`] remembers a per-library habit.
    ///
    /// Bounded per profile ([`super::SubtitleOffsets::CAP`], most recent kept), and setting the offset
    /// back to Original (0) forgets the entry rather than recording a no-op correction.
    ///
    /// **The player does not read or write this field directly**: it is the first tenant of the
    /// memory door (`media::route::{remember, recall}`), which reads a mirror seeded from this list
    /// at the first restore and reseeded only when the revocation tenure or the account stamp moves
    /// (`MemoryMirror::reseed`), writes through at once, and queues the file write as a delta merged
    /// against the list ([`ProfileMemory::upsert_with`]) under `queue_update`'s tenure and
    /// credential guard. So this list is what a restart or a handoff re-seeds from, and it can lag
    /// the mirror by the writes still queued.
    ///
    /// Account-bound, like `track_memory`: every record carries the account stamp, a stamp that is
    /// not the signed-in account's reads as nothing, and sign-out drops the whole key
    /// (`storage::state::ACCOUNT_BOUND_PREFERENCES`). A record written before the stamp existed is
    /// adopted (read as the signed-in account's, stamped at the list's next write).
    ///
    /// Soft-parsed for the reason every list in this struct is; omitted while empty so a session
    /// that never tuned a subtitle's timing serializes exactly as it did before the field existed.
    #[serde(default, deserialize_with = "super::de_soft_vec", skip_serializing_if = "Vec::is_empty")]
    pub subtitle_offsets: Vec<SubtitleOffsets>,
    // ---- per-profile player settings ----
    /// **Each profile's own playback and display settings** — see [`ProfilePrefsRecord`] and the
    /// module doc of `profile_prefs`. A record per (profile, account stamp), soft-parsed per element so one junk
    /// record costs only itself, skipped while empty so a session that never picked anything per
    /// profile serializes exactly as it did before the field existed. Account-bound: a record whose
    /// stamp is not the signed-in account's reads as nothing, and sign-out drops the whole key
    /// (`storage::state::ACCOUNT_BOUND_PREFERENCES`). Written only through [`persist_profile_pref`].
    #[serde(default, deserialize_with = "super::de_soft_vec", skip_serializing_if = "Vec::is_empty")]
    pub profile_prefs: Vec<ProfilePrefsRecord>,
}

/// How long a setting's value lives, and who owns the write. `auto_sign_in`, `ui_resolution`,
/// `hold_hint_taught` and `last_hero_blur` are `Install`; the two lists sign-out clears are `Account`; the ten player
/// scalars, the hero trailer autoplay switch (each with its install-wide key as the [`Fallback`]) and
/// the library lists are `Profile`;
/// `track_memory` is `PerShow` and `subtitle_offsets` is `PerItem`. Sign-out clears every row that
/// is account-bound, whatever its scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The television: shared by every profile, kept across sign-out, never account-bound.
    Install,
    /// The signed-in account: cleared with the credentials on sign-out.
    Account,
    /// One Plex Home profile (the record is a list keyed by profile and account stamp).
    Profile,
    /// One show, for one profile.
    PerShow,
    /// One item, for one profile.
    PerItem,
}

/// What a [`Scope::Profile`] setting reads when the profile has no value of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// The row has no fallback: it is not a per-profile value with an older install-wide home.
    None,
    /// The top-level key of the same name, frozen at the value the TV had when per-profile settings
    /// arrived (`Setting::get`), then the type's default. It is READ, never copied, so a profile
    /// created later starts where the TV was.
    LegacyInstallKey,
}

/// Which soft-parse policy a row's `deserialize_with` implements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parse {
    /// Garbage reads as the type's `Default` (the scalar enums).
    Soft,
    /// A list parses element by element: the good elements are kept (`de_soft_vec`).
    SoftVec,
    /// A switch whose product default is on: garbage reads as `true` (`de_soft_bool_on`).
    BoolOn,
    /// A switch whose product default is off: garbage reads as `false` (`de_soft_bool`).
    BoolOff,
    /// Garbage reads as `None` (quality, the hero blur).
    OptionalSoft,
}

/// How a Settings row saves the value. `Confirm` claims nothing until the write is durable;
/// `Optimistic` publishes first and persists in order behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsWrite {
    Confirm,
    Optimistic,
}

/// What every preference key declares about itself, keyed by a marker type: [`Descriptor`] is these
/// seven facts as data. A `Copy` value is also a [`Setting`]; a list is a [`ListRow`].
pub trait Row: 'static {
    /// The on-disk key (a serde rename is honoured here).
    const KEY: &'static str;
    const SCOPE: Scope;
    /// Cleared with the credentials at sign-out (`storage::state::ACCOUNT_BOUND_PREFERENCES`).
    const ACCOUNT_BOUND: bool;
    /// The media layer applies the value live: its cell holds the SEATED profile's value, restored by
    /// `route::restore_profile_rows` when the seat changes and once at process start with nobody
    /// seated.
    const LIVE: bool;
    /// How a Settings row saves it through the media door's `choose`; `None` when no such row
    /// exists (the player panel owns it, or the Settings screen writes it through its own ticketed
    /// update).
    const SETTINGS: Option<SettingsWrite>;
    const PARSE: Parse;
    /// What a [`Scope::Profile`] row reads when the seated profile has no value of its own.
    const FALLBACK: Fallback = Fallback::None;
}

/// A row whose value is `Copy`: the accessors the write door and the live cells speak.
pub trait Setting: Row {
    /// The field's type in [`Preferences`]. `PartialEq`, because the write door skips a write whose
    /// STORED value already equals the pick ([`persist_pref`]).
    type Stored: PartialEq;
    /// What callers speak. Equal to `Stored` except where the stored form carries "never chosen"
    /// (quality: `Option<PlaybackQuality>` stored, `PlaybackQuality` spoken).
    type Value: Copy + PartialEq + Send + 'static;
    fn stored(p: &Preferences) -> &Self::Stored;
    fn stored_mut(p: &mut Preferences) -> &mut Self::Stored;
    /// quality: `unwrap_or(Original)`.
    fn get(p: &Preferences) -> Self::Value;
    /// quality: `Some(v)`.
    fn to_stored(v: Self::Value) -> Self::Stored;
    fn put(p: &mut Preferences, v: Self::Value) {
        *Self::stored_mut(p) = Self::to_stored(v);
    }
}

/// A marker and its trivial impl, for a row whose `Stored` and `Value` are one type. It carries no
/// schema: the serde attributes stay on the [`Preferences`] field, where rustfmt and a reviewer
/// can see them.
macro_rules! setting {
    ($marker:ident: $ty:ty = $field:ident, $key:literal, $scope:ident $(fallback $fallback:ident)?,
     account $bound:literal, live $live:literal, settings $settings:expr, parse $parse:ident) => {
        pub struct $marker;
        impl Row for $marker {
            const KEY: &'static str = $key;
            const SCOPE: Scope = Scope::$scope;
            const ACCOUNT_BOUND: bool = $bound;
            const LIVE: bool = $live;
            const SETTINGS: Option<SettingsWrite> = $settings;
            const PARSE: Parse = Parse::$parse;
            $(const FALLBACK: Fallback = Fallback::$fallback;)?
        }
        impl Setting for $marker {
            type Stored = $ty;
            type Value = $ty;
            fn stored(p: &Preferences) -> &$ty { &p.$field }
            fn stored_mut(p: &mut Preferences) -> &mut $ty { &mut p.$field }
            fn get(p: &Preferences) -> $ty { p.$field }
            fn to_stored(v: $ty) -> $ty { v }
        }
    };
}

/// Playback quality. `Stored = Option<PlaybackQuality>` (absent means "never chosen", read as
/// Original) and `Value = PlaybackQuality`: hand-written because it is the one row whose two types
/// differ.
pub struct QualityPref;
impl Row for QualityPref {
    const KEY: &'static str = "playback_quality";
    const SCOPE: Scope = Scope::Profile;
    const ACCOUNT_BOUND: bool = false;
    const LIVE: bool = true;
    const SETTINGS: Option<SettingsWrite> = Some(SettingsWrite::Confirm);
    const PARSE: Parse = Parse::OptionalSoft;
    const FALLBACK: Fallback = Fallback::LegacyInstallKey;
}
impl Setting for QualityPref {
    type Stored = Option<PlaybackQuality>;
    type Value = PlaybackQuality;
    fn stored(p: &Preferences) -> &Option<PlaybackQuality> { &p.playback_quality }
    fn stored_mut(p: &mut Preferences) -> &mut Option<PlaybackQuality> { &mut p.playback_quality }
    fn get(p: &Preferences) -> PlaybackQuality { p.playback_quality.unwrap_or(PlaybackQuality::Original) }
    fn to_stored(v: PlaybackQuality) -> Option<PlaybackQuality> { Some(v) }
}

setting!(DirectPlayModePref: DirectPlayMode = direct_play_mode, "direct_play_mode", Profile fallback LegacyInstallKey,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(NextEpisodeModePref: NextEpisodeMode = next_episode_mode, "next_episode_mode", Profile fallback LegacyInstallKey,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(InstantDpadSeekingPref: InstantDpadSeeking = instant_dpad_seeking, "instant_dpad_seeking", Profile fallback LegacyInstallKey,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(DolbyVisionModePref: DolbyVisionMode = dolby_vision_mode, "dolby_vision_mode", Profile fallback LegacyInstallKey,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(SkipIntervalPref: SkipInterval = skip_interval, "skip_interval", Profile fallback LegacyInstallKey,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(DeckPressPref: DeckPress = deck_press, "deck_press", Profile fallback LegacyInstallKey,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(UnderPostersPref: UnderPosters = under_posters, "under_posters", Profile fallback LegacyInstallKey,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(SubtitleSizePref: SubtitleSize = subtitle_size, "subtitle_size", Profile fallback LegacyInstallKey,
    account false, live true, settings Some(SettingsWrite::Optimistic), parse Soft);
setting!(SubtitlePositionPref: SubtitlePosition = subtitle_position, "subtitle_position", Profile fallback LegacyInstallKey,
    account false, live true, settings Some(SettingsWrite::Optimistic), parse Soft);
setting!(HdrOverlayBrightnessPref: HdrOverlayBrightness = hdr_overlay_brightness, "hdr_overlay_brightness",
    Profile fallback LegacyInstallKey, account false, live true, settings Some(SettingsWrite::Optimistic), parse Soft);
// Tone and enhancements have no Settings row: the player panel is their only writer.
setting!(SubtitleTonePref: SubtitleTone = subtitle_tone, "subtitle_tone", Profile fallback LegacyInstallKey,
    account false, live true, settings None, parse Soft);
setting!(AudioEnhancementsPref: crate::plex::AudioEnhancements = audio_enhancements,
    "audio_enhancements", Profile fallback LegacyInstallKey, account false, live true, settings None, parse Soft);

// The hero trailer autoplay switch (P1c) is the tenth per-profile scalar: the Settings root page's System
// section saves it through the `choose` door (Confirm), and `player::preview::enabled` reads its cell every frame.
setting!(TrailerAutoplayPref: bool = trailer_autoplay, "trailer_autoplay", Profile fallback LegacyInstallKey,
    account false, live true, settings Some(SettingsWrite::Confirm), parse BoolOn);

// ---- A5: the remaining preference keys (a separate block, below the ten) -----------------------
//
// None of these has a `choose` door: the Settings screen writes `auto_sign_in` through its own
// ticketed `queue_update_ticket` (pending UI), `hold_hint_taught` through `session::learn_hold_hint_taught`
// (the application's frame tick), `last_hero_blur` and the lists through their own
// writers, and no media cell applies any of them live.

setting!(AutoSignInPref: bool = auto_sign_in, "auto_sign_in", Install,
    account false, live false, settings None, parse BoolOff);
setting!(UiResolutionPref: UiResolution = ui_resolution, "ui_resolution", Install,
    account false, live false, settings None, parse Soft);
setting!(HoldHintTaughtPref: HoldHintTaught = hold_hint_taught, "hold_hint_taught", Install,
    account false, live false, settings None, parse Soft);
setting!(LastHeroBlurPref: Option<[[f32; 3]; 4]> = last_hero_blur, "last_hero_blur", Install,
    account false, live false, settings None, parse OptionalSoft);

/// A row whose value is a list, so it cannot be a [`Setting`] (a `Setting` value is `Copy`): it
/// names the item type and the field, so a test can drive the list and prove the marker names its own
/// field.
pub trait ListRow: Row {
    type Item;
    fn items(p: &Preferences) -> &Vec<Self::Item>;
    fn items_mut(p: &mut Preferences) -> &mut Vec<Self::Item>;
}

macro_rules! list_row {
    ($marker:ident: $item:ty = $field:ident, $key:literal, $scope:ident, account $bound:literal) => {
        pub struct $marker;
        impl Row for $marker {
            const KEY: &'static str = $key;
            const SCOPE: Scope = Scope::$scope;
            const ACCOUNT_BOUND: bool = $bound;
            const LIVE: bool = false;
            const SETTINGS: Option<SettingsWrite> = None;
            const PARSE: Parse = Parse::SoftVec;
        }
        impl ListRow for $marker {
            type Item = $item;
            fn items(p: &Preferences) -> &Vec<$item> { &p.$field }
            fn items_mut(p: &mut Preferences) -> &mut Vec<$item> { &mut p.$field }
        }
    };
}

// The two lists of the account itself (`ACCOUNT_BOUND_PREFERENCES`).
list_row!(PlaintextConsentRow: PlaintextConsent = plaintext_consent, "plaintext_consent", Account, account true);
list_row!(ServerKeyPinsRow: ServerKeyPin = server_key_pins, "server_key_pins", Account, account true);
// What this television's firmware refused: an install fact, so it survives sign-out.
list_row!(NativeAudioRefusedRow: NativeAudioRefusal = native_audio_refused, "native_audio_refused", Install, account false);
// The per-profile library lists. Sign-out clears them (`ACCOUNT_BOUND_PREFERENCES`), but their
// records are not stamped: the one login that is not a sign-out (boot.rs, a stored session that
// could not be activated) leaves them to the next account, as it always has.
list_row!(LastLibraryRow: LastLibrary = last_library, "last_library", Profile, account true);
list_row!(LibraryViewsRow: LibraryViews = library_views, "library_sorts", Profile, account true);

/// A setting as data, so a test (and a later generic restore) can walk them all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub key: &'static str,
    pub scope: Scope,
    pub account_bound: bool,
    pub live: bool,
    pub settings: Option<SettingsWrite>,
    pub parse: Parse,
    pub fallback: Fallback,
}

impl Descriptor {
    pub const fn of<S: Row>() -> Self {
        Descriptor {
            key: S::KEY,
            scope: S::SCOPE,
            account_bound: S::ACCOUNT_BOUND,
            live: S::LIVE,
            settings: S::SETTINGS,
            parse: S::PARSE,
            fallback: S::FALLBACK,
        }
    }
}

/// Every [`Preferences`] field, once. `prefs_tests::t_fields_every_field_is_described_by_exactly_one_setting` holds this to the struct's own serde
/// field list both ways.
pub const DESCRIPTORS: &[Descriptor] = &[
    Descriptor::of::<QualityPref>(),
    Descriptor::of::<DirectPlayModePref>(),
    Descriptor::of::<SubtitleTonePref>(),
    Descriptor::of::<SubtitleSizePref>(),
    Descriptor::of::<SubtitlePositionPref>(),
    Descriptor::of::<HdrOverlayBrightnessPref>(),
    Descriptor::of::<NextEpisodeModePref>(),
    Descriptor::of::<SkipIntervalPref>(),
    Descriptor::of::<InstantDpadSeekingPref>(),
    Descriptor::of::<DolbyVisionModePref>(),
    Descriptor::of::<DeckPressPref>(),
    Descriptor::of::<UnderPostersPref>(),
    Descriptor::of::<AudioEnhancementsPref>(),
    // A5: the remaining keys.
    Descriptor::of::<AutoSignInPref>(),
    Descriptor::of::<TrailerAutoplayPref>(),
    Descriptor::of::<LastHeroBlurPref>(),
    Descriptor::of::<UiResolutionPref>(),
    Descriptor::of::<HoldHintTaughtPref>(),
    Descriptor::of::<PlaintextConsentRow>(),
    Descriptor::of::<ServerKeyPinsRow>(),
    Descriptor::of::<NativeAudioRefusedRow>(),
    Descriptor::of::<LastLibraryRow>(),
    Descriptor::of::<LibraryViewsRow>(),
    // The per-profile player settings' own values.
    Descriptor::of::<ProfilePrefsPref>(),
    // The memory-door tenants (lists with no value API).
    Descriptor::of::<TrackMemoryPref>(),
    Descriptor::of::<SubtitleOffsetsPref>(),
];

impl Session {
    /// This session with `S` set to `value`, every other field intact.
    ///
    /// It writes the setting's TOP-LEVEL key. For per-profile settings that key is the frozen
    /// legacy fallback every profile reads until it picks its own, which no pick ever writes (the
    /// door writes `profile_prefs`: [`persist_profile_pref`]); so this is a test and fixture
    /// builder ("the install says X"), and `auto_sign_in` (TV-wide) is its only production-shaped row.
    pub fn with_pref<S: Setting>(&self, value: S::Value) -> Self {
        let mut next = self.clone();
        S::put(&mut next.prefs, value);
        next
    }
}

// ---- the memory door: per-profile, per-item lists ---------------------------------------------

/// **One profile's remembered entries of one kind**, oldest first: the record shape a per-profile
/// list shares (`subtitle_offsets` and `track_memory`). `{user, items, extensions}` is
/// exactly what `subtitle_offsets` was on disk before the door, so a file is read and written
/// byte for byte as before; `account` is the stamp of an account-bound list and is skipped while
/// empty. `subtitle_offsets` and `track_memory` are the two tenants, and both are account-bound, so
/// every record they write carries the stamp. A `subtitle_offsets` file written before it was bound
/// holds records with none, which the list adopts ([`MemoryEntry::ADOPTS_UNSTAMPED`]) and so stamps
/// at its next write; a `track_memory` record without one is nobody's.
///
/// The list as a whole is a `Vec<ProfileList<E>>` held in a [`Session`] field and soft-parsed per
/// element, so one bad profile record drops only that record; [`ProfileMemory`] is the operations
/// over it, and [`MemoryEntry`] is what a tenant supplies.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default, bound(serialize = "E: Serialize", deserialize = "E: DeserializeOwned"))]
pub struct ProfileList<E> {
    /// The Plex Home user's `uuid`, or **empty for the account owner** (`LastLibrary`'s
    /// convention, and for the same reason).
    pub user: String,
    /// The account stamp of an account-bound list (`grant::account_key`), else empty. A value
    /// that is not a string reads as empty: an unreadable stamp binds to no account.
    #[serde(deserialize_with = "de_soft_string", skip_serializing_if = "String::is_empty")]
    pub account: String,
    /// Oldest first: a re-recorded entry moves to the end and the cap drains the front.
    #[serde(deserialize_with = "super::de_soft_vec")]
    pub items: Vec<E>,
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub extensions: OpaqueExtensions,
}

impl<E> Default for ProfileList<E> {
    fn default() -> Self {
        ProfileList { user: String::new(), account: String::new(), items: Vec::new(), extensions: OpaqueExtensions::default() }
    }
}

/// **A record of a per-profile list**: one profile's data under one account stamp, held in a
/// `Vec` of them in a [`Session`] field. [`ProfileList`] (the memory door's entries) and
/// [`ProfilePrefsRecord`] (the profiles' own playback and display values) are the two shapes, and the
/// identity rules every such list shares are written once, over this: how a profile's record is
/// found, what a foreign stamp does to the list, and when an emptied record goes.
pub trait ProfileKeyed: Sized {
    /// The Plex Home user's `uuid`, or empty for the account owner.
    fn user(&self) -> &str;
    /// The account stamp (`grant::account_key`), else empty.
    fn account(&self) -> &str;
    /// Nothing left to remember: the record goes when a write leaves it so.
    fn is_empty(&self) -> bool;
    /// The session list this record kind lives in.
    fn slot(session: &Session) -> &Vec<Self>;
    fn slot_mut(session: &mut Session) -> &mut Vec<Self>;
}

/// The identity operations over a list of [`ProfileKeyed`] records.
pub trait ProfileRecords<R: ProfileKeyed> {
    /// Where `user`'s record under the stamp `account` is.
    fn position_of(&self, user: &str, account: &str) -> Option<usize>;
    /// Drop every record not stamped `account` (another account's, or an unstamped one nobody can
    /// read): the records of a previous sign-in must not outlive the first write of the next.
    /// Returns whether any went.
    fn prune_foreign(&mut self, account: &str) -> bool;
    /// Drop the records a write left empty.
    fn prune_empty(&mut self);
}

impl<R: ProfileKeyed> ProfileRecords<R> for Vec<R> {
    fn position_of(&self, user: &str, account: &str) -> Option<usize> {
        self.iter().position(|r| r.user() == user && r.account() == account)
    }
    fn prune_foreign(&mut self, account: &str) -> bool {
        let before = self.len();
        self.retain(|r| r.account() == account);
        self.len() != before
    }
    fn prune_empty(&mut self) {
        self.retain(|r| !r.is_empty());
    }
}

impl<E: MemoryEntry> ProfileKeyed for ProfileList<E> {
    fn user(&self) -> &str {
        &self.user
    }
    fn account(&self) -> &str {
        &self.account
    }
    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    fn slot(session: &Session) -> &Vec<Self> {
        E::slot(session)
    }
    fn slot_mut(session: &mut Session) -> &mut Vec<Self> {
        E::slot_mut(session)
    }
}

/// What a tenant of the memory door supplies: its entry type, how an entry is identified, how many
/// a profile keeps, and which [`Session`] list holds it.
pub trait MemoryEntry: Serialize + DeserializeOwned + Clone + PartialEq {
    /// What identifies an entry within one profile's list (an item, a show).
    type Key: PartialEq + Clone + Send + 'static;
    /// The most entries one profile keeps; the oldest are dropped first.
    const CAP: usize;
    /// The list belongs to the signed-in ACCOUNT, not to the television: its records carry the
    /// account stamp (`grant::account_key`), a record stamped for another account reads as nothing,
    /// a write is refused without a stamp, and a write prunes every record that is not its own.
    /// Pair it with the key in `storage::state::ACCOUNT_BOUND_PREFERENCES` (`prefs_tests`' T-account
    /// holds the row to it), which is what makes sign-out drop the whole key. `false` leaves a
    /// tenant exactly as it was before this knob existed.
    const ACCOUNT_BOUND: bool = false;
    /// For an [`ACCOUNT_BOUND`](Self::ACCOUNT_BOUND) list that was written, unstamped, before it
    /// was bound (`subtitle_offsets`): a record with NO stamp is read as the signed-in account's
    /// own, and the first write to the list stamps it ([`ProfileMemory::upsert_with`]). Today's
    /// visibility is kept, so nothing a viewer sees changes until the sign-out that now clears it.
    /// At THIS (plex) layer a tenant that adopts is also not refused without a stamp: its list
    /// reads and writes the unstamped records, prunes nothing and leaves any stamped record alone.
    /// The media door is stricter: it only reads or writes for a seated profile, and a seat needs a
    /// non-empty stamp, so a write with no account token never reaches the list through it.
    /// `track_memory` was bound from its first write, so an unstamped record is nobody's and stays
    /// `false`.
    const ADOPTS_UNSTAMPED: bool = false;
    /// The household budget, in logical bytes (platform's counting rule), for the whole list across
    /// every profile. Over it, a write drops the oldest entry of the record holding the most
    /// entries until the list fits (see [`ProfileMemory::upsert_with`]). `None` is no budget.
    const BUDGET: Option<usize> = None;
    fn key(&self) -> Self::Key;
    /// Whether this entry is the one `key` names: [`key`](Self::key) compared, and the scan over a
    /// list asks it per item, so a tenant whose key is made of its own strings overrides it to
    /// compare without cloning them.
    fn matches(&self, key: &Self::Key) -> bool {
        self.key() == *key
    }
    /// An entry that says "forget me" (or cannot be recorded): writing it removes the entry for
    /// its key and records nothing (an offset of 0 is Original, which needs no record).
    fn forgets(&self) -> bool;
    /// The entry as the list may hold it: strings cut to their bounds, an unusable value dropped.
    /// `upsert_with` runs every written entry through it, and a tenant with bounds parses through
    /// the same function, so a hand-edited file cannot break the budget arithmetic.
    fn normalized(self) -> Self {
        self
    }
    /// The session list this entry kind lives in.
    fn slot(session: &Session) -> &Vec<ProfileList<Self>>;
    fn slot_mut(session: &mut Session) -> &mut Vec<ProfileList<Self>>;
}

impl<E: MemoryEntry> ProfileList<E> {
    /// The most entries one profile keeps ([`MemoryEntry::CAP`]).
    pub const CAP: usize = E::CAP;

    /// The entry recorded for `key`, if any.
    pub fn find(&self, key: &E::Key) -> Option<&E> {
        self.items.iter().find(|e| e.matches(key))
    }

    /// Replace the entry for `key` with `next` as the most recent, or remove it when `next` is
    /// `None` or [`forgets`](MemoryEntry::forgets), then drain the oldest past the cap (T4).
    fn place(&mut self, key: &E::Key, next: Option<E>) {
        self.items.retain(|e| !e.matches(key));
        let Some(next) = next.filter(|e| !e.forgets()) else { return };
        debug_assert!(next.key() == *key, "an upserted entry keeps the key it was upserted under");
        self.items.push(next);
        let excess = self.items.len().saturating_sub(E::CAP);
        self.items.drain(..excess);
    }
}

/// The operations over a session's list of [`ProfileList`]s: the one place a per-profile entry is
/// found, merged and pruned, so a tenant cannot spell its own.
pub trait ProfileMemory<E: MemoryEntry> {
    /// `user`'s entry for `key` in the record stamped `account`.
    fn get(&self, user: &str, account: &str, key: &E::Key) -> Option<&E>;

    /// **Merge a change against what the list holds NOW** (T3). `f` receives the current entry for
    /// `key` and returns the entry to hold (`None`, or an entry that
    /// [`forgets`](MemoryEntry::forgets), removes it). It runs inside the write closure against
    /// the FILE's list, never against the caller's earlier copy, so a delta built from a half of
    /// an entry (one new pick over a diverged file) merges instead of overwriting the other half.
    /// A changed entry moves to the end and the cap drains from the front; a profile record whose
    /// items become empty is dropped (T4), so the list never carries empties.
    ///
    /// **An account-bound tenant** ([`MemoryEntry::ACCOUNT_BOUND`]) is refused without a stamp
    /// (nothing is written, `f` is not called), and a write first PRUNES every record that is not
    /// stamped `account` (another account's, or an unstamped one nobody can read): the record of a
    /// previous account must not outlive the first write of the next. A tenant that
    /// [adopts](MemoryEntry::ADOPTS_UNSTAMPED) first stamps its unstamped records `account`
    /// instead (so they are not pruned), and is not refused without a stamp: with none it prunes
    /// and adopts nothing.
    ///
    /// **A tenant with a budget** ([`MemoryEntry::BUDGET`]) is brought back under it by EVICTION,
    /// never by refusing the write: while the whole list is over, drop the oldest entry of the
    /// record holding the MOST entries; on a tie a record other than the writer's goes first (then
    /// the earlier record). A light viewer is never evicted by a heavy one, and the writer's newest
    /// pick always stays.
    ///
    /// Returns whether the list changed. An entry equal to what the list already holds is not a
    /// change: it is neither rewritten nor moved (pruning a foreign record, or adopting an
    /// unstamped one, is a change).
    fn upsert_with(&mut self, user: &str, account: &str, key: &E::Key, f: impl FnOnce(Option<&E>) -> Option<E>) -> bool;
}

impl<E: MemoryEntry> ProfileMemory<E> for Vec<ProfileList<E>> {
    fn get(&self, user: &str, account: &str, key: &E::Key) -> Option<&E> {
        if E::ACCOUNT_BOUND && account.is_empty() && !E::ADOPTS_UNSTAMPED {
            return None;
        }
        let own = self.position_of(user, account).and_then(|at| self[at].find(key));
        // A record not yet adopted is read as this account's own, behind the stamped one.
        own.or_else(|| {
            if !E::ADOPTS_UNSTAMPED {
                return None;
            }
            self.position_of(user, "").and_then(|at| self[at].find(key))
        })
    }

    fn upsert_with(&mut self, user: &str, account: &str, key: &E::Key, f: impl FnOnce(Option<&E>) -> Option<E>) -> bool {
        if E::ACCOUNT_BOUND && account.is_empty() && !E::ADOPTS_UNSTAMPED {
            return false;
        }
        let mut pruned = false;
        if E::ACCOUNT_BOUND && !account.is_empty() {
            if E::ADOPTS_UNSTAMPED {
                pruned = adopt_unstamped(self, account);
            }
            pruned |= self.prune_foreign(account);
        }
        let at = self.position_of(user, account);
        let current = at.and_then(|i| self[i].find(key));
        let next = f(current).map(E::normalized).filter(|e| !e.forgets());
        if current == next.as_ref() {
            return pruned;
        }
        let at = at.unwrap_or_else(|| {
            self.push(ProfileList { user: user.to_string(), account: account.to_string(), ..ProfileList::default() });
            self.len() - 1
        });
        self[at].place(key, next);
        self.prune_empty();
        if let Some(budget) = E::BUDGET {
            let total = list_size(self);
            evict_by_rule(self, Some((user, account)), |freed| total.saturating_sub(freed) > budget);
        }
        true
    }
}

/// **Adopt the records nobody stamped** (an [`MemoryEntry::ADOPTS_UNSTAMPED`] list written before it
/// was bound): each becomes `account`'s own. A profile that already has a record stamped `account`
/// takes the unstamped one's entries behind its own (the stamped record's entry wins a clash, the
/// cap drains the oldest), so the list never holds two records for one `(user, account)`. Returns
/// whether anything changed.
fn adopt_unstamped<E: MemoryEntry>(list: &mut Vec<ProfileList<E>>, account: &str) -> bool {
    let mut changed = false;
    let mut at = 0;
    while at < list.len() {
        if !list[at].account.is_empty() {
            at += 1;
            continue;
        }
        changed = true;
        let Some(own) = list.position_of(&list[at].user, account) else {
            list[at].account = account.to_string();
            at += 1;
            continue;
        };
        let Some(legacy) = list.drain(at..=at).next() else { break };
        let own = if own > at { own - 1 } else { own };
        let target = &mut list[own];
        let mut items: Vec<E> = legacy.items.into_iter().filter(|e| target.find(&e.key()).is_none()).collect();
        items.append(&mut target.items);
        let excess = items.len().saturating_sub(E::CAP);
        items.drain(..excess);
        target.items = items;
    }
    changed
}

// ---- the household budget: one eviction rule, shared by the write door and the document guard ----

/// The logical size (platform's counting rule) of anything serializable: the unit every byte
/// figure in this section is in.
fn logical<T: Serialize + ?Sized>(value: &T) -> usize {
    serde_json::to_value(value).map(|v| plx_platform::storage::state::logical_size(&v)).unwrap_or(0)
}

/// The logical size of a whole memory list as it serializes into the preferences.
pub(super) fn list_size<E: MemoryEntry>(list: &[ProfileList<E>]) -> usize {
    logical(list)
}

/// **A per-profile record the eviction rule works on**: the entries of one profile, oldest first.
/// [`ProfileList`] is the record of every list that goes through the memory door, and `LibraryViews`
/// (`library_sorts`, the older list that is not one) is a record of its own; the rule below is
/// written once, over this, so that neither has a copy of it.
pub(super) trait EvictionRecord: Serialize + ProfileKeyed {
    /// What the record holds, in the order it was recorded.
    type Entry: Serialize;
    fn entries(&self) -> &Vec<Self::Entry>;
    fn entries_mut(&mut self) -> &mut Vec<Self::Entry>;
}

impl<E: MemoryEntry> EvictionRecord for ProfileList<E> {
    type Entry = E;
    fn entries(&self) -> &Vec<E> {
        &self.items
    }
    fn entries_mut(&mut self) -> &mut Vec<E> {
        &mut self.items
    }
}

/// `library_sorts` is keyed by profile only: its records carry no account stamp.
impl ProfileKeyed for super::LibraryViews {
    fn user(&self) -> &str {
        &self.user
    }
    fn account(&self) -> &str {
        ""
    }
    fn is_empty(&self) -> bool {
        self.libs.is_empty()
    }
    fn slot(session: &Session) -> &Vec<Self> {
        &session.prefs.library_views
    }
    fn slot_mut(session: &mut Session) -> &mut Vec<Self> {
        &mut session.prefs.library_views
    }
}

impl EvictionRecord for super::LibraryViews {
    type Entry = super::LibraryView;
    fn entries(&self) -> &Vec<super::LibraryView> {
        &self.libs
    }
    fn entries_mut(&mut self) -> &mut Vec<super::LibraryView> {
        &mut self.libs
    }
}

/// The record to take from next: the one holding the MOST entries; on a tie a record other than the
/// writer's goes first, then the earlier record. `None` when only the writer's newest pick is left
/// (it always stays).
fn victim<R: EvictionRecord>(list: &[R], writer: Option<(&str, &str)>) -> Option<usize> {
    let is_writer = |r: &R| writer.is_some_and(|(u, a)| r.user() == u && r.account() == a);
    let mut best: Option<usize> = None;
    for (i, r) in list.iter().enumerate() {
        let better = best.is_none_or(|b| {
            let held = &list[b];
            r.entries().len() > held.entries().len()
                || (r.entries().len() == held.entries().len() && is_writer(held) && !is_writer(r))
        });
        if better {
            best = Some(i);
        }
    }
    best.filter(|&i| !(is_writer(&list[i]) && list[i].entries().len() == 1))
}

/// **The eviction rule**: drop the oldest entry of the record holding the most entries (ties: see
/// [`victim`]) for as long as `still_over(freed)` says the list must shrink, `freed` being the
/// logical bytes the evictions so far took out of the list. Returns `(entries evicted, bytes
/// freed)`.
///
/// The bytes are counted as the list's own serialization loses them, without re-serializing it per
/// step: an entry that leaves a record with other entries takes its size and one comma with it; the
/// last entry of a record takes the whole record (its size, and one comma unless it was the only
/// record). `evict_exactness` in the tests holds this to a fresh measurement after every step.
pub(super) fn evict_by_rule<R: EvictionRecord>(
    list: &mut Vec<R>,
    writer: Option<(&str, &str)>,
    mut still_over: impl FnMut(usize) -> bool,
) -> (usize, usize) {
    let (mut evicted, mut freed) = (0, 0);
    while still_over(freed) {
        let Some(at) = victim(list, writer) else { break };
        freed += if list[at].entries().len() > 1 {
            logical(&list[at].entries()[0]) + 1
        } else {
            logical(&list[at]) + usize::from(list.len() > 1)
        };
        list[at].entries_mut().drain(..1);
        if list[at].entries().is_empty() {
            list.drain(at..=at);
        }
        evicted += 1;
    }
    (evicted, freed)
}

/// **What the document guard may shed**, in the order it sheds it: the per-profile memory lists in
/// the preferences payload about to be written (`persistence::commit_session_with`). Each list is
/// brought back through its typed form and shed by the SAME rule as the write door's budget
/// ([`evict_by_rule`]; `library_sorts`, which is not a [`ProfileList`], through the same function
/// over its own record type, [`EvictionRecord`]), so "by the same rule" is the same code, not a copy
/// of it.
///
/// `still_over(freed)` is asked before every step with the logical bytes this call has taken out of
/// `preferences` so far, and the call stops as soon as it says no. Returns the lists that lost
/// entries and how many each lost. A list that empties leaves the payload (all three skip
/// themselves while empty), and a payload that does not hold a list as its typed form leaves it
/// alone: shedding is for data this build wrote, never a reason to rewrite what it cannot read.
pub(super) fn shed_memory(
    preferences: &mut serde_json::Value,
    mut still_over: impl FnMut(usize) -> bool,
) -> Vec<(&'static str, usize)> {
    let Some(object) = preferences.as_object_mut() else { return Vec::new() };
    let freed_total = std::cell::Cell::new(0usize);
    let mut shed = Vec::new();
    let mut note = |name: &'static str, (evicted, freed): (usize, usize)| {
        freed_total.set(freed_total.get() + freed);
        if evicted > 0 {
            shed.push((name, evicted));
        }
    };
    note("track_memory", shed_list::<ProfileList<TrackMemoryEntry>>(object, "track_memory", freed_total.get(), &mut still_over));
    note(
        "subtitle_offsets",
        shed_list::<ProfileList<SubtitleOffsetEntry>>(object, "subtitle_offsets", freed_total.get(), &mut still_over),
    );
    note("library_sorts", shed_list::<super::LibraryViews>(object, "library_sorts", freed_total.get(), &mut still_over));
    shed
}

/// One list of the payload, brought back through its typed form `R`, shed by [`evict_by_rule`] and
/// written back (or removed, when it empties). A value that is not a list of `R` is left alone.
fn shed_list<R: EvictionRecord + DeserializeOwned>(
    object: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    freed_before: usize,
    still_over: &mut impl FnMut(usize) -> bool,
) -> (usize, usize) {
    let Some(mut list) = object.get(key).and_then(|v| serde_json::from_value::<Vec<R>>(v.clone()).ok()) else {
        return (0, 0);
    };
    let (evicted, freed) = evict_by_rule(&mut list, None, |f| still_over(freed_before + f));
    if evicted > 0 {
        match list.is_empty() {
            true => {
                object.remove(key);
            }
            false => {
                if let Ok(value) = serde_json::to_value(&list) {
                    object.insert(key.to_string(), value);
                }
            }
        }
    }
    (evicted, freed)
}

impl MemoryEntry for SubtitleOffsetEntry {
    type Key = (String, String);
    /// A household corrects a handful of mistimed files; the cap keeps the public payload bounded
    /// the same way `LibraryViews::CAP` does, for the same reason. It bounds ONE profile: past
    /// 256 KiB for the whole document the document guard (`persistence::fit_document`,
    /// `prefs::shed_memory`) may also evict this list's oldest entries from the largest record.
    const CAP: usize = 24;
    /// Sign-out drops the whole key (SO), so every record carries the account stamp.
    const ACCOUNT_BOUND: bool = true;
    /// Records written before the stamp existed are the signed-in account's own.
    const ADOPTS_UNSTAMPED: bool = true;
    fn key(&self) -> Self::Key {
        (self.machine_id.clone(), self.rating_key.clone())
    }
    fn matches(&self, key: &Self::Key) -> bool {
        self.machine_id == key.0 && self.rating_key == key.1
    }
    /// Original (0) needs no record, and an item with no machine id or ratingKey is never recorded
    /// (`LastLibrary::set`'s reason).
    fn forgets(&self) -> bool {
        self.machine_id.is_empty() || self.rating_key.is_empty() || self.offset_ms == 0
    }
    fn slot(session: &Session) -> &Vec<ProfileList<Self>> {
        &session.prefs.subtitle_offsets
    }
    fn slot_mut(session: &mut Session) -> &mut Vec<ProfileList<Self>> {
        &mut session.prefs.subtitle_offsets
    }
}

/// The `subtitle_offsets` row: one item, one profile, bound to the account (sign-out drops it, and
/// every record carries the account stamp; a record written before the stamp is adopted at its
/// next write), applied by the player's own restore (not a live cell), with no Settings row. A [`Row`] and
/// deliberately not a [`ListRow`], like [`TrackMemoryPref`]: a memory-door tenant has no whole-list
/// accessor, so it is written only through [`ProfileMemory::upsert_with`].
pub struct SubtitleOffsetsPref;
impl Row for SubtitleOffsetsPref {
    const KEY: &'static str = "subtitle_offsets";
    const SCOPE: Scope = Scope::PerItem;
    const ACCOUNT_BOUND: bool = true;
    const LIVE: bool = false;
    const SETTINGS: Option<SettingsWrite> = None;
    const PARSE: Parse = Parse::SoftVec;
}

impl ProfileList<SubtitleOffsetEntry> {
    /// This profile's remembered correction for one item, if it ever tuned one.
    pub fn get(&self, machine_id: &str, rating_key: &str) -> Option<i64> {
        if machine_id.is_empty() || rating_key.is_empty() {
            return None;
        }
        self.items.iter().find(|e| e.machine_id == machine_id && e.rating_key == rating_key)
            .map(|e| e.offset_ms)
    }

    /// Record an item's correction as the most recent, or FORGET it with `None` (the viewer set
    /// the offset back to Original, which needs no record to be restored). Evicts the oldest
    /// entries past [`CAP`](Self::CAP). An item with no machine id or ratingKey is never recorded.
    pub fn set(&mut self, machine_id: &str, rating_key: &str, offset_ms: Option<i64>) {
        let entry = offset_ms.map(|offset_ms| SubtitleOffsetEntry {
            machine_id: machine_id.to_string(),
            rating_key: rating_key.to_string(),
            offset_ms,
            extensions: Default::default(),
        });
        self.place(&(machine_id.to_string(), rating_key.to_string()), entry);
    }
}

impl Session {
    /// This session with its `E` list edited by `edit`, or `None` when `edit` reports no change
    /// (so a queued update writes nothing). The closure is the memory door's write half:
    /// `cur.with_memory::<E>(|m| m.upsert_with(..))`.
    pub fn with_memory<E: MemoryEntry>(&self, edit: impl FnOnce(&mut Vec<ProfileList<E>>) -> bool) -> Option<Session> {
        let mut next = self.clone();
        edit(E::slot_mut(&mut next)).then_some(next)
    }
}

/// **The revocation tenure**: a counter that moves whenever the session is revoked (sign-out, a
/// replaced account). A reader of the memory door's mirror compares it against the one the mirror
/// was seeded under, and `queue_update` refuses an edit queued under an older one.
pub fn tenure() -> u64 {
    super::REVOCATION_GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

/// A stamp that is not a string binds to no account, instead of costing the record.
fn de_soft_string<'de, D>(d: D) -> Result<String, D::Error>
where D: Deserializer<'de> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(value.as_str().map(str::to_owned).unwrap_or_default())
}

/// Anything this build cannot read is "no value of its own" (`None`), which reads the fallback: the
/// read of a profile's own setting (`profile_prefs`) and of one half of a show's track memory.
fn de_soft_opt<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value::<Option<T>>(value).unwrap_or(None))
}

/// Future or malformed direct-play policies retain the automatic compatibility checks.
fn de_soft_direct_play_mode<'de, D>(d: D) -> Result<DirectPlayMode, D::Error>
where D: Deserializer<'de> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// Playback quality is a preference, not a credential gate. A value written by a newer build or
/// damaged by a hand edit therefore degrades to the legacy-safe Original mode rather than making
/// the enclosing [`Session`] disappear.
fn de_soft_playback_quality<'de, D>(d: D) -> Result<Option<PlaybackQuality>, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(None);
    };
    Ok(serde_json::from_value::<Option<PlaybackQuality>>(v).unwrap_or(None))
}

/// The subtitle tone is a preference too: a spelling this build does not know degrades to white.
fn de_soft_subtitle_tone<'de, D>(d: D) -> Result<SubtitleTone, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(SubtitleTone::White);
    };
    Ok(serde_json::from_value::<SubtitleTone>(v).unwrap_or_default())
}

/// The subtitle size is a preference too: a spelling this build does not know degrades to Medium.
fn de_soft_subtitle_size<'de, D>(d: D) -> Result<SubtitleSize, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(SubtitleSize::Medium);
    };
    Ok(serde_json::from_value::<SubtitleSize>(v).unwrap_or_default())
}

/// `skip_serializing_if` needs a function, not just `PartialEq` with `Default::default()`.
fn is_default_subtitle_size(size: &SubtitleSize) -> bool {
    *size == SubtitleSize::default()
}

/// The subtitle position is a preference too: a spelling this build does not know degrades to Low.
fn de_soft_subtitle_position<'de, D>(d: D) -> Result<SubtitlePosition, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(SubtitlePosition::Low);
    };
    Ok(serde_json::from_value::<SubtitlePosition>(v).unwrap_or_default())
}

/// `skip_serializing_if` needs a function, not just `PartialEq` with `Default::default()`.
fn is_default_subtitle_position(position: &SubtitlePosition) -> bool {
    *position == SubtitlePosition::default()
}

/// The HDR overlay brightness is a preference too: a spelling this build does not know degrades to the default (75%).
fn de_soft_hdr_overlay_brightness<'de, D>(d: D) -> Result<HdrOverlayBrightness, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(HdrOverlayBrightness::default());
    };
    Ok(serde_json::from_value::<HdrOverlayBrightness>(v).unwrap_or_default())
}

/// `skip_serializing_if` needs a function, not just `PartialEq` with `Default::default()`.
fn is_default_hdr_overlay_brightness(b: &HdrOverlayBrightness) -> bool {
    *b == HdrOverlayBrightness::default()
}

/// The next-episode mode is a preference too: a spelling this build does not know degrades to Countdown.
fn de_soft_next_episode_mode<'de, D>(d: D) -> Result<NextEpisodeMode, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(NextEpisodeMode::Countdown);
    };
    Ok(serde_json::from_value::<NextEpisodeMode>(v).unwrap_or_default())
}

/// `skip_serializing_if` needs a function, not just `PartialEq` with `Default::default()`.
fn is_default_next_episode_mode(mode: &NextEpisodeMode) -> bool {
    *mode == NextEpisodeMode::default()
}

/// The skip interval is a preference too: a spelling this build does not know degrades to 10 s.
fn de_soft_skip_interval<'de, D>(d: D) -> Result<SkipInterval, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(SkipInterval::default());
    };
    Ok(serde_json::from_value::<SkipInterval>(v).unwrap_or_default())
}

/// `skip_serializing_if` needs a function, not just `PartialEq` with `Default::default()`.
fn is_default_skip_interval(interval: &SkipInterval) -> bool {
    *interval == SkipInterval::default()
}

/// Instant D-pad seeking is a preference too: a spelling this build does not know degrades to Off.
fn de_soft_instant_dpad_seeking<'de, D>(d: D) -> Result<InstantDpadSeeking, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(InstantDpadSeeking::default());
    };
    Ok(serde_json::from_value::<InstantDpadSeeking>(v).unwrap_or_default())
}

/// Dolby Vision mode is a preference too: a spelling this build does not know degrades to Auto.
fn de_soft_dolby_vision_mode<'de, D>(d: D) -> Result<DolbyVisionMode, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(DolbyVisionMode::default());
    };
    Ok(serde_json::from_value::<DolbyVisionMode>(v).unwrap_or_default())
}

/// The deck-press mode is a preference too: a spelling this build does not know degrades to Details.
fn de_soft_deck_press<'de, D>(d: D) -> Result<DeckPress, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(DeckPress::default());
    };
    Ok(serde_json::from_value::<DeckPress>(v).unwrap_or_default())
}

/// `skip_serializing_if` needs a function, not just `PartialEq` with `Default::default()`.
fn is_default_deck_press(mode: &DeckPress) -> bool {
    *mode == DeckPress::default()
}

/// The under-posters record is a preference too: a value that is not a record is the default, and
/// each field inside softens on its own (`UnderPosters`' own deserializer).
fn de_soft_under_posters<'de, D>(d: D) -> Result<UnderPosters, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(UnderPosters::default());
    };
    Ok(serde_json::from_value::<UnderPosters>(v).unwrap_or_default())
}

/// `skip_serializing_if` needs a function, not just `PartialEq` with `Default::default()`.
fn is_default_under_posters(v: &UnderPosters) -> bool {
    *v == UnderPosters::default()
}

/// The interface resolution is a preference too: a spelling this build does not know is Automatic.
pub(super) fn de_soft_ui_resolution<'de, D>(d: D) -> Result<UiResolution, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(UiResolution::default());
    };
    Ok(serde_json::from_value::<UiResolution>(v).unwrap_or_default())
}

/// The taught flags are a preference too: an unreadable shape is "not taught".
pub(super) fn de_soft_hold_hint_taught<'de, D>(d: D) -> Result<HoldHintTaught, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(HoldHintTaught::NONE);
    };
    Ok(serde_json::from_value::<HoldHintTaught>(v).unwrap_or(HoldHintTaught::NONE))
}

pub(super) fn is_default_hold_hint_taught(taught: &HoldHintTaught) -> bool {
    *taught == HoldHintTaught::NONE
}

/// `skip_serializing_if` needs a function, not just `PartialEq` with `Default::default()`.
pub(super) fn is_default_ui_resolution(mode: &UiResolution) -> bool {
    *mode == UiResolution::default()
}

/// The audio-enhancement toggle is a preference too: an unknown shape degrades to both flags
/// off rather than failing the enclosing [`Session`].
fn de_soft_audio_enhancements<'de, D>(d: D) -> Result<crate::plex::AudioEnhancements, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(crate::plex::AudioEnhancements::NONE);
    };
    Ok(serde_json::from_value::<crate::plex::AudioEnhancements>(v).unwrap_or(crate::plex::AudioEnhancements::NONE))
}

