//! `TexCache<K>` — the library's RENDER-RESOURCE half of image caching (spec §10). It owns GL
//! texture residency and the LRU, `resolve`/`resolve_wh`, `warm`, the upload step under
//! `Budget`'s `Poster` class, and one `Provenance::Resource` invalidate when a texture becomes
//! resident. It never knows what a key denotes: the application's source half
//! (`app/adapters/poster.rs`, phase 3a — interning, the transcode URL, fetch + decode workers,
//! the disk tier) delivers a decoded image as `PosterReady` and the seam is the KEY.
//!
//! **The result handler only ACCEPTS** (`accept`: owned pixels into the pending queue, no GL).
//! **Upload happens in PREPARE** (`prepare`: §3.3 step 9, after `glViewport`, inside the
//! presented frame's GL scope, `Budget::take(Poster)` per upload, `warm` on each). A frame that
//! does not present uploads nothing and the queue waits. The pixels are an owned render resource
//! handed over once: they never enter logical state, and the recorder writes only `(key, ok)`.
#![allow(dead_code)] // phase 2-i: no consumer until phase 3a (spec §13)

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;

use super::frame::{Budget, Class};
use super::machine::PresentHandle;
use super::present::{PresentEvent, Provenance, ResourceKind};

/// A decoded image: an OWNED render resource.
pub struct Decoded {
    pub w: u16,
    pub h: u16,
    pub rgba: Box<[u8]>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PosterError {
    Fetch,
    Decode,
    Refused,
}

/// The one message the app's poster adapter delivers to `MachineId::Cache`.
pub struct PosterReady<K> {
    pub key: K,
    pub result: Result<Decoded, PosterError>,
}

/// A resident texture as the renderer sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Tex {
    pub id: u32,
    pub w: u16,
    pub h: u16,
}

/// The GL half behind the cache: the real one wraps `gfx::tex_upload`/`warm_tex`; tests stub it.
pub trait Uploader {
    fn upload(&mut self, d: &Decoded) -> Tex;
    /// `gfx::warm_tex`: touch the texture inside the presented frame's GL scope (residency).
    fn warm(&mut self, t: Tex);
    fn free(&mut self, t: Tex);
}

struct Entry {
    tex: Tex,
    last_used: u64,
}

pub struct TexCache<K> {
    resident: HashMap<K, Entry>,
    pending: VecDeque<(K, Decoded)>,
    failed: HashSet<K>,
    cap: usize,
    clock: u64,
    bytes: usize,
}

impl<K: Copy + Eq + Hash> TexCache<K> {
    pub fn new(cap: usize) -> Self {
        Self {
            resident: HashMap::new(),
            pending: VecDeque::new(),
            failed: HashSet::new(),
            cap,
            clock: 0,
            bytes: 0,
        }
    }

    /// The result handler: moves the pixels into the pending queue and touches no GL.
    pub fn accept(&mut self, r: PosterReady<K>) {
        match r.result {
            Ok(d) => {
                self.failed.remove(&r.key);
                self.pending.push_back((r.key, d));
            }
            Err(_) => {
                self.failed.insert(r.key);
            }
        }
    }

    /// Whether `prepare` has work — `Budget::note_queued` reads it at the top of the frame.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The upload step (§3.3 step 9). `now_us` is read by the caller before each take; the
    /// spike takes one reading per call, phase 11's `Budget` reads the clock inside.
    /// Returns how many textures became resident.
    pub fn prepare(
        &mut self,
        b: &mut Budget,
        up: &mut dyn Uploader,
        present: &mut PresentHandle<'_>,
        now_us: impl Fn() -> u64,
    ) -> usize {
        let mut n = 0;
        while let Some((key, _)) = self.pending.front() {
            let key = *key;
            if !b.take(Class::Poster, now_us()) {
                break;
            }
            let (_, d) = self.pending.pop_front().expect("front() was Some");
            self.evict_for(1, up);
            let tex = up.upload(&d);
            up.warm(tex);
            self.clock += 1;
            self.bytes += d.rgba.len();
            if let Some(old) = self.resident.insert(
                key,
                Entry {
                    tex,
                    last_used: self.clock,
                },
            ) {
                up.free(old.tex);
            }
            n += 1;
        }
        if n > 0 {
            present.note(PresentEvent::Damage(Provenance::Resource(ResourceKind::Texture)));
        }
        n
    }

    fn evict_for(&mut self, room: usize, up: &mut dyn Uploader) {
        while self.resident.len() + room > self.cap {
            let Some((&k, _)) = self.resident.iter().min_by_key(|(_, e)| e.last_used) else {
                break;
            };
            if let Some(e) = self.resident.remove(&k) {
                up.free(e.tex);
            }
        }
    }

    /// The renderer's question. A hit is a use (LRU); a miss is the absent-resource rule's cue
    /// (§8.2): draw the placeholder at the final geometry and request once.
    pub fn resolve(&mut self, k: K) -> Option<Tex> {
        self.clock += 1;
        let clock = self.clock;
        self.resident.get_mut(&k).map(|e| {
            e.last_used = clock;
            e.tex
        })
    }

    pub fn resolve_wh(&mut self, k: K) -> Option<(u16, u16)> {
        self.resolve(k).map(|t| (t.w, t.h))
    }

    pub fn is_failed(&self, k: K) -> bool {
        self.failed.contains(&k)
    }

    pub fn resident_count(&self) -> usize {
        self.resident.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::present::Present;

    struct StubUp {
        next: u32,
        freed: Vec<u32>,
        warmed: Vec<u32>,
    }
    impl Uploader for StubUp {
        fn upload(&mut self, d: &Decoded) -> Tex {
            self.next += 1;
            Tex {
                id: self.next,
                w: d.w,
                h: d.h,
            }
        }
        fn warm(&mut self, t: Tex) {
            self.warmed.push(t.id);
        }
        fn free(&mut self, t: Tex) {
            self.freed.push(t.id);
        }
    }

    fn ready(k: u32) -> PosterReady<u32> {
        PosterReady {
            key: k,
            result: Ok(Decoded {
                w: 2,
                h: 2,
                rgba: vec![0; 16].into_boxed_slice(),
            }),
        }
    }

    #[test]
    fn accept_touches_no_gl_and_prepare_uploads_under_the_poster_quota() {
        let mut c: TexCache<u32> = TexCache::new(2);
        let mut up = StubUp {
            next: 0,
            freed: vec![],
            warmed: vec![],
        };
        for k in 1..=4 {
            c.accept(ready(k));
        }
        assert_eq!(up.next, 0, "accept uploads nothing");
        assert!(c.has_pending());
        let mut b = Budget::new();
        b.begin_frame(0);
        let mut present = Present::new();
        let _ = present.take(0);
        let mut ph = PresentHandle(&mut present);
        let n = c.prepare(&mut b, &mut up, &mut ph, || 0);
        assert_eq!(n, 3, "the quota is three per frame");
        assert!(c.has_pending(), "the fourth waits");
        assert_eq!(c.resident_count(), 2, "cap 2: the oldest was evicted");
        assert_eq!(up.freed, vec![1]);
        assert_eq!(up.warmed, vec![1, 2, 3]);
        assert!(present.take(16), "a resident texture is one damage");
        assert!(c.resolve(3).is_some() && c.resolve(1).is_none());
    }
}
