//! The query capsule, its caret, and the scope line beside it.
//!
//! **STUB** — signature final, body to come.
//!
//! ## The design, so it does not have to be re-derived from the mock
//!
//! - The field carries **no magnifier**. The pill above it is the mark, and repeating it here says
//!   nothing the screen has not already said.
//! - Focused (or editing) it wears the one control fill, `theme::ACCENT`, with `ACCENT_INK` on it.
//!   Holding a query while focus is elsewhere it drops to `CONTROL_IDLE_FILL`/`CONTROL_IDLE_INK`,
//!   so the query stays legible without claiming focus it does not have.
//! - The placeholder is **unconditional**: an empty focused field is the word "Search" in the
//!   on-accent hint ink (`ROW_VALUE_INK_ON_DIM`), never a blank capsule with a lone caret.
//! - The run clips from the **LEFT** — a field shows the TAIL of the string, so the caret end
//!   stays on screen. There is no text cursor: no pointer, no selection, no click-to-position.
//! - The caret is 3px × 34px and **solid**. Nothing pulses in this app, and a blinking caret would
//!   hold the GL loop awake forever — `ui::idle` cannot see a clock-driven animation, so a blink
//!   would either freeze or defeat the whole idle gate. Drawn only while `editing`.
//! - The **scope line** sits at x=950 on the field's own row, CAPTION/tertiary, and is stated only
//!   when the scope is smaller than the user's whole account: one plain fact, no warning tint and
//!   no Retry (the retry that re-kicks one source lives on that section's Library).
#![allow(dead_code)]

use crate::ui::search::View;
use crate::ui::Painter;

pub(crate) fn draw(_p: Painter, _v: &View) {}
