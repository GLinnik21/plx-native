//! Subtitle clock evidence shared by a media reader and presentation owner. These values
//! carry no demuxer handle, stream index, URL, or player registry.

/// How far ahead of the remux playhead a side reader may be before it parks.
pub const WINDOW_NS: i64 = 12_000_000_000;

/// Bytes from each end of a keyframe used to identify it across a remux.
const FINGERPRINT: usize = 32;

/// The first video keyframe in a remux: stream time and enough bytes to find the same packet
/// in the original Part. Size alone is insufficient because two keyframes may share a size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub pts_ns: i64,
    pub size: usize,
    pub head: [u8; FINGERPRINT],
    pub tail: [u8; FINGERPRINT],
}
impl Anchor {
    pub fn of(pts_ns: i64, data: &[u8]) -> Self {
        let (head, tail) = fingerprint(data);
        Self { pts_ns, size: data.len(), head, tail }
    }

    /// Size, head and tail must all agree. A short packet is zero-padded consistently.
    pub fn matches(&self, data: &[u8]) -> bool {
        let (head, tail) = fingerprint(data);
        data.len() == self.size && head == self.head && tail == self.tail
    }
}

fn fingerprint(data: &[u8]) -> ([u8; FINGERPRINT], [u8; FINGERPRINT]) {
    let mut head = [0u8; FINGERPRINT];
    let mut tail = [0u8; FINGERPRINT];
    let h = data.len().min(FINGERPRINT);
    head[..h].copy_from_slice(&data[..h]);
    tail[..h].copy_from_slice(&data[data.len() - h..]);
    (head, tail)
}

/// Remux playhead of a packet: fed PTS minus the feed shift plus display base.
pub fn playhead_of(anchor_pts_ns: i64, pts_shift: i64, disp_base: i64) -> i64 {
    anchor_pts_ns.saturating_sub(pts_shift).saturating_add(disp_base)
}

/// Part time minus remux playhead from a byte-exact keyframe match.
pub fn delta_exact(part_kf_pts: i64, anchor_pts_ns: i64, pts_shift: i64, disp_base: i64) -> i64 {
    part_kf_pts.saturating_sub(playhead_of(anchor_pts_ns, pts_shift, disp_base))
}

/// Fallback for embedded cues when no keyframe matched. Sidecar draw must use only exact delta.
pub fn delta_assumed(k_le: i64, anchor_pts_ns: i64, pts_shift: i64, disp_base: i64) -> i64 {
    delta_exact(k_le, anchor_pts_ns, pts_shift, disp_base)
}

/// Whether a Part-time packet is beyond the side reader's lead window.
pub fn pacing_parks(pkt_part_ns: i64, delta_ns: i64, playpos_ns: i64) -> bool {
    pkt_part_ns.saturating_sub(delta_ns) > playpos_ns.saturating_add(WINDOW_NS)
}

/// Subtitle clock used to judge a cue while a seek's first displayed frame is pending.
pub fn judged_clock(playpos_ns: i64, anchor_playhead_ns: i64, offset_ns: i64) -> i64 {
    let now = playpos_ns.max(anchor_playhead_ns);
    now.min(now.saturating_sub(offset_ns))
}
