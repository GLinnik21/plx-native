//! TRACE (exploration): Home -> Movies tab with the Collections page remembered.

use super::*;
use super::test_support::{World, DipImage, ImageHeld};

#[test]
fn trace_restore_from_home() {
    use plx_plex::plex::session::LibraryPage;
    use plx_screens::registry::HomeTab;
    use plx_ui::containers::transition::PageDip;
    let mut w = World::movies_library("trace-restore-collections", false);
    w.d.nav.tabs.stack.transition = Box::new(PageDip::new());
    let _held = ImageHeld;
    let mut dip = DipImage::default();
    let key = w.bridge.directory.view().sections()[0].key;
    w.bridge.remember_library_page("route-own", key, LibraryPage::Collections);
    super::nav_tab(&mut w.d, &mut w.bridge, HomeTab::Home, None, None);
    w.frames(60);
    eprintln!("on home: {:?}", w.d.top_arg());
    super::nav_tab(&mut w.d, &mut w.bridge, HomeTab::Movies, None, None);
    for i in 0..80 {
        let paint = w.step_dip(&mut dip, vec![]);
        let top = w.d.nav.top_page().map(|e| e.id);
        let focus = w.d.focus();
        let ents: Vec<_> = w.d.nav.tabs.stack.entries.iter().map(|e| (e.id, e.inst.is_some())).collect();
        let content = w.d.top_screen().and_then(|s| s.as_any()).and_then(|a| a.downcast_ref::<plx_screens::library::LibraryScreen>()).map(|p| (p.probe_content(), p.probe_viewport(focus).0));
        eprintln!("{i}: top={:?} {:?} focus={:?} content={:?} cap={:?} ents={:?}", top, w.d.top_arg(), focus.map(|k| (k.entry, k.elem)), content, paint.as_ref().map(|p| p.captures_page()), ents);
    }
}
