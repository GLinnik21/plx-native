//! Server-scoped key registry and tombstones for an owned Library instance.

use crate::screens::registry::{
    LibraryIdentity, LibraryKey, LibraryMemory, LibrarySectionIdentity,
};
use crate::ui::machine::GroupId;

const LIBRARY_BASE: u32 = 0x2100_0000;
const SHELF_BASE: u32 = 0x2200_0000;
const GRID_BASE: u32 = 0x2300_0000;
const RAIL_BASE: u32 = 0x2400_0000;
const CONTROL_BASE: u32 = 0x2500_0000;
const REGION_MASK: u32 = 0xff00_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyRegion {
    Library,
    Shelf,
    Grid,
    Rail,
    Control,
}

pub(super) fn region_of_elem(elem: u32) -> Option<KeyRegion> {
    Some(match elem & REGION_MASK {
        LIBRARY_BASE => KeyRegion::Library,
        SHELF_BASE => KeyRegion::Shelf,
        GRID_BASE => KeyRegion::Grid,
        RAIL_BASE => KeyRegion::Rail,
        CONTROL_BASE => KeyRegion::Control,
        _ => return None,
    })
}

#[derive(Clone, Debug)]
pub(super) struct KeyRegistry {
    keys: Vec<LibraryKey>,
    next: [u32; 5],
}

impl Default for KeyRegistry {
    fn default() -> Self {
        Self { keys: Vec::new(), next: [0; 5] }
    }
}

impl KeyRegistry {
    pub(super) fn restore(memory: &LibraryMemory) -> Self {
        let mut out = Self { keys: memory.keys.clone(), next: [0; 5] };
        for key in &out.keys {
            if let Some(region) = region_of_elem(key.elem) {
                let slot = region_index(region);
                out.next[slot] = out.next[slot].max((key.elem & !REGION_MASK) + 1);
            }
        }
        out
    }

    pub(super) fn remember(
        &self,
        section: Option<LibrarySectionIdentity>,
        scroll: f32,
        shelf_scroll: Vec<(String, f32)>,
    ) -> LibraryMemory {
        LibraryMemory {
            keys: self.keys.clone(),
            next_elem: self.keys.len() as u32,
            section,
            scroll,
            shelf_scroll,
        }
    }

    pub(super) fn register(
        &mut self,
        identity: LibraryIdentity,
        group: GroupId,
        index: usize,
    ) -> u32 {
        if let Some(key) = self.keys.iter_mut().find(|key| key.identity == identity) {
            key.last_group = group.0;
            key.last_index = index as u32;
            return key.elem;
        }
        let region = identity_region(&identity);
        let slot = region_index(region);
        let ordinal = self.next[slot];
        self.next[slot] = ordinal.checked_add(1).expect("Library element-key space exhausted");
        let elem = region_base(region) | ordinal;
        self.keys.push(LibraryKey {
            identity,
            elem,
            last_group: group.0,
            last_index: index as u32,
        });
        elem
    }

    pub(super) fn key(&self, elem: u32) -> Option<&LibraryKey> {
        self.keys.iter().find(|key| key.elem == elem)
    }

    pub(super) fn region(&self, elem: u32) -> Option<KeyRegion> {
        let typed = match &self.key(elem)?.identity {
            LibraryIdentity::Library(_) => KeyRegion::Library,
            LibraryIdentity::Shelf { .. } | LibraryIdentity::ShelfSlot { .. } => KeyRegion::Shelf,
            LibraryIdentity::Grid { .. } | LibraryIdentity::GridSlot { .. } => KeyRegion::Grid,
            LibraryIdentity::Rail { .. } => KeyRegion::Rail,
            LibraryIdentity::Control { .. } => KeyRegion::Control,
        };
        debug_assert_eq!(region_of_elem(elem), Some(typed));
        Some(typed)
    }

    pub(super) fn last_place(&self, elem: u32) -> Option<(GroupId, usize)> {
        let key = self.key(elem)?;
        Some((GroupId(key.last_group), key.last_index as usize))
    }

    pub(super) fn keys(&self) -> &[LibraryKey] { &self.keys }
}

fn identity_region(identity: &LibraryIdentity) -> KeyRegion {
    match identity {
        LibraryIdentity::Library(_) => KeyRegion::Library,
        LibraryIdentity::Shelf { .. } | LibraryIdentity::ShelfSlot { .. } => KeyRegion::Shelf,
        LibraryIdentity::Grid { .. } | LibraryIdentity::GridSlot { .. } => KeyRegion::Grid,
        LibraryIdentity::Rail { .. } => KeyRegion::Rail,
        LibraryIdentity::Control { .. } => KeyRegion::Control,
    }
}

const fn region_index(region: KeyRegion) -> usize {
    match region {
        KeyRegion::Library => 0,
        KeyRegion::Shelf => 1,
        KeyRegion::Grid => 2,
        KeyRegion::Rail => 3,
        KeyRegion::Control => 4,
    }
}

const fn region_base(region: KeyRegion) -> u32 {
    match region {
        KeyRegion::Library => LIBRARY_BASE,
        KeyRegion::Shelf => SHELF_BASE,
        KeyRegion::Grid => GRID_BASE,
        KeyRegion::Rail => RAIL_BASE,
        KeyRegion::Control => CONTROL_BASE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(sid: u16) -> LibrarySectionIdentity {
        LibrarySectionIdentity { sid: crate::plex::ServerId::from_raw(sid), key: 1 }
    }

    fn grid(sid: u16, rk: &str) -> LibraryIdentity {
        LibraryIdentity::Grid {
            section: section(sid),
            sid: crate::plex::ServerId::from_raw(sid),
            rk: rk.into(),
        }
    }

    #[test]
    fn same_rating_key_on_two_servers_has_two_elements() {
        let mut keys = KeyRegistry::default();
        let a = keys.register(grid(1, "42"), GroupId(9), 0);
        let b = keys.register(grid(2, "42"), GroupId(9), 0);
        assert_ne!(a, b);
        assert_eq!(keys.region(a), Some(KeyRegion::Grid));
        assert_eq!(keys.region(b), Some(KeyRegion::Grid));
    }

    #[test]
    fn item_key_survives_reorder_and_updates_only_recovery_slot() {
        let mut keys = KeyRegistry::default();
        let id = grid(1, "42");
        let first = keys.register(id.clone(), GroupId(9), 2);
        let moved = keys.register(id, GroupId(9), 17);
        assert_eq!(first, moved);
        assert_eq!(keys.last_place(first), Some((GroupId(9), 17)));
    }

    #[test]
    fn disappeared_item_remains_typed_as_grid_tombstone() {
        let mut keys = KeyRegistry::default();
        let elem = keys.register(grid(1, "42"), GroupId(9), 5);
        let memory = keys.remember(Some(section(1)), 400.0, Vec::new());
        let restored = KeyRegistry::restore(&memory);
        assert_eq!(restored.region(elem), Some(KeyRegion::Grid));
        assert_eq!(restored.last_place(elem), Some((GroupId(9), 5)));
    }

    #[test]
    fn slot_promotion_never_reuses_the_slot_identity() {
        let mut keys = KeyRegistry::default();
        let slot = keys.register(
            LibraryIdentity::GridSlot { section: section(1), query: 3, slot: 5 },
            GroupId(9),
            5,
        );
        let item = keys.register(grid(1, "42"), GroupId(9), 5);
        assert_ne!(slot, item);
        assert_eq!(keys.region(slot), Some(KeyRegion::Grid));
        assert_eq!(keys.region(item), Some(KeyRegion::Grid));
    }
}
