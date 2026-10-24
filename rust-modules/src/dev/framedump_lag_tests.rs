//! The landing lag against the driver's own hold rule (`framedump::Sample::with_draw`, `decide`):
//! what a dump of a slow page writes, run over a real paced gate and a real `Working` read-out,
//! because `test_support::frame` runs a frame without the hold rule and so cannot see a dump that
//! never finishes. A separate file: its worker thread is test code the `threads` gate must see as such.

use super::*;

fn frame_of(entries: &[(Reason, &str)], count: u32) -> Frame {
    Frame {
        count,
        entries: entries.iter().map(|&(reason, key)| Entry { reason, key: key.into() }).collect(),
        absent: Vec::new(),
    }
}

#[test]
fn a_loading_state_is_debt_unless_the_lag_is_holding_a_store_out() {
    let lagged = ["collection".to_string()];
    let page = frame_of(&[(Reason::WorkingReadout, "Loading"), (Reason::DetailSpinner, "")], 2);
    assert_eq!(owed_placeholders(&page, &[]), 2, "no lag armed: every placeholder is debt");
    assert_eq!(owed_placeholders(&page, &lagged), 0, "a lagged store's loading state is the picture");
    let art = frame_of(&[(Reason::WorkingReadout, "Loading"), (Reason::CardSkeleton, "/photo/1"),
        (Reason::HeroLogoText, "")], 3);
    assert_eq!(owed_placeholders(&art, &lagged), 2,
        "a card's art and a hero's logo wait on the poster source, lag or not");
    let unknown = frame_of(&[(Reason::CardGround, ""), (Reason::CardSkeleton, ""), (Reason::CardGround, "/t/1")], 3);
    assert_eq!(owed_placeholders(&unknown, &lagged), 1,
        "a tile whose key is not known yet waits on the store; one with a key waits on the poster source");
    assert_eq!(owed_placeholders(&unknown, &[]), 3, "and with no lag it is debt like any other");
    let capped = frame_of(&[(Reason::WorkingReadout, "Loading")], 9);
    assert_eq!(owed_placeholders(&capped, &lagged), 8, "a draw whose reason was not kept is never excused");
}

/// The driver's iteration over a real paced gate with a lag, a real `Working` read-out drawn
/// while the page's answer is out, and the driver's own sample and `decide` (`Sample::with_draw`),
/// minus the GL read-back and the app around it. A held iteration repeats at the same virtual
/// frame; the loop is bounded by a count, never a clock. Returns, per WRITTEN frame, the placeholder
/// count its draw showed.
fn film_a_slow_page(lag: u64, frames: usize) -> Vec<u32> {
    use std::sync::atomic::{AtomicBool, Ordering as O};
    use std::sync::{Arc, Mutex};
    let store = plx_data::stores::StoreId::Collection;
    let gate = plx_machine::landgate::Gate::default();
    gate.arm_dump_paced(lag);
    let claim = Arc::new(AtomicBool::new(true)); // the page's request is out from the start
    let mailbox: Arc<Mutex<Option<u32>>> = Arc::default();
    // The worker answers after a real delay that is long next to an iteration and short next to a test.
    let m = Arc::clone(&mailbox);
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(15));
        *m.lock().unwrap() = Some(7); // the claim is released by the take, never by the worker finishing
    });
    placeholder::arm();
    let (mut v, mut landed, mut written, mut why) = (0u64, false, Vec::new(), Vec::new());
    for _ in 0..2_000_000u32 {
        gate.dump_frame(v);
        let answer = gate.take_owed(store.ord(), store.name(), || claim.load(O::SeqCst),
            || mailbox.lock().unwrap().take());
        if answer.is_some() {
            claim.store(false, O::SeqCst);
            landed = true;
        }
        let lagged = gate.take_lagged();
        let pre = Sample { starved: gate.take_starved(), ..Sample::default() };
        placeholder::reset();
        if !landed {
            // The host has no GL: `declared` runs the real draw as a discovery walk, in which every
            // primitive declares itself and the placeholder counter sees what a frame would.
            placeholder::declared(|| {
                plx_ui::widgets::StatusOverlay::new(plx_ui::Rect::FULL, c"Loading", plx_ui::widgets::StatusKind::Working)
                    .draw_measured(&plx_ui::Env::inert(), plx_ui::Painter::root(), &plx_ui::fixture::FixtureMeasure);
            });
        }
        let frame = placeholder::take();
        let after = Sample { starved: gate.take_starved(), ..Sample::default() };
        let post = pre.with_draw(after, &frame, &lagged);
        match decide(&post) {
            Action::Write => {
                written.push(frame.count);
                v += 1;
                if written.len() == frames {
                    placeholder::disarm();
                    worker.join().unwrap();
                    return written;
                }
            }
            Action::Hold => {
                why = hold_reasons(&post);
                if !post.starved.is_empty() {
                    std::thread::sleep(Duration::from_micros(PACE_SLEEP_US));
                }
            }
        }
    }
    panic!("the dump never finished: {} frames written, stuck at virtual frame {v} on {why:?}", written.len());
}

#[test]
fn a_lagged_loading_page_writes_its_loading_frames_then_its_landed_ones_and_terminates() {
    let _guard = plx_base::testlock::serial();
    let written = film_a_slow_page(12, 20);
    assert_eq!(written.len(), 20);
    assert!(written[..12].iter().all(|&n| n > 0), "the 12 lagged frames show the loading state: {written:?}");
    assert!(written[12..].iter().all(|&n| n == 0), "the frames after the landing show the page: {written:?}");
}

#[test]
fn without_a_lag_the_page_never_writes_a_loading_frame() {
    let _guard = plx_base::testlock::serial();
    let written = film_a_slow_page(0, 8);
    assert!(written.iter().all(|&n| n == 0), "the loop held for the answer instead: {written:?}");
}
