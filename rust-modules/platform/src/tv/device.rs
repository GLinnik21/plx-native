//! Which television this is: the firmware ([`Info`]) and the set ([`Hardware`]). The webOS port
//! reads them at boot and publishes them here; everything else reads them from here.
use std::sync::OnceLock;

/// What the set said about itself. Owned strings rather than borrows into the file, because the
/// file is read once and dropped; `OnceLock` because this is written exactly once, at boot, and
/// read from the render thread every frame the diagnostics panel is up.
#[derive(Debug, Default, Clone)]
pub struct Info {
    /// e.g. "4.10.2" — empty when unknown
    pub release: String,
    /// The exact firmware build, e.g. "6.5.3-47" (nyx's `core_os_release`: the release and the
    /// build id); falls back to `release` when the file has no such key, empty when unknown. A
    /// patch within one release changes this and not `release`.
    pub build: String,
    /// e.g. "goldilocks2-grampians" — webosbrew buckets firmware by this
    pub codename: String,
    /// e.g. "4.1.0"
    pub api: String,
    /// e.g. "webOS TV"
    pub name: String,
    /// leading component of `release`, or 0 when unknown
    pub major: u32,
}

static INFO: OnceLock<Info> = OnceLock::new();
static NO_INFO: Info = Info {
    release: String::new(),
    build: String::new(),
    codename: String::new(),
    api: String::new(),
    name: String::new(),
    major: 0,
};

/// What the set reported. All-empty with `major == 0` when the file could not be read — which is
/// the honest answer and is what the panel prints. A plain `get()` (never `get_or_init`), so a read
/// that races ahead of the boot probe sees the empty answer for itself and leaves the cell open for
/// [`publish_info`].
pub fn info() -> &'static Info {
    INFO.get().unwrap_or(&NO_INFO)
}

/// Whether a set on firmware `major` is offered the Interface resolution setting (and honours the
/// stored choice at boot): webOS 1-3 only. `major == 0`, the unknown answer, is NOT 1-3. A pure
/// function of the number so the boot gate and the Settings row share one rule and both test it
/// without a television.
pub const fn offers_ui_resolution(major: u32) -> bool {
    major >= 1 && major <= 3
}

/// [`offers_ui_resolution`] for this set, from the published [`info`].
pub fn ui_resolution_offered() -> bool {
    offers_ui_resolution(info().major)
}

/// The boot probe's answer. First write wins, like the `OnceLock` it lands in.
pub fn publish_info(info: Info) {
    let _ = INFO.set(info);
}

/// The hardware, for a report that comes from a television nobody here owns.
///
/// [`Info`] answers "which firmware"; this answers "which SET". They are different questions and
/// the second one has been unanswerable: a webOS 6 playback failure on an OLED and on an LCD of
/// the same firmware are two bugs, and nothing in a log said which had been seen. The **board** is
/// the SoC generation (`k8hp`, `o22`, …) and is the field a decode or plane failure actually
/// correlates with.
///
/// Every field is EMPTY when unknown, never a plausible default — same rule as [`Info`], for the
/// same reason: a snapshot that invents a model is worse than one that admits it does not know.
#[derive(Debug, Default, Clone)]
pub struct Hardware {
    /// e.g. "49SM9000PLA"
    pub model: String,
    /// e.g. "HE_DTV_W19H_AFAAABAA" or the SoC name — whichever key this firmware carries
    pub board: String,
    pub hw_revision: String,
}

impl Hardware {
    /// The set as one line — `model · board · hw` with the empty parts left out, and an EMPTY
    /// string when nothing answered (the caller decides what "unknown" reads as on its surface).
    /// One definition for the two photographable surfaces that print it, the diagnostics panel's
    /// "Set" row and the failure read-out's support line, so they cannot drift.
    pub fn set_line(&self) -> String {
        [
            self.model.as_str(),
            self.board.as_str(),
            self.hw_revision.as_str(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
    }
}

static HW: OnceLock<Hardware> = OnceLock::new();
static NO_HARDWARE: Hardware = Hardware {
    model: String::new(),
    board: String::new(),
    hw_revision: String::new(),
};

/// What the set is. All-empty when the file could not be read; read with `get()` like [`info`].
///
/// Shared by the opt-in compatibility telemetry and the local lab snapshot. The values come from
/// the same boot probe, so diagnostics never need to rediscover or reinterpret the device later.
pub fn device() -> &'static Hardware {
    HW.get().unwrap_or(&NO_HARDWARE)
}

/// The boot probe's answer. First write wins, like the `OnceLock` it lands in.
pub fn publish_hardware(hw: Hardware) {
    let _ = HW.set(hw);
}

/// What the boot probe found in ONE of nyx's two files. A closed vocabulary, so an empty `model`
/// or `release` in a report says why it is empty instead of looking like a field the firmware
/// never had.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum NyxFile {
    /// The probe has not published yet (a read racing ahead of boot, or a host build).
    #[default]
    Unprobed,
    /// The file was read and at least one expected key answered.
    Ok,
    /// The file was read and none of the expected keys answered: a firmware that names them
    /// differently, an empty object, or text that is not the flat JSON this parser reads.
    NoKeys,
    /// The file could not be read at all (absent, or not visible to the app's jail).
    Unreadable,
}

impl NyxFile {
    /// The wire spelling — one of four fixed words, never free text.
    pub fn code(self) -> &'static str {
        match self {
            Self::Unprobed => "unprobed",
            Self::Ok => "ok",
            Self::NoKeys => "no_keys",
            Self::Unreadable => "unreadable",
        }
    }
}

/// What the boot probe found in each of nyx's two files: `os_info.json` (the webOS release, which
/// fills [`Info`]) and `device_info.json` (the set, which fills [`Hardware`]).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub os_info: NyxFile,
    pub device_info: NyxFile,
}

impl Probe {
    /// `<os_info>/<device_info>`, each one of [`NyxFile::code`]'s four words — e.g. `ok/ok`,
    /// `unreadable/no_keys`. The closed value the hardware context carries.
    pub fn code(self) -> String {
        format!("{}/{}", self.os_info.code(), self.device_info.code())
    }
}

static PROBE: OnceLock<Probe> = OnceLock::new();

/// What the boot probe found. [`Probe::default`] (`unprobed/unprobed`) until it publishes; a plain
/// `get()` like [`info`], so an early read leaves the cell open for [`publish_probe`].
pub fn probe() -> Probe {
    PROBE.get().copied().unwrap_or_default()
}

/// The boot probe's account of its own reads. First write wins.
pub fn publish_probe(probe: Probe) {
    let _ = PROBE.set(probe);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Interface resolution setting is webOS 1-3; unknown (0) and 4+ are not.
    #[test]
    fn the_interface_resolution_is_offered_on_webos_1_to_3_only() {
        for (major, offered) in [(0, false), (1, true), (2, true), (3, true), (4, false), (5, false), (6, false), (22, false)] {
            assert_eq!(offers_ui_resolution(major), offered, "major {major}");
        }
    }

    #[test]
    fn the_probe_code_is_two_closed_words() {
        assert_eq!(Probe::default().code(), "unprobed/unprobed");
        let p = Probe { os_info: NyxFile::Unreadable, device_info: NyxFile::NoKeys };
        assert_eq!(p.code(), "unreadable/no_keys");
        let p = Probe { os_info: NyxFile::Ok, device_info: NyxFile::Ok };
        assert_eq!(p.code(), "ok/ok");
    }

    #[test]
    fn the_set_line_omits_what_is_unknown_and_never_invents_a_set() {
        let hw = Hardware {
            model: "49SM9000PLA".into(),
            board: "HE_DTV_W19H".into(),
            hw_revision: String::new(),
        };
        assert_eq!(hw.set_line(), "49SM9000PLA · HE_DTV_W19H");
        assert_eq!(Hardware::default().set_line(), "");
    }
}
