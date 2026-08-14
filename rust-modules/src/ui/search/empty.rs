//! The two states with no shelves: nothing searched yet, and nothing found.
//!
//! **STUB** — signature final, body to come.
//!
//! Both are **statements, not failures**: no alert mark, no danger tint, no Retry. Nothing found
//! is an answer the server gave, and dressing it as an error tells the user something untrue about
//! their library. A HEADLINE line over a CAPTION/tertiary hint, at [`super::CONTENT_TOP`].
//!
//! The nothing-searched-yet copy exists for a reason worth keeping: an empty screen with no words
//! on it reads as a screen that failed to draw.
//!
//! The hint names the scope — "across both sources" or "in <server>" — and so is the one place
//! this screen states what it is searching when the field's scope line is not shown.
#![allow(dead_code)]

use crate::ui::search::View;
use crate::ui::Painter;

pub(crate) fn draw(_p: Painter, _v: &View) {}
