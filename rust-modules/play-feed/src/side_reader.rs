//! One feed attempt's side-reader IO seam. The common runner owns the reader's concrete source
//! and lifecycle; the physical feed preserves its start and stop order around worker joins.

pub trait SideReaderPort: Send {
    /// The demuxer must leave embedded subtitles to this reader from its first instruction.
    fn embedded_owner(&self) -> bool;
    /// Start only after both demux and native Load workers have spawned.
    fn start_after_load_spawn(&mut self);
    /// Stop and join before the paired demux worker is joined, including a failed Load spawn.
    fn stop_before_demux_join(&mut self, reason: &str);
}
