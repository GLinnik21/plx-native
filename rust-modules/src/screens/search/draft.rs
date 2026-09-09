//! The Search instance's unacknowledged editing state. The store owns committed query/results;
//! this draft owns edits until the next store notice acknowledges them. Draw never reads a store.
use crate::ui::machine::{Canon, TextEdit};
use crate::ui::text_buffer::TextBuffer;

pub(super) struct Draft {
    buffer: TextBuffer,
    profile: u32,
    pending: bool,
}

impl Draft {
    pub(super) fn new(profile: u32, query: &str) -> Self {
        Self { buffer: TextBuffer::new(query.into(), query.len()), profile, pending: false }
    }
    pub(super) fn query(&self) -> &str { self.buffer.text() }
    pub(super) fn caret(&self) -> usize { self.buffer.caret() }
    pub(super) fn profile(&self) -> u32 { self.profile }
    pub(super) fn pending(&self) -> bool { self.pending }

    /// A profile replacement always wins. An ordinary retained view never overwrites local
    /// input; only a store notification acknowledges it. Equal text keeps the user's caret.
    pub(super) fn observe(&mut self, profile: u32, query: &str, notified: bool) -> bool {
        if profile != self.profile {
            *self = Self::new(profile, query);
            return true;
        }
        if notified {
            if self.pending {
                if query == self.query() { self.pending = false; }
            } else if query != self.query() {
                self.buffer = TextBuffer::new(query.into(), query.len());
            }
        }
        false
    }

    /// Returns the final query for one store command, never a prediction's intermediate erase.
    pub(super) fn edit(&mut self, edit: &TextEdit) -> Option<String> {
        let previous = self.query().to_owned();
        self.buffer.edit(edit);
        if self.query() == previous { return None; }
        self.pending = true;
        Some(self.query().to_owned())
    }

    pub(super) fn replace(&mut self, query: &str) -> Option<String> {
        let changed = self.query() != query;
        self.buffer = TextBuffer::new(query.into(), query.len());
        if changed { self.pending = true; }
        changed.then(|| query.to_owned())
    }

    pub(super) fn to_end(&mut self) {
        self.buffer = TextBuffer::new(self.query().into(), self.query().len());
    }
    pub(super) fn write(&self, c: &mut Canon) {
        c.str(self.query()).u64(self.caret() as u64).u32(self.profile).bool(self.pending);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frozen_view_cannot_erase_an_earlier_commit_in_the_input_batch() {
        let mut draft = Draft::new(7, "");
        for (input, expected) in [("s", "s"), ("u", "su"), ("m", "sum"), ("summer ", "summer ")] {
            draft.observe(7, "", false);
            assert_eq!(draft.edit(&TextEdit::Commit(input.into())).as_deref(), Some(expected));
        }
        assert!(draft.pending());
        draft.observe(7, "sum", true);
        assert_eq!(draft.query(), "summer ", "an older acknowledgement is not the current draft");
        assert!(draft.pending());
        draft.edit(&TextEdit::Left);
        let caret = draft.caret();
        draft.observe(7, "summer ", true);
        assert!(!draft.pending());
        assert_eq!(draft.caret(), caret, "store acknowledgement must not move the insertion point");
    }

    #[test]
    fn profile_replacement_discards_pending_text_and_old_caret() {
        let mut draft = Draft::new(7, "old");
        draft.edit(&TextEdit::Commit("old account ".into()));
        assert!(draft.observe(8, "", false));
        assert_eq!((draft.query(), draft.caret(), draft.profile(), draft.pending()), ("", 0, 8, false));
        draft.observe(8, "new", true);
        assert_eq!(draft.query(), "new");
        draft.edit(&TextEdit::Left);
        draft.to_end();
        assert_eq!(draft.caret(), 3);
    }
}
