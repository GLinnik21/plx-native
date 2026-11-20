//! Where the account's canonical state lives: the platform's storage helper, or plain files.
//!
//! The session record, the install's preferences and the consent record each have two homes. On a
//! webOS television the LS2/DB8 storage helper owns them ([`Authority::Helper`]): the app never
//! reads or writes a file of its own for them. Everywhere else the app keeps them in files under its
//! persistent state root ([`Authority::Files`]): the desktop simulator, a host test, and a platform
//! with no helper at all (Tizen). The layers that persist these records (`plex::session`,
//! `telemetry`) choose their store from this value, once per operation, instead of from the build's
//! CPU and OS, which is what picked it before: a 32-bit ARM Linux binary is not necessarily a
//! television.
//!
//! The platform states it once, as the `authority` of its [`StorageHooks`](super::StorageHooks), and
//! [`authority`] reads it. A process that installed no platform (a host test) reads
//! [`StorageHooks::NONE`], which is `Files`, like every other group's host-test answer. A shipping
//! process must install its services before anything persists: an uninstalled television would read
//! `Files` and write a plaintext session file, which is why `run_application` installs them first
//! (`port::tests` pins the order).
//!
//! This group is the home of the platform's other storage statements as well. The helper's wake-up
//! (`storage::client::install_activator`, which `port::plex_main` installs for webOS today) belongs
//! here as a later member and moves with the os lane after W7b; it is not moved yet.

/// Where the account's canonical state lives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Authority {
    /// The platform's storage helper (webOS: the LS2/DB8 service). No file of the app's own holds
    /// the record, and a helper that cannot answer is never papered over with one.
    Helper,
    /// Files under the app's persistent state root, written by the app itself.
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
