//! FinNative's **manual sign-in form**: a server address typed with the television's keyboard,
//! then a username and password, or Quick Connect against that server.
//!
//! It is a sheet of the sign-in screen, the way the report alert is: while it is open its five
//! elements are the screen's only focus group, it takes every key before the read-out does, and it
//! is drawn in place of the read-out. The screen owns the keyboard and the effects; this module is
//! the form's state, its geometry and its drawing.
//!
//! **The password never leaves this struct except into the sign-in request.** It is not written to
//! the logical state (only its length is), not logged, drawn only as dots
//! ([`plx_ui::text_field`]), and typed text for it is delivered as [`TextEntry::Secret`] so the
//! input recording refuses it. The request carrying it goes to the backend's one-shot slot, never
//! into the session's serialized work description.

use plx_jellyfin::jellyfin::backend::{SignInMethod, SignInRequest};
use plx_machine::machine::{Measure, TextEdit};
use plx_platform::i18n::msg;
use plx_ui::route_screen::RouteLayout;
use plx_ui::screen::TextEntry;
use plx_ui::text_buffer::TextBuffer;
use plx_ui::text_field::{TextField, FIELD_H};
use plx_ui::widgets::Button;
use plx_ui::{theme, Env, Painter, Rect, View};
use std::ffi::CStr;

/// The form's elements, in walk order. Distinct from the read-out's and the alert's ids, so a
/// focus key held across the sheet opening or closing can never name the wrong control.
pub(in crate::login) const SERVER: u32 = 10;
pub(in crate::login) const USERNAME: u32 = 11;
pub(in crate::login) const PASSWORD: u32 = 12;
pub(in crate::login) const SIGN_IN: u32 = 13;
pub(in crate::login) const QUICK_CONNECT: u32 = 14;
pub(in crate::login) const ELEMS: [u32; 5] = [SERVER, USERNAME, PASSWORD, SIGN_IN, QUICK_CONNECT];

/// Between one field and the next, and between the last field and the buttons.
const FIELD_GAP: f32 = theme::space::MD;
/// The fields' width: the content column, capped so an address reads as one line, not a banner.
const FIELD_MAX_W: f32 = 820.0;

pub(in crate::login) struct Form {
    server: TextBuffer,
    username: TextBuffer,
    password: TextBuffer,
    /// The field the keyboard is up for, if any.
    editing: Option<u32>,
    /// Why the last submit was refused (a missing address or username), until the next edit.
    problem: Option<&'static CStr>,
}

impl Form {
    /// A fresh form, the address prefilled with the server the last sign-in found or was given.
    pub(in crate::login) fn new(server_hint: &str) -> Self {
        Self {
            server: TextBuffer::new(server_hint.into(), server_hint.len()),
            username: TextBuffer::new(String::new(), 0),
            password: TextBuffer::new(String::new(), 0),
            editing: None,
            problem: None,
        }
    }

    pub(in crate::login) fn editing(&self) -> Option<u32> {
        self.editing
    }

    /// What typed text is for right now: nothing, an ordinary field, or the password.
    pub(in crate::login) fn text_entry(&self) -> TextEntry {
        match self.editing {
            Some(PASSWORD) => TextEntry::Secret,
            Some(_) => TextEntry::Plain,
            None => TextEntry::None,
        }
    }

    pub(in crate::login) fn is_field(elem: u32) -> bool {
        matches!(elem, SERVER | USERNAME | PASSWORD)
    }

    /// Put the keyboard up for `field` (or take it down with `None`). Answers whether that changed
    /// anything, so the caller sends the keyboard effect only on a real change.
    pub(in crate::login) fn set_editing(&mut self, field: Option<u32>) -> bool {
        let field = field.filter(|f| Self::is_field(*f));
        if self.editing == field {
            return false;
        }
        if let Some(f) = field {
            // The keyboard comes up with the caret at the end of what is already there.
            let buffer = self.buffer_mut(f);
            let text = buffer.text().to_owned();
            *buffer = TextBuffer::new(text.clone(), text.len());
        }
        self.editing = field;
        true
    }

    fn buffer_mut(&mut self, field: u32) -> &mut TextBuffer {
        match field {
            USERNAME => &mut self.username,
            PASSWORD => &mut self.password,
            _ => &mut self.server,
        }
    }

    fn buffer(&self, field: u32) -> &TextBuffer {
        match field {
            USERNAME => &self.username,
            PASSWORD => &self.password,
            _ => &self.server,
        }
    }

    /// One keyboard edit, to the field being edited.
    pub(in crate::login) fn edit(&mut self, edit: &TextEdit) {
        let Some(field) = self.editing else { return };
        self.buffer_mut(field).edit(edit);
        self.problem = None;
    }

    /// Where focus lands when the form opens: the address while it is empty, else the username
    /// (the address is usually the found server, already filled in).
    pub(in crate::login) fn seat(&self) -> u32 {
        if self.server.text().trim().is_empty() { SERVER } else { USERNAME }
    }

    /// The request a press of *Sign in* (`quick` false) or *Use Quick Connect* (`quick` true)
    /// makes, or why it can't be made yet — which the form then shows under its fields. Jellyfin
    /// accepts an empty password, so only the address and, for a password sign-in, the username
    /// are required.
    pub(in crate::login) fn submit(&mut self, quick: bool) -> Option<SignInRequest> {
        let server = self.server.text().trim().to_owned();
        let username = self.username.text().trim().to_owned();
        if server.is_empty() {
            self.problem = Some(msg::browse_jf_need_server_c());
            return None;
        }
        let method = if quick {
            SignInMethod::QuickConnect
        } else {
            if username.is_empty() {
                self.problem = Some(msg::browse_jf_need_username_c());
                return None;
            }
            SignInMethod::Password { username, password: self.password.text().to_owned() }
        };
        self.problem = None;
        Some(SignInRequest { server, method })
    }

    /// The field or button rect of `elem`, the one geometry focus, hit-testing and drawing share.
    pub(in crate::login) fn rect(elem: u32, measure: &dyn Measure) -> Option<Rect> {
        let layout = RouteLayout::screen();
        let w = layout.content.w.min(FIELD_MAX_W);
        let buttons_h = layout.action.h;
        let total = 3.0 * FIELD_H + 3.0 * FIELD_GAP + buttons_h;
        let x = layout.content.x;
        let top = (layout.content.cy() - total * 0.5).max(layout.content.y);
        let field = |i: f32| Rect::new(x, top + i * (FIELD_H + FIELD_GAP), w, FIELD_H);
        let buttons_y = top + 3.0 * (FIELD_H + FIELD_GAP);
        let sign_in_w = Button::pill_w_measured(msg::browse_action_sign_in_c(), theme::size::BODY, false, None, measure);
        let quick_w = Button::pill_w_measured(msg::browse_jf_quick_connect_c(), theme::size::BODY, false, None, measure);
        match elem {
            SERVER => Some(field(0.0)),
            USERNAME => Some(field(1.0)),
            PASSWORD => Some(field(2.0)),
            SIGN_IN => Some(Rect::new(x, buttons_y, sign_in_w, buttons_h)),
            QUICK_CONNECT => Some(Rect::new(x + sign_in_w + plx_ui::widgets::CONTROL_GAP, buttons_y, quick_w, buttons_h)),
            _ => None,
        }
    }

    /// The focus stop of `elem`: a field's plate, or a button's pill.
    pub(in crate::login) fn stop_rect(elem: u32, measure: &dyn Measure) -> Option<Rect> {
        let r = Self::rect(elem, measure)?;
        Some(if Self::is_field(elem) { plx_ui::text_field::plate(r) } else { r })
    }

    /// The group's whole extent.
    pub(in crate::login) fn extent(measure: &dyn Measure) -> Rect {
        ELEMS
            .iter()
            .filter_map(|&e| Self::stop_rect(e, measure))
            .reduce(|a, b| a.union(b))
            .unwrap_or(Rect::FULL)
    }

    pub(in crate::login) fn draw(&self, p: Painter, focus: Option<u32>, press_scale: f32, measure: &dyn Measure) {
        let layout = RouteLayout::screen();
        layout.draw_narrative(p, None, msg::browse_jf_manual(), msg::browse_jf_form_copy(), theme::size::LABEL, measure);
        let fields: [(u32, &CStr, bool); 3] = [
            (SERVER, msg::browse_jf_server_c(), false),
            (USERNAME, msg::browse_jf_username_c(), false),
            (PASSWORD, msg::browse_jf_password_c(), true),
        ];
        for (elem, label, secret) in fields {
            let Some(rect) = Self::rect(elem, measure) else { continue };
            let buffer = self.buffer(elem);
            let mut field = TextField::new(label, buffer.text())
                .caret(buffer.caret())
                .secret(secret)
                .hot(if focus == Some(elem) { 1.0 } else { 0.0 })
                .editing(self.editing == Some(elem), true);
            if elem == SERVER {
                field = field.placeholder(msg::browse_jf_server_example_c());
            }
            field.draw(p, rect, measure);
        }
        for (elem, label) in [(SIGN_IN, msg::browse_action_sign_in_c()), (QUICK_CONNECT, msg::browse_jf_quick_connect_c())] {
            let Some(rect) = Self::rect(elem, measure) else { continue };
            let scale = if focus == Some(elem) { press_scale } else { 1.0 };
            Button::new(label.as_ptr(), theme::size::BODY, rect)
                .focused(focus == Some(elem))
                .scale(scale)
                .draw(&Env::inert(), p);
        }
        if let (Some(problem), Some(buttons)) = (self.problem, Self::rect(SIGN_IN, measure)) {
            let y = buttons.y + buttons.h + theme::space::MD;
            plx_ui::text_view::TextView::new(problem.to_str().unwrap_or_default(), theme::size::BODY, theme::TEXT_SECONDARY)
                .with_measure(measure)
                .draw(p, Rect::new(buttons.x, y, Self::rect(SERVER, measure).map_or(buttons.w, |r| r.w), theme::size::BODY as f32 * 3.0));
        }
    }

    /// The form's part of the screen's logical state. Lengths only: what a person typed is theirs,
    /// and the password is never written anywhere but its request.
    pub(in crate::login) fn digest(&self) -> (u32, u64, u64, u64, bool) {
        (
            self.editing.unwrap_or(0),
            self.server.text().len() as u64,
            self.username.text().len() as u64,
            self.password.text().len() as u64,
            self.problem.is_some(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_sign_in_needs_an_address_and_a_username_but_not_a_password() {
        let mut form = Form::new("");
        assert!(form.submit(false).is_none());
        assert_eq!(form.problem, Some(msg::browse_jf_need_server_c()));
        form.set_editing(Some(SERVER));
        form.edit(&TextEdit::Commit("nas.local".into()));
        assert!(form.problem.is_none(), "an edit clears the refusal");
        assert!(form.submit(false).is_none());
        assert_eq!(form.problem, Some(msg::browse_jf_need_username_c()));
        form.set_editing(Some(USERNAME));
        form.edit(&TextEdit::Commit("alice".into()));
        let request = form.submit(false).expect("an empty password is Jellyfin's to judge");
        assert_eq!(request.server, "nas.local");
        assert_eq!(request.method, SignInMethod::Password { username: "alice".into(), password: String::new() });
    }

    #[test]
    fn quick_connect_needs_only_the_address_and_keeps_its_prefill() {
        let mut form = Form::new("192.168.1.20:8096");
        let request = form.submit(true).expect("the found server is already there");
        assert_eq!((request.server.as_str(), request.method), ("192.168.1.20:8096", SignInMethod::QuickConnect));
    }

    #[test]
    fn typed_text_goes_to_the_field_being_edited_and_the_password_is_secret() {
        let mut form = Form::new("");
        assert_eq!(form.text_entry(), TextEntry::None);
        assert!(form.set_editing(Some(PASSWORD)));
        assert!(!form.set_editing(Some(PASSWORD)), "no change, no keyboard effect");
        assert_eq!(form.text_entry(), TextEntry::Secret);
        form.edit(&TextEdit::Commit("hunter2".into()));
        assert_eq!(form.password.text(), "hunter2");
        assert_eq!(form.username.text(), "");
        assert!(form.set_editing(Some(SIGN_IN)) || form.editing().is_none(), "a button is not a field");
        assert_eq!(form.editing(), None);
    }

    #[test]
    fn the_logical_state_holds_lengths_never_what_was_typed() {
        let mut form = Form::new("nas.local");
        form.set_editing(Some(PASSWORD));
        form.edit(&TextEdit::Commit("hunter2".into()));
        assert_eq!(form.digest(), (PASSWORD, 9, 0, 7, false));
    }

    #[test]
    fn focus_opens_on_the_first_thing_left_to_type() {
        assert_eq!(Form::new("").seat(), SERVER);
        assert_eq!(Form::new("nas.local:8096").seat(), USERNAME);
    }
}
