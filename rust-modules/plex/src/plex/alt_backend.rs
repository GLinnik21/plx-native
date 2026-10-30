//! FinNative: the questions shared code asks about the installed backend. Each answers as Plex
//! does unless an alternative backend (`backend.rs`, the `alt-backend` feature) is installed.

/// Another kind of media server behind the `Client` surface. Only the `alt-backend` feature
/// compiles it.
#[cfg(feature = "alt-backend")]
#[path = "backend.rs"]
pub mod backend;

/// Hand a `Client` method's call to the installed backend: `alt!(b => b.sections());` at the top of
/// `Client::sections` returns the backend's answer when one is installed. Without the
/// `alt-backend` feature it expands to nothing.
#[macro_export]
#[doc(hidden)]
macro_rules! alt {
    ($b:ident => $call:expr) => {
        #[cfg(feature = "alt-backend")]
        if let Some($b) = $crate::plex::backend::active() {
            return $call;
        }
    };
}

/// Hand a `Client` method's call to the installed backend when it has an answer: `alt_some!(b =>
/// b.direct_play_url(..))` returns the backend's `Some`, and a `None` (or no backend) goes on to
/// the Plex code below it.
#[macro_export]
#[doc(hidden)]
macro_rules! alt_some {
    ($b:ident => $call:expr) => {
        #[cfg(feature = "alt-backend")]
        if let Some(answer) = $crate::plex::backend::active().and_then(|$b| $call) {
            return answer;
        }
    };
}

/// Whether an alternative backend answers instead of Plex.
pub fn alt_installed() -> bool {
    #[cfg(feature = "alt-backend")]
    if backend::active().is_some() {
        return true;
    }
    false
}

/// Whether a listing row may lack its file (`Part.key`) and still be played: the route then looks
/// the item up by its key. A Plex listing always carries the part, so an empty one there means
/// nothing playable; an alternative backend's listings may leave media out to stay small.
pub fn rows_resolve_by_key() -> bool {
    alt_installed()
}

/// The signed-in user's name and avatar path from an alternative backend, for the account chip
/// when no Plex Home profile is active; `None` on Plex. Cheap: no request.
pub fn signed_in_user() -> Option<(String, String)> {
    #[cfg(feature = "alt-backend")]
    if let Some(b) = backend::active() {
        return b.signed_in_user();
    }
    None
}

/// See [`backend::Backend::signed_in_generation`]; always 0 on Plex. One atomic read.
pub fn signed_in_generation() -> u32 {
    #[cfg(feature = "alt-backend")]
    if let Some(b) = backend::active() {
        return b.signed_in_generation();
    }
    0
}

/// Whether the signed-in account has plex.tv's Plex Home behind it: the profile roster and every
/// profile's own token come from plex.tv. An alternative backend has no plex.tv account (its
/// sign-in stores a marker, not a credential), so it has no profiles of plex.tv's to switch between;
/// its own users are [`can_switch_profiles`]'.
pub fn has_plex_home() -> bool {
    !alt_installed()
}

/// Whether *Change profile* can switch between the people signed in on this set, with `roster`
/// of them saved: a Plex Home's profiles, or an alternative backend's own users, each a sign-in of
/// its own that "Add user" saved beside the others. One saved user has nobody to switch to.
pub fn can_switch_profiles(roster: usize) -> bool {
    !alt_installed() || roster > 1
}

/// Whether the account menu offers *Add user*: another person signed in beside the saved ones.
/// Only an alternative backend; a Plex Home's members come from plex.tv.
pub fn can_add_users() -> bool {
    alt_installed()
}

/// Whether the server can run the fixed-HLS transcode the Auto quality controller adapts with.
/// Every Plex server can; an alternative backend cannot in the shape the player's HLS reader
/// takes, and the route then keeps to progressive Matroska.
pub fn fixed_hls_available() -> bool {
    !alt_installed()
}

/// `rating_key`'s favourite mark, as last read or pressed; `None` when the server keeps no
/// favourites (PMS keeps none per user), and the item menu and the detail page offer no toggle.
pub fn favorite_mark(rating_key: &str) -> Option<bool> {
    crate::alt!(b => b.favorite_mark(rating_key));
    let _ = rating_key;
    None
}

/// Make `rating_key` a favourite, or not, where the server keeps them: the mark flips at once and
/// the server's answer arrives later; a refusal flips it back. Never blocks the caller.
pub fn set_favorite_mark(rating_key: &str, favourite: bool) {
    crate::alt!(b => b.set_favorite_mark(rating_key, favourite));
    let _ = (rating_key, favourite);
}

/// Has a favourite write landed since the last call? FRAME THREAD: the app then re-reads Home,
/// whose Favorites shelf is the server's own listing.
pub fn take_favorite_landed() -> bool {
    crate::alt!(b => b.take_favorite_landed());
    false
}
