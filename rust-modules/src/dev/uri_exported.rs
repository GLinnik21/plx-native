//! The exported-window override (`plxnative-uriexported`): the PRODUCTION URI backend, bound to the
//! exported window (`Binding::Exported`) on a webOS 5+ set whose firmware major the evidence
//! table has not admitted. Debug builds only: this file is compiled with `devtriggers` and
//! `play-uri` (see `dev.rs`), so a release binary carries none of it, and the read below is
//! `false` at compile time there.
//!
//! **What it changes.** Only the display path the composition hands the driver
//! (`adapters::backends::install_uri_product` passes [`armed`] to [`compose`], applied to
//! `uri_product_binding`'s answer): with the trigger present and a display in `VP_EXPORTED` mode,
//! `Exported` on the full 1920x1080 canvas instead of `Unbound` (what production composes for a major
//! the evidence table, `EXPORTED_STRATA`, has not admitted). Nothing else: the viewer still has to
//! opt in to the URI engine in Settings, the qualifier is still `ProductQualifier`, and a display in any other mode composes as it always
//! did (an ACB set keeps ACB, a set with no window path stays unbound).
//!
//! **Arming.** The file is read ONCE, at boot (`install_uri_product`), unlike the probe's per-Play
//! trigger, so it can be armed only by writing it before the app launches (plain `/tmp`, or the
//! jail's persistent path): a write through `/proc/<pid>/root` while the app runs comes after the
//! read and arms nothing. `docs/uri-mode.md` says so, next to the tester's arming routes. The
//! boot line that confirms it is `uri: product binding=Exported { canvas: (1920, 1080) } mode=2
//! evidence=override`, which [`compose`] logs after the production `uri: product binding=..` line; a
//! run whose log lacks it proves nothing about the product backend on the exported window.
//!
//! **Why a trigger and not a setting.** The override exists so a tester can run the product
//! backend on a release the evidence does not admit, and a release build must not be able to do
//! that. `plxnative-uriexported` is in `dev::DIAG`: it changes how one play is carried and nothing
//! about the boot, so a stale file must not move a later boot off the who's-watching picker.

use plx_play_uri::backend::{Binding, EXPORTED_CANVAS};
use plx_platform::tv::sink::VP_EXPORTED;
use std::os::raw::c_int;

/// Is the override armed? One `stat` of the trigger, taken by the composition at boot.
pub(crate) fn armed() -> bool {
    plx_base::devtrig::flag("uriexported")
}

/// The display path `install_uri_product` composes the driver with: `binding` is the production
/// answer (`uri_product_binding`), and it comes back untouched unless `armed` AND the display is
/// in `VP_EXPORTED` mode, when it is `Exported` on the full canvas and the line the tester reads is
/// logged. The override never applies to another display mode, so an ACB set keeps ACB and a set
/// with no window path stays unbound. The caller passes [`armed`], as the URI dev route's
/// `compose` is passed its trigger.
pub(crate) fn compose(binding: Binding, display_mode: c_int, armed: bool) -> Binding {
    if !armed || display_mode != VP_EXPORTED {
        return binding;
    }
    let binding = Binding::Exported { canvas: EXPORTED_CANVAS };
    plx_base::eventlog::log(&format!(
        "uri: product binding={binding:?} mode={display_mode} evidence=override"));
    binding
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_platform::tv::sink::{VP_ACB, VP_NONE};

    /// The binding `compose` yields for a production `binding` on a display in `mode`, and what it
    /// logged.
    fn composed(binding: Binding, mode: c_int, armed: bool) -> (Binding, String) {
        plx_base::eventlog::with_private_log(|| {
            let composed = compose(binding, mode, armed);
            (composed, std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default())
        })
    }

    /// Armed on a display in `VP_EXPORTED` mode the exported window is composed on the full canvas,
    /// and the boot line the tester reads says so. Nowhere else, and never without the flag.
    #[test]
    fn armed_it_composes_the_exported_window_only_on_an_exported_window_display() {
        let exported = Binding::Exported { canvas: EXPORTED_CANVAS };
        assert_eq!(EXPORTED_CANVAS, (1920, 1080));
        let (binding, line) = composed(Binding::Unbound, VP_EXPORTED, true);
        assert_eq!(binding, exported);
        assert!(line.contains("uri: product binding=Exported { canvas: (1920, 1080) } mode=2 evidence=override"), "{line}");
        let (binding, line) = composed(Binding::Unbound, VP_EXPORTED, false);
        assert_eq!(binding, Binding::Unbound, "without the flag the display composes as production did");
        assert!(line.is_empty(), "{line}");
        let (binding, line) = composed(Binding::Unbound, VP_NONE, true);
        assert_eq!(binding, Binding::Unbound, "no window path: nothing to override");
        assert!(line.is_empty(), "{line}");
        let (binding, line) = composed(Binding::Acb, VP_ACB, true);
        assert_eq!(binding, Binding::Acb, "an ACB set keeps its own binding");
        assert!(line.is_empty(), "{line}");
    }

    /// The trigger is the file `plxnative-uriexported` in the runtime root, and present means armed:
    /// the content is not read.
    #[test]
    fn the_trigger_is_armed_by_the_file_being_present() {
        plx_base::devtrig::with_private_triggers(|| {
            assert!(!super::armed(), "no file, not armed");
            std::fs::write(plx_base::devtrig::path("uriexported"), "").unwrap();
            assert!(super::armed(), "an empty file arms it");
        });
    }
}
