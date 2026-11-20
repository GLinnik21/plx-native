//! Where the account's canonical state lives: the platform's storage helper, or plain files.
//!
//! The session record, the install's preferences and the consent record each have one canonical
//! store, and the platform states which. On a webOS television it is the LS2/DB8 storage helper
//! ([`Authority::Helper`]). Everywhere else it is the app's own record files under its persistent
//! state root ([`Authority::Files`]): the desktop simulator, a host test, and a platform with no
//! helper at all (Tizen).
//!
//! Under `Helper` the app's own files are not the store. They are the legacy migration inputs an
//! older build left, and the marked fallback the session layer writes when a commit to the helper is
//! not durable (a helper outage): while the helper cannot answer, that file is served as transient
//! state, and it never replaces the helper's record.
//!
//! The platform states the value once, as the `authority` of its [`StorageHooks`](super::StorageHooks),
//! and [`authority`] reads it. A process that installed no platform (a host test) reads
//! [`StorageHooks::NONE`], which is `Files`, like every other group's host-test answer. A shipping
//! process has to install its services before anything persists, because an uninstalled television
//! would be told it has no helper; `run_application` installs them as its first statement
//! (`port::tests` pins the order).
//!
//! The value exists so that the layers that persist these records can choose their canonical store
//! from what the platform states, not from the build's CPU and OS (a 32-bit ARM Linux binary is not
//! necessarily a television).
//!
//! This group is the home of the platform's other storage statements as well. The helper's wake-up
//! (`storage::client::install_activator`, which `port::plex_main` installs for webOS today) belongs
//! here as a later member and moves with the os lane after W7b; it is not moved yet.

/// Where the account's canonical state lives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Authority {
    /// The platform's storage helper (webOS: the LS2/DB8 service) is the canonical store. The app's
    /// own files are only the legacy migration inputs and the session layer's marked fallback, which
    /// is written during a helper outage and is never the record.
    Helper,
    /// The app's own record files under its persistent state root are the canonical store.
    #[default]
    Files,
}

/// The storage group of [`Services`](super::Services): what a platform states about where the app's
/// own records live.
pub struct StorageHooks {
    pub authority: Authority,
}

impl StorageHooks {
    /// A platform with no storage helper: the app keeps its records in files.
    pub const NONE: Self = Self { authority: Authority::Files };
}

/// The installed platform's [`Authority`], or `Files` when nothing is installed.
pub fn authority() -> Authority {
    super::services().storage.authority
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_authority_is_files() {
        assert_eq!(Authority::default(), Authority::Files);
        assert_eq!(StorageHooks::NONE.authority, Authority::Files, "a platform with no helper keeps files");
    }

    /// Nothing is installed in a test process, so the reader answers the host-test group's `Files`.
    #[test]
    fn an_uninstalled_process_reads_files() {
        assert_eq!(authority(), Authority::Files);
    }
}
