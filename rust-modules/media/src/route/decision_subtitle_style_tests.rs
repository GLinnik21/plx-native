//! The Size / Position persistence path: publish the live atomic first, persist second, and never
//! republish what the worker learned.
use super::*;
use plx_plex::plex::session::{SubtitlePosition, SubtitleSize};
use std::sync::mpsc;

/// Restore the two atomics the tests move, whatever the outcome.
struct Restore(SubtitleSize, SubtitlePosition);
impl Drop for Restore {
    fn drop(&mut self) {
        restore_subtitle_size(self.0);
        restore_subtitle_position(self.1);
    }
}

fn recv(rx: &mpsc::Receiver<bool>) -> bool {
    rx.recv_timeout(std::time::Duration::from_secs(10)).expect("the persist-only write answered")
}

/// **A pick is live at once and durable later.** The atomic the caption draw reads is the new
/// value the moment `select_*` returns — before the worker has done anything — and the reply then
/// reports the durable write, which the next load reads back.
#[test]
fn select_publishes_the_live_value_before_the_write_and_persists_it() {
    let _g = plx_base::testlock::serial();
    let session = plx_plex::plex::session::TempSession::new("select-style");
    let _restore = Restore(subtitle_size(), subtitle_position());
    restore_subtitle_size(SubtitleSize::Medium);
    restore_subtitle_position(SubtitlePosition::Low);

    let (tx, rx) = mpsc::channel();
    select_subtitle_size(SubtitleSize::Large, Some(tx));
    assert_eq!(subtitle_size(), SubtitleSize::Large, "published synchronously, ahead of the worker");
    assert!(recv(&rx), "a writable session is durable");
    assert_eq!(plx_plex::plex::session::load().subtitle_size, SubtitleSize::Large);

    let (tx, rx) = mpsc::channel();
    select_subtitle_position(SubtitlePosition::High, Some(tx));
    assert_eq!(subtitle_position(), SubtitlePosition::High);
    assert!(recv(&rx));
    assert_eq!(plx_plex::plex::session::load().subtitle_position, SubtitlePosition::High);
    drop(session);
}

/// **The write never republishes.** A newer live pick made while an older write is in flight must
/// survive the older write's completion: the worker touches the durable file only.
#[test]
fn select_never_republishes_when_the_write_completes() {
    let _g = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("select-no-republish");
    let _restore = Restore(subtitle_size(), subtitle_position());
    restore_subtitle_size(SubtitleSize::Medium);

    let (tx, rx) = mpsc::channel();
    select_subtitle_size(SubtitleSize::Large, Some(tx));
    restore_subtitle_size(SubtitleSize::Small); // a newer pick, made before the worker answers
    assert!(recv(&rx));
    assert_eq!(subtitle_size(), SubtitleSize::Small, "the completed write left the live value alone");
}

/// **A failed write claims nothing**: the reply says `false` and the live value the viewer picked
/// stays for this session (it is the durable write that failed, not the pick).
#[test]
fn select_reports_a_failed_write_and_keeps_the_live_value() {
    let _g = plx_base::testlock::serial();
    let session = plx_plex::plex::session::TempSession::new("select-failed");
    let _restore = Restore(subtitle_size(), subtitle_position());
    restore_subtitle_size(SubtitleSize::Medium);
    // the session file's parent is a regular file, so no write can land
    let dir = std::env::temp_dir().join(format!("plxnative-select-blocked-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("blocker"), b"x").unwrap();
    plx_plex::plex::session::redirect_for_test(Some(dir.join("blocker").join("auth.json")));

    let (tx, rx) = mpsc::channel();
    select_subtitle_size(SubtitleSize::ExtraLarge, Some(tx));
    assert!(!recv(&rx), "an unwritable session is not durable");
    assert_eq!(subtitle_size(), SubtitleSize::ExtraLarge, "the live pick stands");
    plx_plex::plex::session::redirect_for_test(None);
    let _ = std::fs::remove_dir_all(&dir);
    drop(session);
}
