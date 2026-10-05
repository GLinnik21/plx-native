//! **Why the servers of one failed sign-in or profile switch could not be used** — the per-server
//! [`Cause`] tally the failure read-out and the events log are both built from.
//!
//! One entry per server that no route verified, keyed by its `machine_id`, so a later stage that
//! asks the person to approve something about a server can name exactly the servers whose verdict
//! was [`Cause::TlsUntrusted`] and the X509 verify result each one carried. The machine id is the
//! server's identity within this process and its events line never carries it: [`FailureCauses::log_line`]
//! names counts and verify numbers only.
use plx_plex::plex::probe::Cause;

/// One server's cause.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ServerCause {
    machine_id: String,
    cause: Cause,
}

/// The causes of every server a failed flow could not use, in the order they were probed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FailureCauses {
    servers: Vec<ServerCause>,
}

impl FailureCauses {
    /// Record `cause` for `machine_id`. One entry per machine: a second one for the same machine
    /// (a re-probe after an admission refusal) replaces the first.
    pub fn push(&mut self, machine_id: &str, cause: Cause) {
        match self.servers.iter_mut().find(|s| s.machine_id == machine_id) {
            Some(existing) => existing.cause = cause,
            None => self.servers.push(ServerCause { machine_id: machine_id.to_owned(), cause }),
        }
    }

    pub fn is_empty(&self) -> bool { self.servers.is_empty() }

    /// The servers whose certificate chain this television could not verify, with the X509 verify
    /// result each carried — what a user-approval alert is attached to.
    pub fn untrusted(&self) -> impl Iterator<Item = (&str, u8)> {
        self.servers.iter().filter_map(|s| match s.cause {
            Cause::TlsUntrusted { verify } => Some((s.machine_id.as_str(), verify)),
            _ => None,
        })
    }

    fn count(&self, matches: impl Fn(Cause) -> bool) -> usize {
        self.servers.iter().filter(|s| matches(s.cause)).count()
    }

    pub fn unreachable(&self) -> usize { self.count(|c| c == Cause::Unreachable) }
    pub fn tls_untrusted(&self) -> usize { self.count(|c| matches!(c, Cause::TlsUntrusted { .. })) }
    pub fn unauthorized(&self) -> usize { self.count(|c| c == Cause::Unauthorized) }

    /// The cause a PROFILE SWITCH words its failure by: a certificate this television does not trust
    /// outranks a refusal, which outranks silence — the first is the one a person can act on and
    /// the other two are what it would otherwise be mistaken for. `None` when nothing is recorded.
    /// (Sign-in discovery orders them differently — a 401 first — and does so in
    /// `resolved_without_roster`, where the refusal is already a flag of its own.)
    pub fn worst(&self) -> Option<Cause> {
        let first_untrusted = self.untrusted().next().map(|(_, verify)| Cause::TlsUntrusted { verify });
        first_untrusted
            .or_else(|| (self.unauthorized() > 0).then_some(Cause::Unauthorized))
            .or_else(|| (self.unreachable() > 0).then_some(Cause::Unreachable))
    }

    /// The one events-log line for a failure none of whose servers verified:
    /// `auth: no server verified — unreachable=1 tls_untrusted=2 unauthorized=0 verify=[20,20]`.
    /// Counts and X509 verify numbers only — no profile or server name, host, address or token.
    pub fn log_line(&self) -> String {
        let verify: Vec<String> = self.untrusted().map(|(_, v)| v.to_string()).collect();
        format!(
            "auth: no server verified \u{2014} unreachable={} tls_untrusted={} unauthorized={} verify=[{}]",
            self.unreachable(),
            self.tls_untrusted(),
            self.unauthorized(),
            verify.join(","),
        )
    }
}
