//! player::threads — the media-side Load receipt and worker seam. The physical Load worker
//! (`plx_play_feed::native_load::load_thread`) takes the sink it Loads through from the caller:
//! this wrapper hands it the player's fenced [`super::sink`], so `engine`'s workers and
//! `LaunchHooks.load_thread` keep one signature. The demux thread body is `ff::demux` (libavformat
//! over a custom AVIO on stream.rs); its HttpStream box lives in the Engine (main owns it, closes it
//! to interrupt, and outlives the threads).
use plx_play_feed::pcm_lead::PcmLead;
pub use plx_play_feed::SendPtr;
use std::os::raw::c_char;

pub(super) use plx_play_feed::feed_resources::LoadReceipt;

pub(super) fn load_thread(
    payload: SendPtr<c_char>,
    native_epoch: u32,
    receipt: Option<LoadReceipt>,
    pcm_lead: PcmLead,
) {
    plx_play_feed::native_load::load_thread(super::sink, payload, native_epoch, receipt, pcm_lead);
}
