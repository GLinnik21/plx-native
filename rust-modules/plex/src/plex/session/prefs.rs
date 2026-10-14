//! **The install-wide player settings, declared once.**
//!
//! [`Preferences`] is the nine player scalars that used to be spelled, field for field, in
//! [`Session`] AND in the canonical (DB8) preference record: the struct field, the manual
//! `Default`, the copy in and out of the canonical form, the getter and a `with_<x>` builder, ten
//! sites per setting. It is flattened into BOTH on-disk forms (`#[serde(flatten)]`, declared before
//! each form's own `extensions` so that a flattened struct takes its keys first and the unknown-key
//! map still sees only keys nobody models), so the legacy file and the canonical public
//! preferences carry exactly the keys they always did, under exactly the attributes they always had.
//!
//! **Every serde attribute below is copied character for character from where it used to sit**,
//! and no two parse policies were unified: each row keeps the soft parse it was written with
//! (`Setting::PARSE` names it, and `prefs_tests::t_parse_each_row_keeps_its_declared_policy` drives it). A reader of the wire format
//! never sees this module.
//!
//! One [`Setting`] impl per field, keyed by a MARKER type (never by the value type: two settings
//! of one type, or a stored `Option` read as a plain value, would collide). It carries the key,
//! the `Stored` and `Value` types, the [`Scope`], whether the row is account-bound, whether the
//! media layer applies it live, how a Settings row saves it, and its parse policy. [`DESCRIPTORS`]
//! lists them, and the tests hold that table to the struct and to platform's account-bound list,
//! both ways, so a field without a row (or a row without a field) fails instead of drifting.
//!
//! **The tenth row is a LIST, not a scalar**: `track_memory`, each profile's remembered audio and
//! subtitle picks per show ([`TrackMemoryEntry`], in the memory door below). A list has no `Copy`
//! value to hand `persist_pref`, so it states its facts through [`ListSetting`] and its writes go
//! through [`ProfileMemory::upsert_with`], never through `put`.
use super::{
    DeckPress, DirectPlayMode, NextEpisodeMode, OpaqueExtensions, PlaybackQuality, Session, SkipInterval,
    SubtitleOffsetEntry, SubtitlePosition, SubtitleSize, SubtitleTone,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

mod track_memory;
#[cfg(test)]
mod track_memory_document_tests;
#[cfg(test)]
mod track_memory_tests;
pub use track_memory::{AudioMemo, SubtitleMemo, SubtitleTrackMemo, TrackMemoryEntry, TrackMemoryPref};

/// The nine install-wide player settings, flattened into [`Session`] and into the canonical
/// preference record, and the per-show track memory beside them (a list, see the module doc).
/// Defaults are each type's own `Default`, which A0 pins as equal to the soft parse's fallback.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Preferences {
    /// The install's playback-quality preference. `None` is deliberately distinct from an
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
    /// Install-wide original-stream override; malformed/future values remain automatic.
    #[serde(default, deserialize_with = "de_soft_direct_play_mode")]
    pub direct_play_mode: DirectPlayMode,
    /// **How bright the client-rendered subtitles are drawn** — white, or a rung of the grey
    /// ladder under it. Install-wide like [`Preferences::playback_quality`], because it answers a fact
    /// about the PANEL (an HDR picture maps graphics white to a searing level) rather than about
    /// whoever is watching. Absence is white, which is what every build before the field drew.
    /// Soft-parsed: an unknown spelling costs the preference, never the credentials.
    #[serde(default, deserialize_with = "de_soft_subtitle_tone")]
    pub subtitle_tone: SubtitleTone,
    /// **The client-rendered subtitle caption's text size.** Install-wide like
    /// [`Preferences::subtitle_tone`]. Absence is Medium — see [`SubtitleSize`]'s own doc. Skipped at
    /// the default so a session predating this field — committed replay fixtures included —
    /// serializes exactly as it did before.
    #[serde(default, deserialize_with = "de_soft_subtitle_size", skip_serializing_if = "is_default_subtitle_size")]
    pub subtitle_size: SubtitleSize,
    /// **The client-rendered subtitle caption's vertical placement.** Install-wide like
    /// [`Preferences::subtitle_tone`]. Absence is Low — see [`SubtitlePosition`]'s own doc. Skipped at
    /// the default for the same reason as [`Preferences::subtitle_size`].
    #[serde(default, deserialize_with = "de_soft_subtitle_position", skip_serializing_if = "is_default_subtitle_position")]
    pub subtitle_position: SubtitlePosition,
    /// **What the player does when an episode with a successor reaches its credits** — see
    /// [`NextEpisodeMode`]. Install-wide like [`Preferences::playback_quality`]. Absence is
    /// Countdown, which is what every build before the field did. Skipped at the default so a
    /// session predating this field — committed replay fixtures included — serializes exactly as
    /// it did before.
    #[serde(default, deserialize_with = "de_soft_next_episode_mode", skip_serializing_if = "is_default_next_episode_mode")]
    pub next_episode_mode: NextEpisodeMode,
    /// **How far one Left/Right press jumps in the player** — see [`SkipInterval`]. Install-wide
    /// like [`Preferences::playback_quality`], one value for both directions. Absence is 10 s, the
    /// hop every build before the field made. Skipped at the default so a session predating this
    /// field — committed replay fixtures included — serializes exactly as it did before.
    #[serde(default, deserialize_with = "de_soft_skip_interval", skip_serializing_if = "is_default_skip_interval")]
    pub skip_interval: SkipInterval,
    /// **What OK does on a Continue Watching card** — see [`DeckPress`]. Install-wide like
    /// [`Preferences::playback_quality`]. Absence is Details, so a session written before the field
    /// existed opens the page on OK. Skipped at the default so a session predating this field —
    /// committed replay fixtures included — serializes exactly as it did before.
    #[serde(default, deserialize_with = "de_soft_deck_press", skip_serializing_if = "is_default_deck_press")]
    pub deck_press: DeckPress,
    /// Plex Pass per-track transcoder DSP preference (issue #266): dialog boost / loudness
    /// normalization. `NONE` by default and skipped on write while `NONE`, so every session
    /// persisted before the field existed — and every viewer who never opted in — round-trips
    /// byte-identical to what it wrote before. Restored into `player::audio_enhancements` at boot
    /// and at the credentials handoff; written by `player::set_audio_enhancements` through the media door (`route::choose`, which saves via `persist_pref`). Soft-parsed
    /// like every preference in this struct: an unknown shape costs the preference, never the
    /// credentials.
    #[serde(default, deserialize_with = "de_soft_audio_enhancements", skip_serializing_if = "crate::plex::AudioEnhancements::is_none")]
    pub audio_enhancements: crate::plex::AudioEnhancements,
    // ---- per-profile memory lists (a list row, not a scalar: see `ListSetting`) ----
    /// **Each profile's remembered audio and subtitle picks, per show** — see [`TrackMemoryEntry`].
    /// A [`ProfileList`] per profile, soft-parsed per element so one junk record or entry costs
    /// only itself, and skipped while empty so a session that never remembered anything serializes
    /// exactly as it did before the field existed. Account-bound: the record carries the account
    /// stamp, a stamp that is not the signed-in account's reads as nothing, and sign-out drops the
    /// whole key (`storage::state::ACCOUNT_BOUND_PREFERENCES`). Written only through
    /// [`ProfileMemory::upsert_with`], which prunes foreign stamps and holds the household budget.
    #[serde(default, deserialize_with = "super::de_soft_vec", skip_serializing_if = "Vec::is_empty")]
    pub track_memory: Vec<ProfileList<TrackMemoryEntry>>,
}

/// How long a setting's value lives, and who owns the write. Nine of the ten rows are `Install`.
/// `track_memory` is `PerShow` and account-bound, and the `Profile`, `Account` and `PerItem` scopes
/// are still declared with no row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The television: shared by every profile, kept across sign-out, never account-bound.
    Install,
    /// The signed-in account: cleared with the credentials on sign-out.
    Account,
    /// One Plex Home profile.
    Profile,
    /// One show, for one profile.
    PerShow,
    /// One item, for one profile.
    PerItem,
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

/// One setting, keyed by a marker type.
pub trait Setting: 'static {
    /// The field's type in [`Preferences`]. `PartialEq`, because the write door skips a write whose
    /// STORED value already equals the pick ([`persist_pref`]).
    type Stored: PartialEq;
    /// What callers speak. Equal to `Stored` except where the stored form carries "never chosen"
    /// (quality: `Option<PlaybackQuality>` stored, `PlaybackQuality` spoken).
    type Value: Copy + PartialEq + Send + 'static;
    /// The on-disk key (a serde rename is honoured here).
    const KEY: &'static str;
    const SCOPE: Scope;
    /// Cleared with the credentials at sign-out (`storage::state::ACCOUNT_BOUND_PREFERENCES`).
    const ACCOUNT_BOUND: bool;
    /// The media layer applies the value live (a cell restored at boot and at the handoff).
    const LIVE: bool;
    /// How a Settings row saves it; `None` when no Settings row exists (the player panel owns it).
    const SETTINGS: Option<SettingsWrite>;
    const PARSE: Parse;
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
    ($marker:ident: $ty:ty = $field:ident, $key:literal, $scope:ident,
     account $bound:literal, live $live:literal, settings $settings:expr, parse $parse:ident) => {
        pub struct $marker;
        impl Setting for $marker {
            type Stored = $ty;
            type Value = $ty;
            const KEY: &'static str = $key;
            const SCOPE: Scope = Scope::$scope;
            const ACCOUNT_BOUND: bool = $bound;
            const LIVE: bool = $live;
            const SETTINGS: Option<SettingsWrite> = $settings;
            const PARSE: Parse = Parse::$parse;
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
impl Setting for QualityPref {
    type Stored = Option<PlaybackQuality>;
    type Value = PlaybackQuality;
    const KEY: &'static str = "playback_quality";
    const SCOPE: Scope = Scope::Install;
    const ACCOUNT_BOUND: bool = false;
    const LIVE: bool = true;
    const SETTINGS: Option<SettingsWrite> = Some(SettingsWrite::Confirm);
    const PARSE: Parse = Parse::OptionalSoft;
    fn stored(p: &Preferences) -> &Option<PlaybackQuality> { &p.playback_quality }
    fn stored_mut(p: &mut Preferences) -> &mut Option<PlaybackQuality> { &mut p.playback_quality }
    fn get(p: &Preferences) -> PlaybackQuality { p.playback_quality.unwrap_or(PlaybackQuality::Original) }
    fn to_stored(v: PlaybackQuality) -> Option<PlaybackQuality> { Some(v) }
}

setting!(DirectPlayModePref: DirectPlayMode = direct_play_mode, "direct_play_mode", Install,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(NextEpisodeModePref: NextEpisodeMode = next_episode_mode, "next_episode_mode", Install,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(SkipIntervalPref: SkipInterval = skip_interval, "skip_interval", Install,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(DeckPressPref: DeckPress = deck_press, "deck_press", Install,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(SubtitleSizePref: SubtitleSize = subtitle_size, "subtitle_size", Install,
    account false, live true, settings Some(SettingsWrite::Optimistic), parse Soft);
setting!(SubtitlePositionPref: SubtitlePosition = subtitle_position, "subtitle_position", Install,
    account false, live true, settings Some(SettingsWrite::Optimistic), parse Soft);
// Tone and enhancements have no Settings row: the player panel is their only writer.
setting!(SubtitleTonePref: SubtitleTone = subtitle_tone, "subtitle_tone", Install,
    account false, live true, settings None, parse Soft);
setting!(AudioEnhancementsPref: crate::plex::AudioEnhancements = audio_enhancements,
    "audio_enhancements", Install, account false, live true, settings None, parse Soft);

/// **The facts of a row whose field is a LIST.** [`Setting`] reads and writes a `Copy` value
/// through `put`/`get`, and a list has none: handing `put` a whole `Vec` would let a caller replace
/// every profile's memory with one assignment, which is exactly what the memory door exists to
/// prevent (`upsert_with` merges, prunes and budgets). So a list row declares only what the
/// descriptor table and the tie tests read, and has no value API at all.
pub trait ListSetting: 'static {
    /// The on-disk key (a serde rename is honoured here).
    const KEY: &'static str;
    const SCOPE: Scope;
    /// Cleared with the credentials at sign-out (`storage::state::ACCOUNT_BOUND_PREFERENCES`).
    const ACCOUNT_BOUND: bool;
    /// The media layer applies the value live (a cell restored at boot and at the handoff).
    const LIVE: bool;
    /// How a Settings row saves it; `None` when no Settings row exists.
    const SETTINGS: Option<SettingsWrite>;
    const PARSE: Parse;
}

/// A setting as data, so a test (and a later generic restore) can walk them all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub key: &'static str,
    pub scope: Scope,
    pub account_bound: bool,
    pub live: bool,
    pub settings: Option<SettingsWrite>,
    pub parse: Parse,
}

impl Descriptor {
    pub const fn of<S: Setting>() -> Self {
        Descriptor {
            key: S::KEY,
            scope: S::SCOPE,
            account_bound: S::ACCOUNT_BOUND,
            live: S::LIVE,
            settings: S::SETTINGS,
            parse: S::PARSE,
        }
    }

    /// The descriptor of a list row ([`ListSetting`]).
    pub const fn of_list<S: ListSetting>() -> Self {
        Descriptor {
            key: S::KEY,
            scope: S::SCOPE,
            account_bound: S::ACCOUNT_BOUND,
            live: S::LIVE,
            settings: S::SETTINGS,
            parse: S::PARSE,
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
    Descriptor::of::<NextEpisodeModePref>(),
    Descriptor::of::<SkipIntervalPref>(),
    Descriptor::of::<DeckPressPref>(),
    Descriptor::of::<AudioEnhancementsPref>(),
    Descriptor::of_list::<TrackMemoryPref>(),
];

/// **The persist half of the one write door** (`media::route::prefs::choose` is the live half):
/// make the record hold `S = v`, as a read-modify-write under the session lock, and say whether the
/// record holds it afterwards.
///
/// The skip compares STORED values, so for quality a legacy absent field (`None`) and a pick of
/// Original count as different, and the pick writes `Some(Original)`; an absent field stays absent
/// until an explicit choice. `true` is a `Durable` write, or no write at all because the record
/// already read the same (`read && unchanged`: there was nothing to refuse). `false` is a refused
/// or failed write, an unreadable record, and the revocation guard of
/// [`update_guarded_with_outcome`](super::update_guarded_with_outcome) (sign-out wins over an
/// install write queued before it).
pub fn persist_pref<S: Setting>(v: S::Value) -> bool {
    let (mut read, mut unchanged) = (false, false);
    let write = super::update_guarded_with_outcome(
        |cur| {
            read = true;
            if S::stored(&cur.prefs) == &S::to_stored(v) {
                unchanged = true;
                None
            } else {
                Some(cur.with_pref::<S>(v))
            }
        },
        || true,
    );
    match write {
        Some(write) => matches!(write.classify(), super::async_persistence::CompletionOutcome::Durable(_)),
        None => read && unchanged,
    }
}

impl Session {
    /// This session with `S` set to `value`, every other field intact.
    pub fn with_pref<S: Setting>(&self, value: S::Value) -> Self {
        let mut next = self.clone();
        S::put(&mut next.prefs, value);
        next
    }
}

// ---- the memory door: per-profile, per-item lists ---------------------------------------------

/// **One profile's remembered entries of one kind**, oldest first: the record shape a per-profile
/// list shares. `{user, items, extensions}` is
/// exactly what `subtitle_offsets` was on disk before the door, so a file is read and written
/// byte for byte as before; `account` is the stamp of an account-bound list and is skipped while
/// empty. `subtitle_offsets` and `track_memory` are the two tenants. `subtitle_offsets` is not
/// account-bound, so its `account` stays empty and writes no key. `track_memory` is account-bound,
/// so every record carries the stamp.
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
    /// holds the row to it). `false` leaves a tenant exactly as it was before this knob existed.
    const ACCOUNT_BOUND: bool = false;
    /// The household budget, in logical bytes (platform's counting rule), for the whole list across
    /// every profile. Over it, a write drops the oldest entry of the record holding the most
    /// entries until the list fits (see [`ProfileMemory::upsert_with`]). `None` is no budget.
    const BUDGET: Option<usize> = None;
    fn key(&self) -> Self::Key;
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
        self.items.iter().find(|e| e.key() == *key)
    }

    /// Replace the entry for `key` with `next` as the most recent, or remove it when `next` is
    /// `None` or [`forgets`](MemoryEntry::forgets), then drain the oldest past the cap (T4).
    fn place(&mut self, key: &E::Key, next: Option<E>) {
        self.items.retain(|e| e.key() != *key);
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
    /// previous account must not outlive the first write of the next.
    ///
    /// **A tenant with a budget** ([`MemoryEntry::BUDGET`]) is brought back under it by EVICTION,
    /// never by refusing the write: while the whole list is over, drop the oldest entry of the
    /// record holding the MOST entries; on a tie a record other than the writer's goes first (then
    /// the earlier record). A light viewer is never evicted by a heavy one, and the writer's newest
    /// pick always stays.
    ///
    /// Returns whether the list changed. An entry equal to what the list already holds is not a
    /// change: it is neither rewritten nor moved (pruning a foreign record is a change).
    fn upsert_with(&mut self, user: &str, account: &str, key: &E::Key, f: impl FnOnce(Option<&E>) -> Option<E>) -> bool;
}

impl<E: MemoryEntry> ProfileMemory<E> for Vec<ProfileList<E>> {
    fn get(&self, user: &str, account: &str, key: &E::Key) -> Option<&E> {
        if E::ACCOUNT_BOUND && account.is_empty() {
            return None;
        }
        self.iter().find(|r| r.user == user && r.account == account).and_then(|r| r.find(key))
    }

    fn upsert_with(&mut self, user: &str, account: &str, key: &E::Key, f: impl FnOnce(Option<&E>) -> Option<E>) -> bool {
        if E::ACCOUNT_BOUND && account.is_empty() {
            return false;
        }
        let before = self.len();
        if E::ACCOUNT_BOUND {
            self.retain(|r| r.account == account);
        }
        let pruned = self.len() != before;
        let at = self.iter().position(|r| r.user == user && r.account == account);
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
        self.retain(|r| !r.items.is_empty());
        if let Some(budget) = E::BUDGET {
            let total = list_size(self);
            evict_by_rule(self, Some((user, account)), |freed| total.saturating_sub(freed) > budget);
        }
        true
    }
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
pub(super) trait EvictionRecord: Serialize {
    /// What the record holds, in the order it was recorded.
    type Entry: Serialize;
    /// The Plex Home user's `uuid`, empty for the account owner.
    fn user(&self) -> &str;
    /// The account stamp of an account-bound record, else empty.
    fn account(&self) -> &str {
        ""
    }
    fn entries(&self) -> &Vec<Self::Entry>;
    fn entries_mut(&mut self) -> &mut Vec<Self::Entry>;
}

impl<E: MemoryEntry> EvictionRecord for ProfileList<E> {
    type Entry = E;
    fn user(&self) -> &str {
        &self.user
    }
    fn account(&self) -> &str {
        &self.account
    }
    fn entries(&self) -> &Vec<E> {
        &self.items
    }
    fn entries_mut(&mut self) -> &mut Vec<E> {
        &mut self.items
    }
}

impl EvictionRecord for super::LibraryViews {
    type Entry = super::LibraryView;
    fn user(&self) -> &str {
        &self.user
    }
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
    fn key(&self) -> Self::Key {
        (self.machine_id.clone(), self.rating_key.clone())
    }
    /// Original (0) needs no record, and an item with no machine id or ratingKey is never recorded
    /// (`LastLibrary::set`'s reason).
    fn forgets(&self) -> bool {
        self.machine_id.is_empty() || self.rating_key.is_empty() || self.offset_ms == 0
    }
    fn slot(session: &Session) -> &Vec<ProfileList<Self>> {
        &session.subtitle_offsets
    }
    fn slot_mut(session: &mut Session) -> &mut Vec<ProfileList<Self>> {
        &mut session.subtitle_offsets
    }
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

