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

#[cfg(test)]
mod tests {
    use super::*;

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
