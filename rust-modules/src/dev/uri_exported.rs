//! The exported-window override (`plxnative-uriexported`): the PRODUCTION URI backend, bound to the
//! exported window (`Binding::Exported`) on a webOS 5+ set whose firmware release the evidence
//! table has not admitted. Debug builds only: this file is compiled with `devtriggers` and
//! `play-uri` (see `dev.rs`), so a release binary carries none of it, and the read below is
//! `false` at compile time there.
//!
//! **What it changes.** Only the display path the composition hands the driver
//! (`adapters::backends::uri_product_binding`, which consumes [`armed`]): with the trigger present
//! and a display in `VP_EXPORTED` mode, `Exported` on the full 1920x1080 canvas instead of
//! `Unbound`. Nothing else: the viewer still has to opt in to the URI engine in Settings, the
//! qualifier is still `ProductQualifier`, and a display in any other mode composes as it always
//! did (an ACB set keeps ACB, a set with no window path stays unbound).
//!
//! **Arming.** The file is read ONCE, at boot (`install_uri_product`), like every dev trigger, so it
//! has to be in place before the app launches; `docs/uri-mode.md` carries the tester steps. The
//! boot line that confirms it is `uri: product binding=Exported .. evidence=override`; a run whose
//! log lacks it proves nothing about the product backend on the exported window.
//!
//! **Why a trigger and not a setting.** The override exists so a tester can run the product
//! backend on a release the evidence does not admit, and a release build must not be able to do
//! that. `plxnative-uriexported` is in `dev::DIAG`: it changes how one play is carried and nothing
//! about the boot, so a stale file must not move a later boot off the who's-watching picker.

/// Is the override armed? One `stat` of the trigger, taken by the composition at boot.
pub(crate) fn armed() -> bool {
    plx_base::devtrig::flag("uriexported")
}

#[cfg(test)]
mod tests {
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
