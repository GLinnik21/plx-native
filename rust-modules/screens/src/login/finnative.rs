//! FinNative: the manual sign-in form's life cycle. Open from *Manual sign-in*; BACK closes it
//! (focus returns to the button that opened it); *Sign in* / *Use Quick Connect* hand the request to
//! the backend's one-shot slot and start a fresh sign-in, which this screen then reads out as any
//! other: the Quick Connect code, or the failure with *Try again* and *Manual sign-in* again.

use super::*;

/// The manual sign-in form itself.
#[path = "form.rs"]
pub(super) mod form;

/// *Manual sign-in*, beside the read-out's primary — it opens [`form::Form`].
pub(super) const MANUAL: u32 = 5;
/// The manual sign-in form's fields and buttons, the only group while it is open.
pub(super) const FORM_GROUP: GroupId = GroupId(2);

impl LoginScreen {
    /// Whether the read-out offers *Manual sign-in* — on the QR screen, and beside a failure's
    /// *Try again* (most of FinNative's failures are a server the network did not answer with,
    /// which the form is the way past).
    pub(super) fn manual_offered(&self) -> bool {
        self.persistence_warning.is_none() && matches!(self.phase, Phase::Waiting | Phase::Error)
    }

    pub(super) fn form_digest(&self) -> Option<(u32, u64, u64, u64, bool)> {
        self.form.as_ref().map(form::Form::digest)
    }

    /// The QR screen's *Manual sign-in*, on the narrative column's action band.
    pub(super) fn waiting_manual(measure: &dyn Measure) -> Rect {
        let w = Button::pill_w_measured(plx_platform::i18n::msg::browse_jf_manual_c(), theme::size::BODY, false, None, measure);
        RouteLayout::screen().action_pair(w, 0.0).0
    }

    /// The one group the open form is: its fields and buttons.
    pub(super) fn form_groups<H: AppLike>(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: FORM_GROUP,
            kind: GroupKind::Free,
            seat: Seat::First,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: form::Form::extent(cx.measure),
            len: form::ELEMS.len(),
            elem: ElemKind::Bare,
        });
    }

    /// Where one of the open form's stops sits.
    pub(super) fn form_place<H: AppLike>(&self, key: &u32, cx: &Cx<'_, H>) -> Option<Placed> {
        let index = form::ELEMS.iter().position(|e| e == key)?;
        let rect = form::Form::stop_rect(*key, cx.measure)?;
        Some(Placed { rect, rest_rect: rect, clip: Rect::FULL, index: Some(index as u32) })
    }

    pub(super) fn open_form<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        let f = form::Form::new(&plx_jellyfin::jellyfin::backend::server_hint());
        let seat = f.seat();
        self.form = Some(f);
        Self::enter_elem(fx, self.key(seat));
    }

    pub(super) fn keyboard<H: AppLike>(fx: &mut Effects<'_, H>, up: bool) {
        let me = fx.from();
        fx.push(Fx::Deliver(me, Delivery::Keyboard { up }));
    }

    /// Put the keyboard up for `field`, or take it down (`None`), sending the keyboard effect only
    /// when whether it is up actually changes.
    pub(super) fn form_keyboard<H: AppLike>(&mut self, field: Option<u32>, fx: &mut Effects<'_, H>) {
        let Some(f) = self.form.as_mut() else { return };
        let was_up = f.editing().is_some();
        if f.set_editing(field) {
            let up = f.editing().is_some();
            if up != was_up {
                Self::keyboard(fx, up);
            }
        }
        fx.invalidate(plx_machine::present::Provenance::Input);
    }

    pub(super) fn close_form<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        self.form_keyboard(None, fx);
        self.form = None;
        if self.row().position(MANUAL).is_some() {
            Self::enter_elem(fx, self.key(MANUAL));
        } else {
            Self::enter_group(fx, CONTROL_GROUP);
        }
        self.sync_state();
    }

    pub(super) fn submit_form<H: AppLike>(&mut self, quick: bool, fx: &mut Effects<'_, H>) {
        let Some(request) = self.form.as_mut().and_then(|f| f.submit(quick)) else {
            fx.invalidate(plx_machine::present::Provenance::Input);
            self.sync_state();
            return;
        };
        plx_base::eventlog::log(if quick {
            "login: manual sign-in with Quick Connect"
        } else {
            "login: manual sign-in with a username and password"
        });
        plx_jellyfin::jellyfin::backend::request_sign_in(request);
        self.form_keyboard(None, fx);
        self.form = None;
        fx.push(Fx::App(AppFx::Session(auth::SessionCmd::StartLogin)));
        self.sync_state();
    }

    /// The form's share of `step`, run first while it is open: `Some` when it handled the event,
    /// `None` to let the screen's ordinary handling see it (ticks, replies, teardown).
    pub(super) fn step_form<H: AuthLike>(
        &mut self,
        ev: &ScreenEvent<H>,
        cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) -> Option<Handled> {
        let editing = self.form.as_ref()?.editing();
        match ev {
            ScreenEvent::FocusMoved { to, .. } => {
                if editing.is_some_and(|e| e != to.elem) {
                    self.form_keyboard(None, fx);
                }
                self.sync_state();
                Some(Handled::Yes)
            }
            ScreenEvent::Activate(elem) => {
                match *elem {
                    form::SIGN_IN => self.submit_form(false, fx),
                    form::QUICK_CONNECT => self.submit_form(true, fx),
                    e if form::Form::is_field(e) => {
                        // OK on a field brings the keyboard up for it; OK again takes it down.
                        self.form_keyboard(if editing == Some(e) { None } else { Some(e) }, fx);
                        self.sync_state();
                    }
                    _ => {}
                }
                Some(Handled::Yes)
            }
            ScreenEvent::Input(input) => match &input.kind {
                InputKind::SystemKeyboard(up) => {
                    // The television closed its keyboard itself (its own hide key): follow it.
                    if !*up {
                        if let Some(f) = self.form.as_mut() {
                            f.set_editing(None);
                        }
                        self.sync_state();
                    }
                    Some(Handled::Yes)
                }
                InputKind::Text(edit) => {
                    if editing.is_some() {
                        if let Some(f) = self.form.as_mut() {
                            f.edit(edit);
                        }
                        fx.invalidate(plx_machine::present::Provenance::Input);
                        self.sync_state();
                    }
                    Some(Handled::Yes)
                }
                InputKind::Key { key, sym, edge, .. } if *edge != Edge::Up => {
                    if editing.is_some() {
                        let edit = match (*key, *sym) {
                            (Key::Left, _) => Some(plx_machine::machine::TextEdit::Left),
                            (Key::Right, _) => Some(plx_machine::machine::TextEdit::Right),
                            (_, plx_ui::consts::SDLK_BACKSPACE) => Some(plx_machine::machine::TextEdit::Backspace),
                            (_, plx_ui::consts::SDLK_CLEAR) => Some(plx_machine::machine::TextEdit::Clear),
                            _ => None,
                        };
                        if let Some(edit) = edit {
                            if let Some(f) = self.form.as_mut() {
                                f.edit(&edit);
                            }
                            fx.invalidate(plx_machine::present::Provenance::Input);
                            self.sync_state();
                            return Some(Handled::Yes);
                        }
                        if matches!(key, Key::Back | Key::Ok) {
                            self.form_keyboard(None, fx);
                            self.sync_state();
                            return Some(Handled::Yes);
                        }
                        if matches!(key, Key::Up | Key::Down) {
                            // Leaving the field takes the keyboard down; the engine moves focus.
                            self.form_keyboard(None, fx);
                            self.sync_state();
                            return Some(Handled::No);
                        }
                        return Some(Handled::Yes);
                    }
                    if *key == Key::Back && *edge == Edge::Down {
                        self.close_form(fx);
                        return Some(Handled::Yes);
                    }
                    Some(Handled::No)
                }
                InputKind::Pointer { .. } | InputKind::Click { .. } => Some(Handled::No),
                _ => Some(Handled::Yes),
            },
            ScreenEvent::WillLeave(_) | ScreenEvent::Cover | ScreenEvent::Suspend => {
                self.form_keyboard(None, fx);
                None
            }
            ScreenEvent::Unmount => {
                self.form_keyboard(None, fx);
                None
            }
            _ => {
                let _ = cx;
                None
            }
        }
    }

    pub(super) fn form_neighbour(&self, elem: u32, dir: Dir) -> Option<u32> {
        use form::{PASSWORD, QUICK_CONNECT, SERVER, SIGN_IN, USERNAME};
        match (elem, dir) {
            (SERVER, Dir::Down) => Some(USERNAME),
            (USERNAME, Dir::Up) => Some(SERVER),
            (USERNAME, Dir::Down) => Some(PASSWORD),
            (PASSWORD, Dir::Up) => Some(USERNAME),
            (PASSWORD, Dir::Down) => Some(SIGN_IN),
            (SIGN_IN | QUICK_CONNECT, Dir::Up) => Some(PASSWORD),
            (SIGN_IN, Dir::Right) => Some(QUICK_CONNECT),
            (QUICK_CONNECT, Dir::Left) => Some(SIGN_IN),
            _ => None,
        }
    }

    pub(super) fn draw_form<H: AppLike>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, focus: Option<u32>) {
        let Some(form) = self.form.as_ref() else { return };
        form.draw(p, focus, f.press.scale, f.measure);
        for elem in form::ELEMS {
            if let Some(rect) = form::Form::stop_rect(elem, f.measure) {
                self.control_stop(f, p, elem, rect);
            }
        }
    }
}
