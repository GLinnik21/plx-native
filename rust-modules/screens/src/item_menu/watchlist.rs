//! The menu's Watchlist rows (plan §1.7-§1.9): Add / Remove on a card, an episode or a season,
//! and the membership-aware tail of the Catalog menu.
//!
//! Everything a row needs is a [`Face`] **captured when the menu opened** — the catalog id, the
//! membership as the store read it THEN, and what the subject is. Nothing here reads a store at
//! press time, so a list that changes under an open panel cannot re-point a row.
//!
//! **No face, no row.** The app hands a face only when the account's Watchlist can be reached
//! (plex.tv answered and the title's guid is known), so an unreachable service omits the row
//! rather than offering a press that can only fail. An UNKNOWN membership is not unreachable: it
//! wears the Add face, because the PUT is idempotent.
use std::convert::Infallible;

use plx_data::catalog::Membership;
use plx_data::watchlist::model::Kind;
use plx_ui::form::FormSection;
use plx_ui::form::RowKind;
use plx_ui::icons::Icon;
use plx_ui::table::Row;

use super::{Action, ItemRow};

/// What the menu is about, as the Watchlist knows it. Identity only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Face {
    /// The 12 bytes of the title's discover guid.
    pub hex: [u8; 12],
    /// The membership as read when the menu opened.
    pub member: Membership,
    /// The catalog kind the rows act on: a movie card is `Movie`; an episode or season acts on
    /// its `Show`.
    pub kind: Kind,
    /// The menu is opened on a leaf or season of the show, so the labels say "Show".
    pub from_episode: bool,
}

/// `plex://movie|show/<24 hex>`, the key the Remove write and the page's removal landing use.
pub fn guid_of(kind: Kind, hex: &[u8; 12]) -> String {
    plx_data::watchlist::guid_of(kind, hex)
}

/// The catalog id at the tail of a `plex://movie|show/<24 hex>` guid.
pub fn hex_of(guid: &str) -> Option<[u8; 12]> {
    plx_data::watchlist::decode_hex(guid.rsplit('/').next()?)
}

type Built = (ItemRow, Action, Row);

fn add_row(hex: [u8; 12], show_label: bool) -> Built {
    let label = if show_label {
        plx_platform::i18n::msg::watchlist_action_add_show()
    } else {
        plx_platform::i18n::msg::watchlist_action_add()
    };
    (ItemRow::WatchlistAdd, Action::WatchlistAdd { hex }, Row::new(label).licon(Icon::Bookmark))
}

fn remove_row(guid: String, show_label: bool) -> Built {
    let label = if show_label {
        plx_platform::i18n::msg::watchlist_action_remove_show()
    } else {
        plx_platform::i18n::msg::watchlist_action_remove()
    };
    (ItemRow::WatchlistRemove, Action::WatchlistRemove(guid), Row::new(label).licon(Icon::Close).destructive(true))
}

/// The one row a face adds to a card, episode or season menu: Remove when the title is on the
/// list, Add otherwise (an unknown membership included).
pub fn face_row(face: &Face) -> Built {
    match face.member {
        Membership::On => remove_row(guid_of(face.kind, &face.hex), face.from_episode),
        Membership::Off | Membership::Unknown => add_row(face.hex, face.from_episode),
    }
}

/// Add `face`'s row to `sec`, or leave it alone with no face (service unreachable).
pub fn push_face(
    sec: FormSection<ItemRow, Action, Infallible>, face: Option<&Face>,
) -> FormSection<ItemRow, Action, Infallible> {
    match face {
        Some(f) => {
            let (id, action, row) = face_row(f);
            sec.item(id, RowKind::Button, action, row)
        }
        None => sec,
    }
}

/// The Catalog menu's membership row for the title `guid`: Remove while it is on the list, Add
/// when it is not (or not known). `None` when the guid carries no catalog id.
pub fn catalog_row(member: Membership, guid: &str) -> Option<Built> {
    match member {
        Membership::On => Some(remove_row(guid.to_string(), false)),
        Membership::Off | Membership::Unknown => hex_of(guid).map(|hex| add_row(hex, false)),
    }
}

#[cfg(test)]
#[path = "watchlist_tests.rs"]
mod watchlist_tests;
