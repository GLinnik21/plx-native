//! **Legal notices and About, as pages of the Settings surface** (restructure phase 5b; the
//! documents are `ui/legal.rs`'s words, moved here unchanged). The index is a `TableScreen` whose
//! every row pushes a [`DocumentPage`]; a document is a `DocumentScreen` — one `Document` group
//! that scrolls inside and leaves at its ends, LEFT = BACK. About is a document pushed straight
//! from the root, so its crumb names Settings rather than the index.

use std::borrow::Cow;

use crate::ui::document_reader::DocumentReader;
use crate::ui::frame::Budget;
use crate::ui::form::{Form, FormId, FormSection, FormTable, RowKey, RowKind};
use crate::ui::machine::{Canon, Cx, Effects, EntryId, GroupId, Handled, Key, LogicalState, Machine};
use crate::ui::route_screen::RouteLayout;
use crate::ui::screen::{DrawFrame, FocusSource, HitSource, Part, RenderStrategy, Screen, ScreenEvent};
use crate::ui::table::Row;
use crate::ui::table_screen::{DocumentFocus, DocumentScreen, Header, TableScreen};
use crate::ui::{theme, Rect};

use super::family::{form_activate, form_focus, form_right_target, InnerHost, SettingsPage};
use super::registry::word;



/// **The one contact address the application prints.**
/// `every_document_prints_only_the_one_contact_address` scans the documents for stray `@`s;
/// `screens::consent` imports it.
pub(crate) const CONTACT_EMAIL: &str = "support@plxnative.com";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Page {
    Privacy,
    OpenSource,
    Ffmpeg,
    Source,
    Trademarks,
    Contact,
}

impl Page {
    pub(crate) const ALL: [Self; 6] = [
        Self::Privacy,
        Self::OpenSource,
        Self::Ffmpeg,
        Self::Source,
        Self::Trademarks,
        Self::Contact,
    ];
    fn title(self) -> &'static str {
        match self {
            Self::Privacy => crate::i18n::msg::settings_legal_privacy_title(),
            Self::OpenSource => crate::i18n::msg::settings_legal_opensource_title(),
            Self::Ffmpeg => crate::i18n::msg::settings_legal_ffmpeg_title(),
            Self::Source => crate::i18n::msg::settings_legal_source_title(),
            Self::Trademarks => crate::i18n::msg::settings_legal_trademarks_title(),
            Self::Contact => crate::i18n::msg::settings_legal_contact_title(),
        }
    }
    fn subtitle(self) -> &'static str {
        match self {
            Self::Privacy => crate::i18n::msg::settings_legal_privacy_subtitle(),
            Self::OpenSource => crate::i18n::msg::settings_legal_opensource_subtitle(),
            Self::Ffmpeg => crate::i18n::msg::settings_legal_ffmpeg_subtitle(),
            Self::Source => crate::i18n::msg::settings_legal_source_subtitle(),
            Self::Trademarks => crate::i18n::msg::settings_legal_trademarks_subtitle(),
            Self::Contact => crate::i18n::msg::settings_legal_contact_subtitle(),
        }
    }
    pub(crate) fn body(self) -> &'static str {
        match self {
            Self::Privacy => &PRIVACY,
            Self::OpenSource => &OPEN_SOURCE,
            Self::Ffmpeg => &FFMPEG,
            Self::Source => &SOURCE,
            Self::Trademarks => &TRADEMARKS,
            Self::Contact => &CONTACT,
        }
    }
}

/// The full privacy policy — the one narrative the consent screen's Privacy policy row also
/// opens, so the two doors cannot disagree.
pub(crate) fn privacy_policy() -> &'static str {
    &PRIVACY
}

static PRIVACY: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| [
    crate::i18n::msg::settings_legal_privacy_responsible(),
    crate::i18n::msg::settings_legal_privacy_responsible_body(),
    crate::i18n::msg::settings_legal_privacy_plex(),
    crate::i18n::msg::settings_legal_privacy_plex_body(),
    crate::i18n::msg::settings_legal_privacy_plex_link(),
    crate::i18n::msg::settings_legal_privacy_servers(),
    crate::i18n::msg::settings_legal_privacy_servers_body(),
    crate::i18n::msg::settings_legal_privacy_local(),
    crate::i18n::msg::settings_legal_privacy_local_body(),
    crate::i18n::msg::settings_legal_privacy_plaintext_body(),
    crate::i18n::msg::settings_legal_privacy_crashes(),
    crate::i18n::msg::settings_legal_privacy_crashes_body(),
    crate::i18n::msg::settings_legal_privacy_signin_body(),
    crate::i18n::msg::settings_legal_privacy_analytics(),
    crate::i18n::msg::settings_legal_privacy_analytics_body(),
    crate::i18n::msg::settings_legal_privacy_excluded(),
    crate::i18n::msg::settings_legal_privacy_excluded_body(),
    crate::i18n::msg::settings_legal_privacy_retention(),
    crate::i18n::msg::settings_legal_privacy_retention_body(),
    crate::i18n::msg::settings_legal_privacy_choices(),
    crate::i18n::msg::settings_legal_privacy_choices_body(),
    crate::i18n::msg::settings_legal_privacy_processing(),
    crate::i18n::msg::settings_legal_privacy_processing_body(),
    crate::i18n::msg::settings_legal_privacy_uninstall(),
    crate::i18n::msg::settings_legal_privacy_uninstall_body(),
    crate::i18n::msg::settings_legal_privacy_contact(),
    crate::i18n::msg::settings_legal_privacy_contact_body()
].join("\n\n"));
static OPEN_SOURCE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("{}\n\n{}", crate::i18n::msg::settings_legal_opensource_body(), include_str!("../../../LICENSE")));
static FFMPEG: std::sync::LazyLock<&'static str> = std::sync::LazyLock::new(crate::i18n::msg::settings_legal_ffmpeg_body);
static SOURCE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| crate::i18n::msg::settings_legal_source_body(env!("PLX_BUILD_SHA")));
static TRADEMARKS: std::sync::LazyLock<&'static str> = std::sync::LazyLock::new(crate::i18n::msg::settings_legal_trademarks_body);
static CONTACT: std::sync::LazyLock<&'static str> = std::sync::LazyLock::new(crate::i18n::msg::settings_legal_contact_body);
static ABOUT: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| crate::i18n::msg::settings_about_body(env!("PLX_BUILD_SHA"), env!("PLX_VERSION")));

// ---------------------------------------------------------------------------------------------
// the index
// ---------------------------------------------------------------------------------------------

/// A Legal index row's identity is the document it opens. Its key is hand-assigned, never the
/// enum's discriminant or its position, so reordering [`legal_form`] moves no focus key.
impl FormId for Page {
    fn key(&self) -> RowKey {
        RowKey(match self {
            Page::Privacy => 0,
            Page::OpenSource => 1,
            Page::Ffmpeg => 2,
            Page::Source => 3,
            Page::Trademarks => 4,
            Page::Contact => 5,
        })
    }
}

impl Page {
    /// The `SettingsPage::Document` argument that opens this page. `Document(i)` indexes
    /// [`Page::ALL`], so the byte is the page's place in that list.
    fn document(self) -> SettingsPage {
        let at = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        SettingsPage::Document(at as u8)
    }
}

/// The Legal index as a [`Form`]: every document is a `Nav` row pushing its `Document` page.
fn legal_form() -> Form<Page, (), SettingsPage> {
    let mut docs = FormSection::new(crate::i18n::msg::settings_legal_section());
    for page in Page::ALL {
        docs = docs.item(
            page,
            RowKind::Nav(page.document()),
            (),
            Row::new(page.title()).detail(page.subtitle()).chevron(true),
        );
    }
    Form::new().section(docs)
}

pub(crate) struct LegalIndex {
    entry: EntryId,
    form: FormTable<Page, (), SettingsPage>,
    state: IndexState,
}

struct IndexState {
    /// The focused row's key — an identity, not a position.
    sel: RowKey,
}

impl LogicalState for IndexState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel.0);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("legal sel={}", self.sel.0));
    }
}

impl LegalIndex {
    pub(crate) fn new(entry: EntryId) -> Self {
        let mut form = FormTable::new(super::registry::BAND);
        form.table.compact = false;
        form.table.header_ink = theme::TEXT_READING;
        form.set(legal_form(), None);
        form.table.list_focused = true;
        let sel = form.key_at(form.table.sel.max(0) as usize).unwrap_or(RowKey(0));
        Self {
            entry,
            form,
            state: IndexState { sel },
        }
    }

    fn view(&self) -> TableScreen<'_> {
        TableScreen::new(
            Header::new(
                RouteLayout::screen(),
                Some(crate::i18n::msg::settings_title()),
                crate::i18n::msg::settings_legal_title(),
                crate::i18n::msg::settings_legal_copy(),
            ),
            &self.form.table,
            GroupId(0),
            self.entry,
        )
        .keyed(&self.form)
    }
}

impl Machine<InnerHost> for LegalIndex {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Tick(t) => {
                self.form.table.update(t.dt(), RouteLayout::screen().sectioned_table().h);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                form_focus(&mut self.form, to.elem);
                if let Some(key) = self.form.key_at(self.form.table.sel.max(0) as usize) {
                    self.state.sel = key;
                }
                Handled::Yes
            }
            ScreenEvent::Activate(key) => {
                form_activate(&self.form, *key, fx);
                Handled::Yes
            }
            ScreenEvent::Input(crate::ui::machine::InputEvent {
                kind: crate::ui::machine::InputKind::Key { key: Key::Right, at_edge: true, .. },
                ..
            }) => {
                if let Some(key) = cx.focus.current.and_then(|k| form_right_target(&self.form, k.elem)) {
                    form_activate(&self.form, key, fx);
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

crate::focusable_via_view!(LegalIndex, InnerHost, view);

impl Screen<InnerHost> for LegalIndex {
    fn name(&self) -> &'static str {
        word::LEGAL
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed(crate::i18n::msg::settings_title()))
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        let mut v = self.view();
        Part::<InnerHost>::draw(&mut v, f, Rect::FULL);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
    fn focus_source(&self) -> FocusSource {
        FocusSource::Engine
    }
    fn hit_source(&self) -> HitSource {
        HitSource::Engine
    }
}

// ---------------------------------------------------------------------------------------------
// a document
// ---------------------------------------------------------------------------------------------

/// One document: a Legal page under the index, or About under the root.
pub(crate) struct DocumentPage {
    entry: EntryId,
    reader: DocumentReader,
    crumb: &'static str,
    title: &'static str,
    subtitle: &'static str,
    body: Cow<'static, str>,
    qr: Option<crate::ui::qr::QrCode>,
    guide_caption: Option<&'static str>,
    word: &'static str,
    state: DocState,
}

struct DocState {
    which: u8,
    /// The reading position, folded into the hashed state so a replay can tell two frames of the
    /// SAME document apart when only the scroll differs. `ui/document_reader.rs`'s own doc is
    /// explicit that `at_top`/`at_end` — which read `target`, not the animating `scroll.pos` — are
    /// what a direction key's routing turns on: short of an end the key scrolls the reader and
    /// `DocumentPage::step` keeps it (`Handled::Yes`), while at an end the same key is declined so
    /// the engine's edge rule can walk back out to the index. That makes the reading position
    /// exactly the kind of thing this trait exists to catch — a `move_by` that silently became a
    /// no-op, or moved the wrong way, or by the wrong step, still leaves `which` and every other
    /// surface word unchanged, so a build with that bug replays `verdict=SAME` across the whole
    /// document the same way an unfixed `PreviewState` (`consent.rs`) would over its own reader.
    ///
    /// This mirrors `DocumentReader::target` — the SETTLED destination `move_by` writes, never
    /// `scroll.pos`, which is the spring's per-frame animating value and so is RENDER state (it
    /// keeps ticking toward `target` for several frames after the key that moved it, which would
    /// make the hash keep changing on ticks where no input landed at all, the opposite of a
    /// logical-state hash's job). It is a local mirror rather than a read of `target` itself
    /// because that field is private to `document_reader.rs`, which this screen does not own; the
    /// mirror cannot drift from the real value because `DocumentPage::step` only ever advances it
    /// in the same branch, guarded by the same `at_top`/`at_end` check, that calls `move_by` for
    /// real — the two can never see a different verdict about whether this key actually moves the
    /// document. Counted in whole `document_reader::STEP`s rather than pixels: `move_by` moves by
    /// exactly one `STEP` per call (clamped at the far end, where the clamp itself is what makes
    /// `at_end()` true and stops this counter from advancing further), so a step count is the
    /// EXACT quantity input moves, with no float bits to reproduce and no risk of two builds
    /// disagreeing on a fractional pixel that never mattered to routing.
    pos: u32,
}

impl LogicalState for DocState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.which as u32);
        w.u32(self.pos);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("doc {} pos={}", self.which, self.pos));
    }
}

impl DocumentPage {
    pub(crate) fn legal(entry: EntryId, i: u8) -> Self {
        let page = Page::ALL[(i as usize).min(Page::ALL.len() - 1)];
        Self {
            entry,
            reader: DocumentReader::new(),
            crumb: crate::i18n::msg::settings_legal_title(),
            title: page.title(),
            subtitle: page.subtitle(),
            body: Cow::Borrowed(page.body()),
            qr: None,
            guide_caption: None,
            word: word::LEGAL,
            state: DocState { which: i, pos: 0 },
        }
    }

    pub(crate) fn about(entry: EntryId) -> Self {
        Self {
            entry,
            reader: DocumentReader::new(),
            crumb: crate::i18n::msg::settings_title(),
            title: crate::i18n::msg::settings_about_title(),
            subtitle: crate::i18n::msg::settings_about_subtitle(),
            body: Cow::Borrowed(&ABOUT),
            qr: None,
            guide_caption: None,
            word: word::LEGAL,
            state: DocState { which: 0xff, pos: 0 },
        }
    }

    pub(crate) fn contribute(entry: EntryId) -> Self {
        Self {
            entry,
            reader: DocumentReader::new(),
            crumb: crate::i18n::msg::settings_language_title(),
            title: crate::i18n::msg::settings_language_contribute(),
            subtitle: crate::i18n::msg::settings_language_contribute_copy(),
            body: Cow::Owned(contribution_address()),
            qr: crate::ui::qr::QrCode::new(crate::i18n::CONTRIBUTE_URL).ok(),
            guide_caption: Some(crate::i18n::msg::settings_language_contribute_body()),
            word: "contribute",
            state: DocState { which: 0xfe, pos: 0 },
        }
    }

    fn document_frame(&self) -> Rect {
        RouteLayout::screen().document(true)
    }

    fn guide(&self) -> Option<crate::ui::qr::QrLink<'_>> {
        self.guide_caption.map(|caption| crate::ui::qr::QrLink::new(caption, self.body.as_ref()))
    }

    fn view(&self) -> DocumentFocus<'_> {
        DocumentFocus {
            reader: &self.reader,
            frame: self.document_frame(),
            group: GroupId(0),
            entry: self.entry,
        }
    }
}

impl Machine<InnerHost> for DocumentPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, InnerHost>, _fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Tick(t) => {
                self.reader.update(t.dt());
                Handled::Yes
            }
            ScreenEvent::Input(crate::ui::machine::InputEvent {
                kind: crate::ui::machine::InputKind::Key { key: key @ (Key::Up | Key::Down), edge, .. },
                ..
            }) if *edge != crate::ui::machine::Edge::Up => {
                // a document scrolls INSIDE on UP/DOWN and leaves at its ends (spec §7.3 step 2):
                // the one element cannot MOVE, so the scroll is the page's own arm, and at an end
                // the key goes back to the engine, whose `neighbour` answers `Edge` there
                let inside = match key {
                    Key::Up => !self.reader.at_top(),
                    _ => !self.reader.at_end(),
                };
                if inside {
                    self.reader.move_by(if *key == Key::Up { -1 } else { 1 });
                    // Keep `DocState::pos` in lockstep with the move that just happened for real —
                    // see that field's doc for why this counter, and not a read of the reader's own
                    // `target`, is what gets hashed. `saturating_sub` is defensive rather than
                    // load-bearing: `inside` already proved `!at_top()` on the Up arm, i.e. `pos`
                    // cannot be 0 here, so the only way this saturates is a future edit that lets
                    // `pos` and the reader's real position drift apart — which is exactly the bug
                    // class this field exists to catch, so let it clamp instead of panicking.
                    if *key == Key::Up {
                        self.state.pos = self.state.pos.saturating_sub(1);
                    } else {
                        self.state.pos += 1;
                    }
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            _ => Handled::No,
        }
    }
}

crate::focusable_via_view!(DocumentPage, InnerHost, view);

impl Screen<InnerHost> for DocumentPage {
    fn name(&self) -> &'static str {
        self.word
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed(self.crumb))
    }
    fn prepare(&mut self, _b: &mut Budget, cx: &Cx<'_, InnerHost>) {
        let code = self.guide().map(|guide| guide.layout(self.document_frame(), cx.measure).code);
        if let (Some(qr), Some(code)) = (&mut self.qr, code) {
            qr.prepare(code);
        }
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        let document_frame = self.document_frame();
        if let Some(guide) = self.guide() {
            let layout = guide.draw(f.painter, document_frame, f.measure);
            if let Some(qr) = &self.qr { qr.draw(f.painter, layout.code); }
            Header::new(RouteLayout::screen(), Some(self.crumb), self.title, self.subtitle)
                .paint(f.painter, f.measure);
            return;
        }
        let Self { reader, crumb, title, subtitle, body, entry, .. } = self;
        let mut v = DocumentScreen::new(
            Header::new(RouteLayout::screen(), Some(crumb), title, subtitle),
            reader,
            body.as_ref(),
            GroupId(0),
            *entry,
        );
        v.doc.frame = document_frame;
        Part::<InnerHost>::draw(&mut v, f, Rect::FULL);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
    fn focus_source(&self) -> FocusSource {
        FocusSource::Engine
    }
    fn hit_source(&self) -> HitSource {
        HitSource::Engine
    }
}

/// A visual line break, not a different address. The path keeps its leading slash and all
/// GitHub route segments so typing the two lines reaches the QR's exact destination.
fn contribution_address() -> String {
    crate::i18n::CONTRIBUTE_URL.trim_start_matches("https://").replace("/blob/", "\n/blob/")
}

/// The `(focus key, destination)` of every `Nav` row the index lists — the structural navigation
/// test's expectation, built by the page's own form builder.
#[cfg(test)]
pub(super) fn nav_items_for_test() -> Vec<(u32, SettingsPage)> {
    let mut form: FormTable<Page, (), SettingsPage> = FormTable::new(super::registry::BAND);
    form.set(legal_form(), None);
    (0..form.table.n_rows() as usize)
        .filter_map(|i| match form.binding_at(i)?.kind.clone() {
            RowKind::Nav(dest) => Some((form.key_at(i)?.0, dest)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ui::fixture::FixtureMeasure;
    use crate::ui::hit::{HitMap, PointerKind};
    use crate::ui::machine::{
        Edge, FocusKey, FocusRead, Fx, InputEvent, InputKind, InputOwner, MachineId, NavOp, NavOpKind,
        PressRead, Source, Stamped, Tick,
    };
    use crate::ui::present::Present;
    use crate::ui::screen::{Activate, By, EdgeRule, Focusable, Hover, Stop};

    #[test]
    fn contribution_manual_address_is_the_complete_qr_destination() {
        let _guard = crate::testlock::serial();
        let page = DocumentPage::contribute(EntryId(0));
        assert_eq!(format!("https://{}", page.body.replace('\n', "")), crate::i18n::CONTRIBUTE_URL,
            "a viewer who cannot scan the QR needs the same complete address in text");
        assert!(page.body.lines().nth(1).unwrap().starts_with('/'));
    }

    #[test]
    fn every_contribution_locale_preserves_the_canonical_address() {
        use crate::i18n::{LocaleContext, SHIPPED};
        for preference in SHIPPED {
            let locale = LocaleContext::resolve(preference, None, None, None, None);
            let caption = crate::i18n::msg::settings_language_contribute_body_in(&locale);
            assert!(!caption.contains("github.com"), "only the caption is translated");
            assert_eq!(format!("https://{}", contribution_address().replace('\n', "")),
                crate::i18n::CONTRIBUTE_URL);
        }
    }

    /// **No document may print an address other than [`CONTACT_EMAIL`].** Written against the
    /// personal address these pages used to carry: a support address that reaches only some of
    /// the screens is worse than none, because the reader cannot tell which one is current. The
    /// scan is for `@` rather than for the old address, so the NEXT stray address fails too.
    #[test]
    fn every_document_prints_only_the_one_contact_address() {
        for page in Page::ALL {
            let local_len = CONTACT_EMAIL.find('@').expect("CONTACT_EMAIL has a local part");
            for (i, _) in page.body().match_indices('@') {
                let tail = i
                    .checked_sub(local_len)
                    .map(|start| &page.body()[start..])
                    .unwrap_or("");
                assert!(
                    tail.starts_with(CONTACT_EMAIL),
                    "{:?} prints an address that is not CONTACT_EMAIL",
                    page.title()
                );
            }
        }
    }

    /// **The policy must describe the build it ships in.** These assertions pin CLAIMS, not
    /// wording: each names a fact about this application that the document was silently wrong or
    /// silent about, and each was RED when it was written.
    ///
    /// * Nothing in the persisted session carries a playhead: `session.rs` has no such field, and
    ///   position is read from and reported to the server. **Do not cite `coldstart.rs` as the
    ///   evidence** — what it retired is `lastplace.json`, the last-PAGE/route bookmark (which
    ///   Detail or Library screen a cold boot reopened), which is a different artefact that was
    ///   conflated with a playhead when this test was written.
    /// * `session.rs` persists `recent_searches` — the exact search terms, per Home profile — plus
    ///   per-server tokens and server addresses. None were listed.
    /// * There are TWO identifiers and they never travel together: `install_id` goes to PostHog
    ///   only (`telemetry/sender.rs`) and `errors_id` to Sentry only, as `user.id`
    ///   (`telemetry/sentry.rs::attach_user`). The policy used to promise that a crash report
    ///   "carries no installation identifier" and "cannot be linked to any other report"; both
    ///   became false the day the crash-report id was added, and the assertion below is what
    ///   would have caught a policy that still said so.
    /// * The sign-in is stored OUTSIDE the app directory on purpose (`paths.rs`), so it survives a
    ///   reinstall. "Uninstall removes everything" would have been false.
    ///
    /// Reword the document freely; when you do, move the assertion with it deliberately rather
    /// than deleting it.
    #[test]
    fn the_policy_describes_what_this_build_actually_does() {
        let p = Page::Privacy.body();
        assert!(
            !p.contains("Home library choices, playback position"),
            "nothing in the session schema stores a playhead; the server holds position"
        );
        for claim in [
            "recent searches",
            "Analytics ID",
            "Crash report ID",
            "RETENTION",
            "WHERE DATA IS PROCESSED",
            "UNINSTALLING",
        ] {
            assert!(p.contains(claim), "the policy never mentions {claim:?}");
        }
        for stale in [
            "carries no installation identifier",
            "cannot be linked",
            "cannot be found or deleted",
            // consent and both identifiers END with the sign-in since 2026-09-04
            "NOT removed by signing out",
        ] {
            assert!(
                !p.contains(stale),
                "the policy still claims crash reports are anonymous: {stale:?}"
            );
        }
        // The two identifier names the Settings rows use are the names the policy uses.
        assert!(p.contains("Settings shows it as your Crash report ID"));
        assert!(p.contains("Settings shows that identifier as your Analytics ID"));
        // …and the policy says what `auth::forget_account` does to them.
        assert!(p.contains("signing out removes them with it"));
        // Every profile switched to on this television keeps its own offline access: a server
        // token and, for a PIN-protected profile, a one-way check of the PIN — never the PIN.
        assert!(p.contains("server access token"), "the policy never mentions per-profile offline access");
        assert!(p.contains("PIN"), "the policy never mentions the PIN check");
    }

    #[test]
    fn complete_gpl_is_available_offline() {
        let body = Page::OpenSource.body();
        assert!(body.contains("GPL-3.0-or-later"));
        assert!(body.contains(include_str!("../../../LICENSE")));
        assert!(body.contains("END OF TERMS AND CONDITIONS"));
    }

    #[test]
    fn legal_has_six_current_documents() {
        assert_eq!(Page::ALL.len(), 6);
    }

    /// The About page names the binary the user is RUNNING.
    ///
    /// It was a hand-typed `PlxNative 0.5.0` that no bump script touched, so it could only ever
    /// have been right by accident. Written against `identity::VERSION` rather than against
    /// `env!` again so that re-typing a literal here fails: on any developer build the two differ.
    #[test]
    fn about_names_the_running_version() {
        let v = crate::plex::identity::VERSION;
        assert!(
            ABOUT.contains(&format!("Version {v}")),
            "About should name {v}, says: {ABOUT:?}"
        );
    }

    /// The About page also names the exact COMMIT — `PLX_VERSION` alone cannot distinguish two
    /// trunk builds cut minutes apart, since both report the same `X.Y.0-dev`.
    #[test]
    fn about_names_a_build_sha() {
        assert!(
            ABOUT.contains("\nBuild "),
            "About should carry a Build line, says: {ABOUT:?}"
        );
        assert!(
            !env!("PLX_BUILD_SHA").is_empty(),
            "PLX_BUILD_SHA must never be the empty string (build.rs falls back to \"unknown\")"
        );
    }

    #[test]
    fn plex_boundary_is_explicit() {
        assert!(PRIVACY.contains(
            "Plex processes information received by those services under Plex’s own Privacy Policy"
        ));
        assert!(PRIVACY.contains("https://www.plex.tv/about/privacy-legal/"));
        assert!(!TRADEMARKS.contains("used under licence"));
    }

    /// **New for phase 5b.** Ported from `ui::legal`'s `legal_has_six_current_documents`,
    /// generalised from a bare count to the actual row content: the six rows `LegalIndex` draws
    /// are `Page::ALL`'s own words, in `Page::ALL`'s own order, and every one carries the chevron
    /// that promises it opens something. A count alone would still pass if two rows' title and
    /// subtitle were transposed; this would not.
    #[test]
    fn the_index_lists_every_document_in_order_with_a_chevron() {
        let idx = LegalIndex::new(EntryId(3));
        assert_eq!(idx.form.table.sections.len(), 1, "the index is one flat list, not grouped");
        let rows = &idx.form.table.sections[0].rows;
        assert_eq!(rows.len(), Page::ALL.len());
        for (row, page) in rows.iter().zip(Page::ALL) {
            assert_eq!(row.label, page.title());
            assert_eq!(row.detail, page.subtitle());
            assert_eq!(
                row.ticon,
                Some(crate::ui::icons::Icon::Chevron),
                "{page:?} must open on a press of its own row"
            );
        }
    }

    /// Activate the row whose focus element is `elem` on a fresh index, and return what it pushed.
    fn activate_row(elem: u32) -> Vec<Stamped<InnerHost>> {
        let mut idx = LegalIndex::new(EntryId(1));
        let m = FixtureMeasure;
        let cx = fixture_cx(&m, None);
        let mut buf: Vec<Stamped<InnerHost>> = Vec::new();
        let mut present = Present::default();
        {
            let mut fx = Effects::new(&mut buf, MachineId::Input, &mut present);
            idx.step(&ScreenEvent::Activate(elem), &cx, &mut fx);
        }
        buf
    }

    /// **New for phase 5b: "every index row opens a document that is non-empty."** Exercised
    /// through the real `LegalIndex` dispatch, addressing each row by its `Page` identity, rather
    /// than by reading `Page::ALL` a second time — a transposed row/page mapping would still pass
    /// a test that only asked `Page::ALL[i]` whether ITS OWN body is non-empty. A key that names
    /// no row — a stale focus key surviving a document being removed, for instance — must open
    /// nothing at all rather than silently opening the wrong page.
    #[test]
    fn every_index_row_opens_its_own_non_empty_document() {
        for (i, page) in Page::ALL.into_iter().enumerate() {
            let buf = activate_row(page.key().0);
            assert_eq!(buf.len(), 1, "{page:?} must push exactly one navigation effect");
            match &buf[0].fx {
                Fx::Nav(NavOp::Push(SettingsPage::Document(d))) => {
                    assert_eq!(*d as usize, i, "{page:?} opened document {d} instead of its own");
                }
                _ => panic!("{page:?} did not push its own document"),
            }
            let doc = DocumentPage::legal(EntryId(2), i as u8);
            assert!(!doc.body.is_empty(), "{page:?} has an empty document");
            assert_eq!(doc.title, page.title());
            assert_eq!(doc.subtitle, page.subtitle());
        }
        assert!(activate_row(0xdead).is_empty(), "a key naming no row must not open a document");
    }

    /// The keys are hand-assigned identities: distinct, and below the screen's band.
    #[test]
    fn every_page_has_its_own_key_below_the_band() {
        let mut keys: Vec<u32> = Page::ALL.iter().map(|p| p.key().0).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), Page::ALL.len());
        assert!(keys.iter().all(|k| *k < crate::screens::registry::BAND));
    }

    /// A `Cx<InnerHost>` with an explicit empty directory fixture for asking a `Focusable` query
    /// or stepping a `Machine` outside the real dispatcher.
    fn fixture_cx(m: &FixtureMeasure, focus: Option<FocusKey<u32>>) -> Cx<'_, InnerHost> {
        Cx {
            views: crate::stores::browse::DirectoryView::empty_for_test(),
            tick: Tick::default(),
            measure: m,
            press: PressRead::default(),
            focus: FocusRead { current: focus , ..Default::default() },
            owner: InputOwner::Entry(EntryId(0)),
        }
    }

    /// A scripted D-pad key, the shape `LegalIndex`/`DocumentPage::step` actually receive off the
    /// dispatcher — `app/bridge.rs::key_input`'s twin, kept local because that one is
    /// `pub(super)` to `app` and this file owns none of it.
    fn key_event(key: Key, edge: Edge, at_edge: bool) -> ScreenEvent<InnerHost> {
        ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: Source::Script,
            kind: InputKind::Key { key, sym: 0, wcode: 0, edge, at_edge },
        })
    }

    /// **route_screen rule 9, at the wiring level this file controls.** A document is a READ, so
    /// its LEFT edge is BACK unconditionally — the `DocumentFocus` override `table_screen.rs`
    /// applies over the plain `Document` group's geometric edges. Ported from `ui::legal`'s
    /// `right_enters_a_document_and_left_walks_all_the_way_back_out`, at the half this module
    /// alone can prove: walking all the way out to Settings crosses into `RouteSurface`'s own
    /// inner `NavStack` (`screens::settings`, not this file) and then the surface container
    /// itself (`app/bridge.rs`) — its own
    /// `the_settings_surface_owns_input_and_walks_its_own_stack` proves that trip end to end,
    /// through OK on Settings' own "Legal notices" row and BACK out of the whole surface again.
    #[test]
    fn a_document_s_left_edge_is_back_to_the_index() {
        let m = FixtureMeasure;
        let doc = DocumentPage::legal(EntryId(4), 0);
        let cx = fixture_cx(&m, None);
        let mut groups = Vec::new();
        Focusable::<InnerHost>::groups(&doc, &cx, &mut groups);
        assert_eq!(groups.len(), 1, "a document is one group, whatever its length");
        assert_eq!(
            groups[0].edge[2],
            EdgeRule::Nav(NavOpKind::Back),
            "LEFT on a document always walks back out — it is a read, so rule 9 has nothing to lose"
        );
    }

    /// **route_screen rules 8 and 9, on the index's own row column.** With no action band
    /// (`LegalIndex` draws none), LEFT off the rows follows the crumb straight back to Settings;
    /// RIGHT is handed to the SCREEN (`EdgeRule::Screen`) rather than resolved by the engine, which
    /// is what makes the re-delivered-key arm in `LegalIndex::step` reachable at all — see the next
    /// test for what the screen does with it.
    #[test]
    fn the_index_s_row_group_hands_left_to_back_and_right_to_the_screen() {
        let m = FixtureMeasure;
        let idx = LegalIndex::new(EntryId(5));
        let cx = fixture_cx(&m, None);
        let mut groups = Vec::new();
        Focusable::<InnerHost>::groups(&idx, &cx, &mut groups);
        assert_eq!(groups.len(), 1, "the index has one focusable column: its table");
        let g = groups[0];
        assert_eq!(g.len, Page::ALL.len(), "one focus stop per document");
        assert_eq!(
            g.edge[2],
            EdgeRule::Nav(NavOpKind::Back),
            "LEFT on the index, which owns no band, follows the crumb straight to Settings"
        );
        assert_eq!(g.edge[3], EdgeRule::Screen, "RIGHT on a chevron row is the screen's to answer");
    }

    /// The engine re-delivers an unhandled RIGHT with `at_edge: true` once its own `EdgeRule::
    /// Screen` fires (spec §7.1) — this is the exact event `LegalIndex::step`'s RIGHT arm answers,
    /// and it must open the row the FOCUSED key names, not row 0 and not whatever was open last.
    #[test]
    fn a_re_delivered_right_opens_the_currently_focused_row() {
        let mut idx = LegalIndex::new(EntryId(6));
        let m = FixtureMeasure;
        let focus = FocusKey { entry: EntryId(6), elem: Page::Trademarks.key().0 };
        let cx = fixture_cx(&m, Some(focus));
        let mut buf: Vec<Stamped<InnerHost>> = Vec::new();
        let mut present = Present::default();
        let ev = key_event(Key::Right, Edge::Down, true);
        {
            let mut fx = Effects::new(&mut buf, MachineId::Input, &mut present);
            assert_eq!(idx.step(&ev, &cx, &mut fx), Handled::Yes);
        }
        match &buf[0].fx {
            Fx::Nav(NavOp::Push(SettingsPage::Document(d))) => assert_eq!(Page::ALL[*d as usize], Page::Trademarks),
            _ => panic!("a re-delivered RIGHT must open the FOCUSED row, not a fixed one"),
        }
    }

    /// A screen never keeps its own idea of which row is selected: `family::form_focus` seats the
    /// drawn table on whatever key the ENGINE reports through `FocusMoved`, and this is what wires
    /// that call into `LegalIndex` specifically.
    #[test]
    fn focus_moving_onto_a_row_seats_the_table_there() {
        let mut idx = LegalIndex::new(EntryId(7));
        let m = FixtureMeasure;
        let cx = fixture_cx(&m, None);
        let mut buf: Vec<Stamped<InnerHost>> = Vec::new();
        let mut present = Present::default();
        let to = FocusKey { entry: EntryId(7), elem: Page::Source.key().0 };
        let ev = ScreenEvent::FocusMoved { from: None, to, by: By::Dir };
        {
            let mut fx = Effects::new(&mut buf, MachineId::Input, &mut present);
            assert_eq!(idx.step(&ev, &cx, &mut fx), Handled::Yes);
        }
        assert_eq!(idx.form.table.sel, 3, "the drawn selection follows the engine's own focus key");
        assert!(idx.form.table.list_focused);
    }

    /// **A document scrolls INSIDE on UP/DOWN and leaves only at its ends (spec §7.3 step 2).**
    /// The document's one element never MOVES, so `FocusEngine::set` would report `Outcome::
    /// Nothing` for it — there is no key for the engine to see land — which is why the PAGE itself
    /// has to notice the reader's own top/end and only THEN answer `Handled::No`, handing the key
    /// back to the engine's edge rule (`a_document_s_left_edge_is_back_to_the_index`'s sibling on
    /// the vertical axis, proven here instead because `Document`'s `neighbour` — not this file's —
    /// is what decides it, and this is the contract `DocumentPage::step` has to honour against it).
    #[test]
    fn a_document_scrolls_on_up_down_and_leaves_only_at_its_ends() {
        // `DocumentReader::move_by` reports to `ui::idle`'s process-global gate, so — like that
        // module's own scroll tests — this one takes the crate-wide lock rather than racing
        // another test's read of the same `DIRTY`/`DAMAGE_GEN` statics.
        let _guard = crate::testlock::serial();
        let mut doc = DocumentPage::legal(EntryId(8), 0);
        doc.reader.set_extent_for_test(500.0);
        let m = FixtureMeasure;
        let cx = fixture_cx(&m, None);
        let mut buf: Vec<Stamped<InnerHost>> = Vec::new();
        let mut present = Present::default();

        assert!(doc.reader.at_top());
        {
            let up = key_event(Key::Up, Edge::Down, false);
            let mut fx = Effects::new(&mut buf, MachineId::Input, &mut present);
            assert_eq!(
                doc.step(&up, &cx, &mut fx),
                Handled::No,
                "at the top, UP must be declined so the engine can walk back out to the index"
            );
        }

        {
            let down = key_event(Key::Down, Edge::Down, false);
            let mut fx = Effects::new(&mut buf, MachineId::Input, &mut present);
            assert_eq!(
                doc.step(&down, &cx, &mut fx),
                Handled::Yes,
                "short of the end, DOWN scrolls the reader and the page keeps the key"
            );
        }
        assert!(!doc.reader.at_top(), "the DOWN above must have moved the reader off the top");

        // Walk to the end: twenty steps of 192px (`document_reader::STEP`) each must clear a
        // 500px document well before the loop runs out.
        for _ in 0..20 {
            let down = key_event(Key::Down, Edge::Down, false);
            let mut fx = Effects::new(&mut buf, MachineId::Input, &mut present);
            doc.step(&down, &cx, &mut fx);
        }
        assert!(doc.reader.at_end());
        {
            let down = key_event(Key::Down, Edge::Down, false);
            let mut fx = Effects::new(&mut buf, MachineId::Input, &mut present);
            assert_eq!(
                doc.step(&down, &cx, &mut fx),
                Handled::No,
                "at the end, DOWN must be declined so the engine's own edge rule can run"
            );
        }
    }

    /// **The regression `DocState::pos` exists to close.** Before that field, `DocState` hashed
    /// only `which`, so a recorded `Legal notices -> "Open-source licences" -> DOWN x5` flow — the
    /// exact scenario a verifier constructed — wrote five IDENTICAL state hashes: the route and
    /// overlay words never move on a scroll, `focusprobe::line` has nothing to say about a family
    /// screen, and the engine's own focus scope is untouched (the document is ONE element that
    /// cannot MOVE, only scroll — `DocumentPage::step`'s own comment on the arm this test drives).
    /// A replay against that recording could not have told a working `move_by` apart from one that
    /// silently became a no-op, scrolled backwards, or moved by the wrong step: every one of those
    /// builds would have replayed `verdict=SAME` across the whole document, which is exactly the
    /// class of miss `docs/agent-reference.md`'s testing section names as the reason a fix needs a
    /// test that was RED against the broken behaviour, not just green against the fixed one.
    #[test]
    fn a_down_inside_a_document_changes_the_hashed_state() {
        let _guard = crate::testlock::serial();
        let mut doc = DocumentPage::legal(EntryId(9), 1);
        // Long enough that five DOWNs (5 * `document_reader::STEP` = 960px) never reach the end,
        // so every one of them is a genuine mid-document scroll rather than a clamp at `at_end()`.
        doc.reader.set_extent_for_test(5000.0);
        let m = FixtureMeasure;
        let cx = fixture_cx(&m, None);
        let mut present = Present::default();

        let mut hashes = vec![doc.state.hash()];
        for _ in 0..5 {
            let mut buf: Vec<Stamped<InnerHost>> = Vec::new();
            let down = key_event(Key::Down, Edge::Down, false);
            let mut fx = Effects::new(&mut buf, MachineId::Input, &mut present);
            assert_eq!(doc.step(&down, &cx, &mut fx), Handled::Yes);
            hashes.push(doc.state.hash());
        }

        // Every consecutive pair must differ — a stuck, reversed or mis-stepped `move_by` leaves at
        // least one pair identical, which is precisely the false `verdict=SAME` the unfixed field
        // produced across the verifier's whole five-DOWN sequence.
        for w in hashes.windows(2) {
            assert_ne!(
                w[0], w[1],
                "a DOWN inside the document must change the hashed state, not just the render position"
            );
        }

        // Walking back UP must retrace the exact same sequence of hashes in reverse: the hash is a
        // function of the reading POSITION (`DocState::pos`, mirroring the reader's settled
        // `target`), never of how many keys have been pressed or which direction they came from —
        // a counter that only ever incremented would pass the loop above while still being wrong.
        for expect in hashes.iter().rev().skip(1) {
            let mut buf: Vec<Stamped<InnerHost>> = Vec::new();
            let up = key_event(Key::Up, Edge::Down, false);
            let mut fx = Effects::new(&mut buf, MachineId::Input, &mut present);
            assert_eq!(doc.step(&up, &cx, &mut fx), Handled::Yes);
            assert_eq!(
                doc.state.hash(),
                *expect,
                "walking back UP must retrace the same positions, not accumulate a new one"
            );
        }
    }

    /// **Rule 11 (route_screen's, restated in `table_screen.rs`'s own module doc): "every
    /// selectable row is a stop — hover parks, a click activates".** `ui::legal`'s retired
    /// `hover_parks_an_index_row_and_a_document_has_no_click_target` proved both halves of that
    /// sentence by simulating a mouse over the bespoke `RouteFocus` ladder this phase deleted; the
    /// phase-5b audit that sent this lane here found no analogous proof against the new
    /// architecture and flagged the gap rather than assuming `table_screen.rs`'s prose covered it.
    /// It does not, on inspection: `ui/table_screen.rs::tests` has exactly three tests and none of
    /// them calls `Part::draw` at all, so the actual `hover`/`activate` VALUES `TablePart::draw`
    /// attaches to a row's `Stop` are asserted NOWHERE in this tree, table_screen.rs included —
    /// which is worth recording here because the natural next reader will otherwise assume "the
    /// shared widget's own tests must cover this" and stop looking, exactly as the first assessment
    /// did.
    ///
    /// **Why this test cannot call `Part::draw` either, and does not pretend to.** `TablePart::draw`
    /// paints the row through `TableView::draw` before it ever reaches the stop-registration loop,
    /// and that paint measures text through SDL2_ttf and issues GL calls — neither of which a host
    /// unit build links (`ui/table_screen.rs::tests`' own comment says so beside its three
    /// `Focusable`-only tests, and `ui/screen.rs::ClipScope`'s doc says the same of the GL half: "no
    /// GL is linked" on the host test binary). So this test MIRRORS `TablePart::draw`'s documented
    /// contract — `Hover::Focus`, `Activate::Direct` for a plain row — onto REAL geometry instead of
    /// calling into the function that assigns it: `LegalIndex`'s own `TableView::row_frame`, the
    /// exact walk `TablePart::draw` would ask for each stop's rect. What that buys is everything on
    /// either side of the one call this tree cannot make: proof that two of Legal's six real rows
    /// occupy two real, non-overlapping bands (so a hover CAN tell them apart at all), that the
    /// `HitMap` resolves a point in each band to that row and nothing above the list to any row, and
    /// — the half nothing else in this tree touches — that the resulting `Activate` reaches the REAL
    /// `LegalIndex::step` and opens the ROW THE POINTER ACTUALLY LANDED ON. The one link left
    /// unproven is named in the paragraph above rather than left implicit: whether `TablePart::draw`
    /// truly assigns the values mirrored here is `table_screen.rs`'s claim to keep, not this test's
    /// to fake by calling code this build cannot run.
    #[test]
    fn a_hover_over_a_real_row_parks_it_and_a_click_opens_the_row_the_pointer_landed_on() {
        let mut idx = LegalIndex::new(EntryId(12));
        let frame = RouteLayout::screen().sectioned_table();
        // Two rows chosen apart from each other (not neighbours), so a geometry bug that merges
        // adjacent rows into one band would still be caught.
        let r0 = idx.form.table.row_frame(frame, 0).expect("row 0 is drawn");
        let r4 = idx.form.table.row_frame(frame, 4).expect("row 4 is drawn");
        assert!(
            r0.y + r0.h <= r4.y,
            "two different rows must occupy two non-overlapping bands, or a hover could never \
             tell them apart: row 0 {r0:?}, row 4 {r4:?}"
        );

        let entry = EntryId(12);
        let mirrored_row_stop = |elem: u32, r: Rect| Stop {
            key: FocusKey { entry, elem },
            rect: r,
            rest_rect: r,
            clip: frame,
            hover: Hover::Focus,
            activate: Activate::Direct,
        };
        let mut map: HitMap<u32> = HitMap::new();
        map.fill(vec![mirrored_row_stop(0, r0), mirrored_row_stop(4, r4)]);
        map.swap();

        let mid = |r: Rect| (r.x + r.w * 0.5, r.y + r.h * 0.5);
        let (x0, y0) = mid(r0);
        let (x4, y4) = mid(r4);
        assert_eq!(
            map.resolve(Some(entry), PointerKind::Move, x0, y0, None).focus.map(|k| k.elem),
            Some(0),
            "hovering row 0's own band parks row 0"
        );
        assert_eq!(
            map.resolve(Some(entry), PointerKind::Move, x4, y4, None).focus.map(|k| k.elem),
            Some(4),
            "…and hovering a DIFFERENT row's band parks a DIFFERENT row, not the last one hovered"
        );
        assert!(
            map.resolve(Some(entry), PointerKind::Move, x0, frame.y - 400.0, None).focus.is_none(),
            "above the list is dead space — hovering there parks nothing"
        );

        // A click resolves the same hit and additionally asks for the row's `Activate` — deliver
        // exactly that event to the real screen, the half neither `table_screen.rs` nor `ui/hit.rs`
        // can answer on their own, since it is a question about what LEGAL does with an activation,
        // not about the widget or the map that hand it one.
        let click = map.resolve(Some(entry), PointerKind::Click, x4, y4, None);
        let (key, act) = click.activate.expect("a click on a row's own band always activates it");
        assert_eq!(act, Activate::Direct, "a row's door opens at once, with no press dip to arm");
        assert_eq!(key.elem, 4, "the click landed in row 4's band and must activate row 4");

        let m = FixtureMeasure;
        let cx = fixture_cx(&m, Some(key));
        let mut buf: Vec<Stamped<InnerHost>> = Vec::new();
        let mut present = Present::default();
        let ev = ScreenEvent::Activate(key.elem);
        {
            let mut fx = Effects::new(&mut buf, MachineId::Input, &mut present);
            assert_eq!(
                idx.step(&ev, &cx, &mut fx),
                Handled::Yes,
                "the index answers the delivered Activate"
            );
        }
        // `Fx`/`Stamped` derive no `Debug` (`ui/machine.rs`), so a mismatch here is named in the
        // panic text rather than interpolated off the value itself.
        assert_eq!(buf.len(), 1, "a click-activated row must push exactly one effect");
        match &buf[0].fx {
            Fx::Nav(NavOp::Push(SettingsPage::Document(d))) => {
                assert_eq!(*d, 4, "the click on row 4 must open row 4's own document, not another");
            }
            _ => panic!("a click-activated row must push its own document, not some other effect"),
        }
    }

    /// **Rule 11's second half: "a document has no click target."** `ui::legal`'s retired test
    /// proved this by asserting `pointer_focus` returned `false` everywhere over a pushed document
    /// under the old bespoke ladder. The new `DocumentPart` does not answer the question the same
    /// way — it registers a `Stop` covering the WHOLE reading rect (`table_screen.rs`'s own source,
    /// read for this audit: `hover: Hover::Ignore, activate: Activate::Direct`) rather than
    /// registering nothing — so "no click target" is no longer "the pointer resolves no hit here"
    /// but "hovering never parks anything AND a landed click reaches a screen that has nothing to
    /// do with it." Both halves matter and neither was provable from the old test's assertions,
    /// which is why this is a new test rather than a transliteration.
    ///
    /// As in the sibling test above, this MIRRORS `DocumentPart::draw`'s documented stop policy
    /// onto real geometry (`RouteLayout::document`, pure) rather than calling `draw` itself — see
    /// that test's doc for why a host build cannot make the other call.
    #[test]
    fn a_click_on_a_document_parks_nothing_and_the_screen_declines_the_activation() {
        let mut doc = DocumentPage::legal(EntryId(13), 2);
        let frame = RouteLayout::screen().document(true);

        let mut map: HitMap<u32> = HitMap::new();
        map.fill(vec![Stop {
            key: FocusKey { entry: EntryId(13), elem: 0u32 },
            rect: frame,
            rest_rect: frame,
            clip: frame,
            hover: Hover::Ignore,
            activate: Activate::Direct,
        }]);
        map.swap();

        let (mx, my) = (frame.x + frame.w * 0.5, frame.y + frame.h * 0.5);
        assert!(
            map.resolve(Some(EntryId(13)), PointerKind::Move, mx, my, None).focus.is_none(),
            "hovering a document parks nothing — Hover::Ignore is the whole of rule 11's second half"
        );
        let click = map.resolve(Some(EntryId(13)), PointerKind::Click, mx, my, None);
        assert!(click.focus.is_none(), "a click on a document must not park focus on it either");
        let (key, act) = click.activate.expect("the stop still resolves a hit — this is not a miss");
        assert_eq!(act, Activate::Direct, "the mirrored stop's own policy, unchanged by the miss/hit path above");

        // Deliver the resulting event to the REAL screen: whether an activation the map hands out
        // actually DOES anything is a fact about `DocumentPage`, not about the map or the widget,
        // and it is the fact "a document has no click target" is actually a claim about.
        let hash_before = doc.state.hash();
        let m = FixtureMeasure;
        let cx = fixture_cx(&m, Some(key));
        let mut buf: Vec<Stamped<InnerHost>> = Vec::new();
        let mut present = Present::default();
        let ev = ScreenEvent::Activate(key.elem);
        {
            let mut fx = Effects::new(&mut buf, MachineId::Input, &mut present);
            assert_eq!(
                doc.step(&ev, &cx, &mut fx),
                Handled::No,
                "the document has no reaction to an Activate at all — a click reaches nothing"
            );
        }
        assert!(buf.is_empty(), "a click on a document must push no effect whatsoever");
        assert_eq!(
            doc.state.hash(),
            hash_before,
            "…and it must leave the reading position and every other hashed fact untouched"
        );
    }

    /// Every Legal notices row fits its column in every shipped language, measured with the
    /// device's whole-pixel advances (see `settings_text_fit_tests.rs` for the Settings root).
    #[test]
    fn every_legal_row_fits_its_column_in_every_language() {
        use crate::i18n::{language_on_this_thread_for_test, SHIPPED};
        let frame_w = crate::ui::route_screen::RouteLayout::screen().sectioned_table().w;
        let mut out = Vec::new();
        for language in SHIPPED {
            let _guard = language_on_this_thread_for_test(language);
            let tag = language.tag();
            let table = &LegalIndex::new(EntryId(0)).form.table;
            out.extend(table.app_fit_failures(frame_w, tag));
        }
        crate::ui::table::assert_no_fit_failures(&out);
    }
}
