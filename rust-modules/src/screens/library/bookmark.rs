//! Tier-three application snapshots. Live focus and group memory remain in FocusEngine.
use super::*;

impl LibraryScreen {
    /// Save only at leave/section-switch boundaries, through the addressed store drain.
    pub(super) fn save_cursor<H: LibraryLike>(&self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let listing = H::listing(cx);
        let Some(id) = listing.id() else { return };
        if self.epoch != Some(id.epoch)
            || self.query != Some(id.query)
            || self
                .section
                .as_ref()
                .is_none_or(|section| section.sid != id.sid || section.key != id.section)
        {
            return;
        }
        let Some(target) = self.address(cx) else {
            return;
        };
        if self.grid_reset_pending {
            self.store(
                target,
                LibraryWork::SaveCursor {
                    query: id.query,
                    cursor: crate::browse::Cursor {
                        at: crate::browse::CursorAt::SlotIndex(0),
                        scroll: self.target_layout.grid_block_top(),
                    },
                },
                fx,
            );
            return;
        }
        let current = cx
            .focus
            .current
            .filter(|key| key.entry == self.entry)
            .and_then(|key| self.pair.detail.index_of(key.elem));
        let index = current.or_else(|| {
            cx.focus
                .remembered(self.pair.groups_config().detail)
                .and_then(|elem| self.pair.detail.index_of(elem))
        });
        let Some(index) = index else {
            return;
        };
        let at = listing
            .item(index)
            .filter(|item| !item.rk.is_empty())
            .map_or(crate::browse::CursorAt::SlotIndex(index), |item| {
                crate::browse::CursorAt::ItemKey {
                    sid: item.sid,
                    rk: item.rk.clone(),
                    slot: index,
                }
            });
        self.store(
            target,
            LibraryWork::SaveCursor {
                query: id.query,
                cursor: crate::browse::Cursor {
                    at,
                    scroll: self.scroll.pos,
                },
            },
            fx,
        );
    }

    /// Return false while a saved grid cannot yet be placed. An existing engine group
    /// always wins; this stale store snapshot is only the seed for an unseen section.
    pub(super) fn seed_cursor<H: LibraryLike>(
        &mut self,
        cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) -> bool {
        let group = self.pair.groups_config().detail;
        if self.wanted_kind.is_some() || cx.focus.remembered(group).is_some() {
            return true;
        }
        let Some(cursor) = H::listing(cx).cursor() else {
            return true;
        };
        if self.pair.detail.elems.is_empty() {
            return self.readout == Readout::Empty;
        }
        let (identity, slot) = match &cursor.at {
            crate::browse::CursorAt::ItemKey { sid, rk, slot } => {
                (Some((*sid, rk.as_str())), *slot)
            }
            crate::browse::CursorAt::SlotIndex(slot) => (None, *slot),
        };
        let stable = identity.and_then(|(sid, rk)| {
            self.keys.keys().iter().find_map(|key| {
            matches!(&key.identity, LibraryIdentity::Grid { section, sid: item_sid, rk: item_rk }
                if Some(section) == self.section.as_ref() && *item_sid == sid && item_rk == rk)
                .then_some(key.elem).filter(|elem| self.pair.detail.index_of(*elem).is_some())
        })
        });
        if let Some(elem) = stable.or_else(|| {
            self.pair
                .detail
                .elem_at(slot.min(self.pair.detail.elems.len().saturating_sub(1)))
        }) {
            fx.remember(group, elem);
            let index = self
                .pair
                .detail
                .index_of(elem)
                .expect("a published bookmark target");
            let scroll = if index == slot {
                cursor.scroll
            } else {
                self.target_layout.row_reveal(index / COLS)
            };
            self.scroll_target = scroll.clamp(0.0, self.target_layout.max_scroll());
            self.scroll.jump(self.scroll_target);
            self.restore_scroll = Some(self.scroll_target);
            self.relayout(cx.focus.current);
        }
        true
    }
}
