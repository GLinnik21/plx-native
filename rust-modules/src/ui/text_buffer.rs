//! Owned text and insertion point, with no store, keyboard, clock or render dependency.
//! Keep one buffer for a sequence of edits: several commits can arrive before a retained
//! store view acknowledges them. Reconstructing from that old view loses preceding edits.

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextBuffer {
    text: String,
    caret: usize,
}

impl TextBuffer {
    pub(crate) fn new(text: String, caret: usize) -> Self {
        let mut caret = caret.min(text.len());
        while !text.is_char_boundary(caret) { caret -= 1; }
        Self { text, caret }
    }
    pub(crate) fn text(&self) -> &str { &self.text }
    pub(crate) fn caret(&self) -> usize { self.caret }
    pub(crate) fn into_text(self) -> String { self.text }

    /// Apply a normalized keyboard event without consulting a store publication.
    pub(crate) fn edit(&mut self, edit: &super::machine::TextEdit) {
        use super::machine::TextEdit;
        match edit {
            TextEdit::Commit(text) => self.commit(text),
            TextEdit::Backspace => self.backspace(),
            TextEdit::Clear => self.clear(),
            TextEdit::Left => self.left(),
            TextEdit::Right => self.right(),
        }
    }

    /// A literal insertion, unlike the TV's prediction-bearing commit.
    pub(crate) fn insert(&mut self, text: &str) {
        self.text.insert_str(self.caret, text);
        self.caret += text.len();
    }

    /// LG sends a whole prediction without a corresponding deletion event (docs/search.md).
    /// Preserve the commit boundary: splitting it into chars loses that replacement signal.
    pub(crate) fn commit(&mut self, text: &str) {
        if let Some(start) = replaces_word_at(&self.text, self.caret, text) {
            self.text.replace_range(start..self.caret, "");
            self.caret = start;
        }
        self.insert(text);
    }

    pub(crate) fn backspace(&mut self) {
        let previous = prev_boundary(&self.text, self.caret);
        self.text.replace_range(previous..self.caret, "");
        self.caret = previous;
    }
    pub(crate) fn clear(&mut self) { self.text.clear(); self.caret = 0; }
    pub(crate) fn left(&mut self) { self.caret = prev_boundary(&self.text, self.caret); }
    pub(crate) fn right(&mut self) { self.caret = next_boundary(&self.text, self.caret); }
}

fn prev_boundary(q: &str, at: usize) -> usize {
    let mut i = at.min(q.len());
    loop {
        if i == 0 { return 0; }
        i -= 1;
        if q.is_char_boundary(i) { return i; }
    }
}
fn next_boundary(q: &str, at: usize) -> usize {
    let mut i = at.saturating_add(1).min(q.len());
    while i < q.len() && !q.is_char_boundary(i) { i += 1; }
    i
}

/// Multi-character commits at the end replace the final word, including corrections that
/// do not share its prefix. Single keys, spaces and mid-string commits are plain insertions.
fn replaces_word_at(q: &str, caret: usize, text: &str) -> Option<usize> {
    if text.chars().count() < 2 || caret != q.len() { return None; }
    let head = q.get(..caret)?;
    let start = match head.rfind(char::is_whitespace) {
        Some(i) => i + head[i..].chars().next().map_or(1, char::len_utf8),
        None => 0,
    };
    (start < caret).then_some(start)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// **Tapping a word prediction is a REPLACE and the panel never says so.** Typing `sum` and
    /// tapping the offered `summer` gave `sumsummer` — device-reported, and the event log shows why
    /// there is no exact fix available: three single-character commits for the keys, then one
    /// commit of `"summer "`, and no `SDL_TEXTEDITING`, no backspace keys, nothing else at all.
    ///
    /// So this is a rule, and a rule's tests are the cases it must NOT fire on.
    #[test]
    fn a_multi_character_commit_at_the_end_replaces_the_word_being_typed() {
        // the reported case, exactly as the log has it — trailing space included
        assert_eq!(replaces_word_at("sum", 3, "summer "), Some(0));
        // …and mid-sentence, where only the LAST word goes
        assert_eq!(replaces_word_at("the sum", 7, "summer "), Some(4));

        // ---- the cases it must not fire on ----
        // A single character is a KEY PRESS, always. Without this guard, typing a letter twice
        // replaces it with itself and `aa` is impossible to type.
        assert_eq!(replaces_word_at("a", 1, "a"), None);
        assert_eq!(replaces_word_at("sum", 3, "m"), None);
        assert_eq!(
            replaces_word_at("sum", 3, " "),
            None,
            "the space bar is a space"
        );
        // A caret the user MOVED is not a word being predicted on: a multi-character insertion
        // there must land where they put it (this is the `txt:` dev token's path, and the ◀/▶ one).
        assert_eq!(replaces_word_at("summer", 4, "XY"), None);
        // Nothing to replace: the query ends in a space, so the prediction is for the NEXT word.
        assert_eq!(replaces_word_at("the ", 4, "office "), None);
        assert_eq!(replaces_word_at("", 0, "summer "), None);

        // Multi-byte: the word boundary is found by BYTE index and must land on a char boundary,
        // or the `replace_range` that follows panics inside the SDL event loop.
        assert_eq!(replaces_word_at("суб", "суб".len(), "суббота "), Some(0));
        let two = "я суб";
        assert_eq!(
            replaces_word_at(two, two.len(), "суббота "),
            Some("я ".len())
        );
    }

    #[test]
    fn consecutive_commits_edit_the_owned_draft_without_a_store_round_trip() {
        let retained = String::new();
        let mut draft = TextBuffer::new(retained.clone(), 0);
        for commit in ["s", "u", "m", "summer ", "holiday "] { draft.commit(commit); }
        assert_eq!(draft.text(), "summer holiday ");
        assert_eq!(draft.caret(), draft.text().len());
        assert!(retained.is_empty());
        let untouched = draft.clone();
        draft.left();
        draft.backspace();
        draft.insert("X");
        assert_eq!(draft.text(), "summer holidaX ");
        assert_eq!(untouched.text(), "summer holiday ");
        draft.clear();
        assert_eq!((draft.text(), draft.caret()), ("", 0));
        draft.backspace(); draft.left(); draft.right();
        assert_eq!((draft.text(), draft.caret()), ("", 0));
    }

    #[test]
    fn stale_offsets_and_every_unicode_step_stay_on_character_boundaries() {
        for offset in 0..=12 {
            let mut text = TextBuffer::new("я🙂б".into(), offset);
            assert!(text.text().is_char_boundary(text.caret()));
            text.left(); text.insert("Ж"); text.right(); text.backspace();
            assert!(text.text().is_char_boundary(text.caret()));
        }
        let mut text = TextBuffer::new("я🙂б".into(), usize::MAX);
        text.left(); assert_eq!(text.caret(), "я🙂".len());
        text.backspace(); assert_eq!(text.text(), "яб");
        text.left(); text.left(); assert_eq!(text.caret(), 0);
        text.right(); assert_eq!(text.caret(), "я".len());
        text.commit("XY"); assert_eq!(text.text(), "яXYб");
        text.right(); text.right(); assert_eq!(text.caret(), text.text().len());
    }
}
