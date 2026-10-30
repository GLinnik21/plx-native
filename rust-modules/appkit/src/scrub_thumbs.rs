//! **Scrubbing thumbnails**: the picture of the moment the viewer is scrubbing to, cut out of the
//! server's thumbnail sheets (`plx_plex::plex::Trickplay`) on a worker, for the HUD to draw above
//! the playhead.
//!
//! A sheet is one JPEG holding a grid of pictures (Jellyfin's default is 10×10 pictures 320 wide,
//! so a 16:9 sheet decodes to 3200×1800). The worker keeps ONE decoded sheet, as RGB, while the
//! viewer scrubs, because a hold-scrub walks through neighbouring pictures many times a second and
//! re-decoding the sheet for each would put the picture far behind the playhead. Only the picture
//! asked for is copied out (as RGBA, a few hundred KiB), and [`Trickplays::release`] drops the
//! decoded sheet the moment scrubbing ends, so it is resident only while it is in use. The sheet's
//! JPEG (a few hundred KiB) is kept until the item changes, so the next scrub near the same place
//! decodes it again without asking the server.
//!
//! The protocol is the sidecar worker's: the main thread states what it wants ([`Trickplays::want`]),
//! one worker at a time serves the latest want, and a generation drops a result that a change of
//! item or a release made stale. The main thread collects a finished picture with
//! [`Trickplays::take_ready`]; it owns the texture from there. There is no process-wide instance:
//! the player screen's render resources hold one ([`crate::player_hud::ScrubThumb`]), so it ends
//! with the screen.
use plx_plex::plex::{ServerId, Trickplay};
use std::sync::{Arc, Mutex, MutexGuard};

/// Where the layout and the sheets come from: the playing item's server, or a test's fake.
pub trait Source: Send + Sync + 'static {
    fn info(&self, server: ServerId, rk: &str, part: i64) -> Option<Trickplay>;
    fn sheet(&self, server: ServerId, rk: &str, part: i64, info: &Trickplay, sheet: u32) -> Option<Vec<u8>>;
}

/// The playing item's own server (`Client::trickplay`, `Client::trickplay_sheet`).
pub struct ServerSource;

impl Source for ServerSource {
    fn info(&self, server: ServerId, rk: &str, part: i64) -> Option<Trickplay> {
        plx_plex::plex::client_for(server)?.trickplay(rk, part)
    }
    fn sheet(&self, server: ServerId, rk: &str, part: i64, info: &Trickplay, sheet: u32) -> Option<Vec<u8>> {
        plx_plex::plex::client_for(server)?.trickplay_sheet(rk, part, info, sheet)
    }
}

/// One picture, ready to upload: `width`×`height` straight RGBA, tightly packed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tile {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Item {
    server: ServerId,
    rk: String,
    /// The part (version) that plays: its thumbnails, not the item's first version's.
    part: i64,
}

/// A decoded sheet: its number on the item, its size and its RGB pixels.
struct Sheet {
    number: u32,
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

#[derive(Default)]
struct State {
    item: Option<Item>,
    /// Moves on every change of item and every release; a worker result from an older one is
    /// dropped.
    generation: u64,
    /// The item's layout once asked: `Some(None)` is "this item has none", asked once per item.
    info: Option<Option<Trickplay>>,
    /// The position the viewer is scrubbing to, not yet served.
    want_ms: Option<i64>,
    running: bool,
    sheet: Option<Sheet>,
    /// The last sheet's JPEG, by number: kept across a release, dropped with the item.
    jpeg: Option<(u32, Arc<Vec<u8>>)>,
    /// A sheet that could not be fetched or decoded, not asked for again for this item.
    failed_sheet: Option<u32>,
    /// The picture last cut, until the main thread takes it.
    ready: Option<Tile>,
    /// The index of the last picture cut, so a want for the same picture does no work.
    served: Option<u32>,
}

pub struct Trickplays<S: Source> {
    source: S,
    state: Mutex<State>,
}

impl<S: Source> Trickplays<S> {
    pub fn new(source: S) -> Arc<Self> {
        Arc::new(Self { source, state: Mutex::new(State::default()) })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The viewer is scrubbing part `part` of `rk` on `server` at `pos_ms`. Cheap and non-blocking:
    /// it records the want and starts the worker if none is running. MAIN THREAD.
    pub fn want(self: &Arc<Self>, server: ServerId, rk: &str, part: i64, pos_ms: i64) {
        if rk.is_empty() || pos_ms < 0 {
            return;
        }
        {
            let mut st = self.state();
            if st.item.as_ref().is_none_or(|i| i.server != server || i.rk != rk || i.part != part) {
                *st = State {
                    item: Some(Item { server, rk: rk.to_string(), part }),
                    generation: st.generation.wrapping_add(1),
                    running: st.running,
                    ..State::default()
                };
            }
            if matches!(st.info, Some(None)) {
                return;
            }
            if let Some(Some(t)) = st.info {
                if t.index_at(pos_ms).is_some_and(|i| st.served == Some(i)) {
                    st.want_ms = None;
                    return;
                }
            }
            st.want_ms = Some(pos_ms);
            if st.running {
                return;
            }
            st.running = true;
        }
        let me = self.clone();
        if plx_base::task::spawn("trickplay", move || me.serve()).is_none() {
            let mut st = self.state();
            st.running = false;
            st.want_ms = None;
        }
    }

    /// The picture cut since the last call, if any. MAIN THREAD.
    pub fn take_ready(&self) -> Option<Tile> {
        self.state().ready.take()
    }

    /// Scrubbing ended: drop the decoded sheet and any picture not yet taken. The item's layout and
    /// the last sheet's JPEG are kept, so the next scrub of the same item near the same place asks
    /// the server nothing. MAIN THREAD.
    pub fn release(&self) {
        let mut st = self.state();
        st.generation = st.generation.wrapping_add(1);
        st.want_ms = None;
        st.sheet = None;
        st.ready = None;
        st.served = None;
    }

    /// The playback ended: forget the item too. MAIN THREAD.
    pub fn forget(&self) {
        let mut st = self.state();
        let running = st.running;
        *st = State { generation: st.generation.wrapping_add(1), running, ..State::default() };
    }

    /// Is a decoded sheet resident? For the release's tests.
    #[cfg(test)]
    fn holds_sheet(&self) -> bool {
        self.state().sheet.is_some()
    }

    /// The worker: serve the latest want until none is left.
    fn serve(&self) {
        loop {
            let (generation, item, pos_ms, info) = {
                let mut st = self.state();
                let (Some(pos_ms), Some(item)) = (st.want_ms.take(), st.item.clone()) else {
                    st.running = false;
                    return;
                };
                (st.generation, item, pos_ms, st.info)
            };
            let info = match info {
                Some(info) => info,
                None => {
                    let info = self.source.info(item.server, &item.rk, item.part);
                    let mut st = self.state();
                    if st.item.as_ref() != Some(&item) {
                        continue;
                    }
                    st.info = Some(info);
                    if info.is_none() {
                        plx_base::eventlog::log("trickplay: the item has no scrubbing thumbnails");
                    }
                    info
                }
            };
            let Some(t) = info else { continue };
            let Some(index) = t.index_at(pos_ms) else { continue };
            let (number, col, row) = t.place(index);
            let (resident, kept) = {
                let st = self.state();
                if st.failed_sheet == Some(number) && st.item.as_ref() == Some(&item) {
                    continue;
                }
                let kept = st.jpeg.as_ref().filter(|(n, _)| *n == number).map(|(_, b)| b.clone());
                (st.sheet.as_ref().is_some_and(|s| s.number == number), kept)
            };
            if !resident {
                let bytes = kept.or_else(|| {
                    let fetched = self.source.sheet(item.server, &item.rk, item.part, &t, number).map(Arc::new);
                    // Kept even when a release came meanwhile: the next scrub of this item decodes
                    // it without asking again.
                    let mut st = self.state();
                    if let Some(b) = fetched.as_ref().filter(|_| st.item.as_ref() == Some(&item)) {
                        st.jpeg = Some((number, b.clone()));
                    }
                    fetched
                });
                let decoded = bytes.and_then(|b| plx_gfx::img::img_decode_rgb_owned(&b));
                let mut st = self.state();
                if st.generation != generation {
                    continue;
                }
                match decoded {
                    Some((width, height, rgb)) => st.sheet = Some(Sheet { number, width, height, rgb }),
                    None => {
                        plx_base::eventlog::log(&format!("trickplay: sheet {number} could not be fetched or decoded"));
                        st.failed_sheet = Some(number);
                        continue;
                    }
                }
            }
            let mut st = self.state();
            if st.generation != generation {
                continue;
            }
            let Some(sheet) = st.sheet.as_ref().filter(|s| s.number == number) else { continue };
            let Some(rgba) = cut(sheet, &t, col, row) else {
                plx_base::eventlog::log(&format!("trickplay: picture {index} is outside its {}x{} sheet", sheet.width, sheet.height));
                continue;
            };
            st.ready = Some(Tile { index, width: t.width, height: t.height, rgba });
            st.served = Some(index);
            drop(st);
            plx_machine::present::wake_from_worker();
        }
    }
}

/// Picture (`col`, `row`) of `sheet` as RGBA, or `None` when the sheet is too small to hold it (a
/// layout that does not match the image the server sent).
fn cut(sheet: &Sheet, t: &Trickplay, col: u32, row: u32) -> Option<Vec<u8>> {
    let (w, h) = (t.width as usize, t.height as usize);
    let (x0, y0) = (col as usize * w, row as usize * h);
    let (sw, sh) = (sheet.width as usize, sheet.height as usize);
    if x0 + w > sw || y0 + h > sh || sheet.rgb.len() < sw * sh * 3 {
        return None;
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for y in y0..y0 + h {
        let line = &sheet.rgb[(y * sw + x0) * 3..(y * sw + x0 + w) * 3];
        for px in line.chunks_exact(3) {
            out.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// How many 5 ms waits a test gives the worker (10 s): a count, since `appkit` reads no clock.
    const WAITS: u32 = 2_000;

    const LAYOUT: Trickplay = Trickplay { width: 4, height: 2, tile_cols: 3, tile_rows: 2, count: 10, interval_ms: 1000 };

    /// Sheets whose every picture is one flat colour: red = the sheet, green = the picture's
    /// index on it. Counts what it was asked.
    #[derive(Default)]
    struct Fake {
        none: bool,
        infos: AtomicUsize,
        sheets: AtomicUsize,
    }

    impl Source for Fake {
        fn info(&self, _: ServerId, _: &str, _: i64) -> Option<Trickplay> {
            self.infos.fetch_add(1, Ordering::SeqCst);
            (!self.none).then_some(LAYOUT)
        }
        fn sheet(&self, _: ServerId, _: &str, _: i64, t: &Trickplay, n: u32) -> Option<Vec<u8>> {
            self.sheets.fetch_add(1, Ordering::SeqCst);
            let (w, h) = (t.width * t.tile_cols, t.height * t.tile_rows);
            let mut rgba = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    let at = (y / t.height) * t.tile_cols + x / t.width;
                    rgba.extend_from_slice(&[n as u8 * 40, at as u8 * 20, 7, 255]);
                }
            }
            plx_gfx::img::img_encode_png(w, h, &rgba)
        }
    }

    fn server() -> ServerId {
        plx_plex::plex::ServerId::from_raw(1)
    }

    fn wait_ready<S: Source>(tp: &Trickplays<S>) -> Tile {
        for _ in 0..WAITS {
            if let Some(t) = tp.take_ready() {
                return t;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("no picture was cut");
    }

    fn idle<S: Source>(tp: &Trickplays<S>) {
        for _ in 0..WAITS {
            if !tp.state().running {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the worker never finished");
    }

    #[test]
    fn a_scrub_position_is_cut_out_of_its_sheet() {
        let tp = Trickplays::new(Fake::default());
        // 7.5 s in is picture 7: sheet 1 (six a sheet), its second picture.
        tp.want(server(), "m1", 1, 7_500);
        let tile = wait_ready(&tp);
        assert_eq!((tile.index, tile.width, tile.height), (7, 4, 2));
        assert_eq!(tile.rgba.len(), 4 * 2 * 4);
        assert!(tile.rgba.chunks_exact(4).all(|p| p == [40, 20, 7, 255]), "{:?}", &tile.rgba[..8]);

        // A neighbour on the same sheet is cut from the resident sheet, not fetched again.
        idle(&tp);
        tp.want(server(), "m1", 1, 9_000);
        assert_eq!(wait_ready(&tp).rgba[..4], [40, 60, 7, 255]);
        idle(&tp);
        assert_eq!((tp.source.infos.load(Ordering::SeqCst), tp.source.sheets.load(Ordering::SeqCst)), (1, 1));

        // The same picture again is no work at all.
        tp.want(server(), "m1", 1, 9_400);
        idle(&tp);
        assert!(tp.take_ready().is_none());
    }

    #[test]
    fn the_decoded_sheet_is_dropped_when_scrubbing_ends_and_its_jpeg_and_the_layout_are_kept() {
        let tp = Trickplays::new(Fake::default());
        tp.want(server(), "m1", 1, 0);
        wait_ready(&tp);
        idle(&tp);
        assert!(tp.holds_sheet());
        tp.release();
        assert!(!tp.holds_sheet());
        tp.want(server(), "m1", 1, 0);
        wait_ready(&tp);
        idle(&tp);
        assert_eq!((tp.source.infos.load(Ordering::SeqCst), tp.source.sheets.load(Ordering::SeqCst)), (1, 1),
            "the next scrub of the same item decodes the kept JPEG");
        tp.forget();
        tp.want(server(), "m1", 1, 0);
        wait_ready(&tp);
        idle(&tp);
        assert_eq!(tp.source.sheets.load(Ordering::SeqCst), 2, "a forgotten item asks again");
    }

    #[test]
    fn an_item_without_thumbnails_is_asked_once() {
        let tp = Trickplays::new(Fake { none: true, ..Fake::default() });
        tp.want(server(), "m1", 1, 0);
        idle(&tp);
        tp.want(server(), "m1", 1, 5_000);
        idle(&tp);
        assert!(tp.take_ready().is_none());
        assert_eq!(tp.source.infos.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_picture_outside_a_short_sheet_is_not_cut() {
        let sheet = Sheet { number: 0, width: 12, height: 2, rgb: vec![0; 12 * 2 * 3] };
        assert!(cut(&sheet, &LAYOUT, 2, 0).is_some());
        assert!(cut(&sheet, &LAYOUT, 0, 1).is_none(), "the sheet has one row of pictures");
    }
}
