//! **`state:<nonce>`**: the accessibility read-back a UI test uses to learn what is on screen BY ID
//! (the a11y-ids plan, D7 and D8). A harness writes `state:<nonce>` to the remote FIFO; at the end of
//! the loop iteration the app projects the input owner (`Dispatcher::a11y_snapshot`), serializes it
//! with the frame number and the nonce, and replaces `plxnative-a11y.json` atomically. The reader
//! waits until the file's `nonce` is the one it sent (`tools/a11y.py`).
//!
//! **Where it sits.** The token is parsed in `ingress_token` BEFORE `dispatch_remote_token`, so it
//! notes no input damage and, being no key, is not recorded. The request waits in the app's
//! [`Pending`] slot, `App::a11y_pending` (the latest nonce wins); [`serve`] takes it at the frame tail
//! (`run.rs::report`), which both the presented and the skipped path reach, and asks for no frame:
//! like the focus probe it must never hold the idle gate open. On a skipped path the answer can lag by
//! one idle poll, which the reader's nonce wait absorbs. A full writer queue leaves the request
//! pending for the next iteration.
//!
//! **IO.** This module is the runner, not a machine: the projection is a `&self` read of the
//! dispatcher, the JSON is built here (`plx_ui` names no JSON type), and the file is written by ONE
//! `storage_worker` job (the `anim_diagnostics` recipe): remove a stale `plxnative-a11y.json.tmp`,
//! create the temp file 0600 with `create_new` (so a planted symlink is refused, never followed),
//! write it, rename it over the target. `plx_plex::plex::session::write_atomic` is not reused: its
//! temp name is random, and a temp a crash left behind could not be listed in `dev.rs`'s `DIAG`.
//! Both fixed names are in `DIAG` and in `paths::runtime_file::LOGS` (the file holds titles and
//! rating keys, so Delete all local data removes it).
//!
//! **Privacy.** An element's label and subject are runtime text. This module keeps them in the snapshot
//! and the state file only: the one function that logs takes a closed [`Note`], never snapshot text.
//! (A `press:<id>@<subject>` token is the harness's own text, and a recording keeps it: see
//! `plx_ui::a11y`.)
//!
//! **Builds.** The whole module is `devtriggers`: a `RELEASE=1` build does not parse the token and
//! never writes the file; `run.rs` names the module only behind the same cfg. Over Lab Control the
//! token is accepted in a non-RELEASE `LAB=1` build, and writes a file Lab cannot read.

use std::io::Write as _;
use std::path::Path;

use plx_base::paths::runtime_file::{A11Y_STATE, A11Y_STATE_TMP};
use plx_base::storage_worker::SubmitError;
use plx_ui::a11y::{A11ySnapshot, Role};
use serde_json::json;

use super::App;

/// The nonce grammar's width: `[A-Za-z0-9_-]{1,32}`.
const NONCE_BYTES: usize = 32;

/// The file's schema, so a reader can refuse a shape it does not know.
const SCHEMA: u32 = 1;

/// One request's nonce, held by value: fixed, `Copy` and ASCII by construction, so the pending slot
/// owns no heap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Nonce {
    len: u8,
    bytes: [u8; NONCE_BYTES],
}

impl Nonce {
    fn parse(text: &str) -> Option<Nonce> {
        let b = text.as_bytes();
        let valid = !b.is_empty()
            && b.len() <= NONCE_BYTES
            && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-');
        if !valid {
            return None;
        }
        let mut bytes = [0u8; NONCE_BYTES];
        bytes[..b.len()].copy_from_slice(b);
        Some(Nonce { len: b.len() as u8, bytes })
    }

    pub(super) fn as_str(&self) -> &str {
        // ASCII by `parse`; the fallback is unreachable.
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }
}

/// **The latest `state:` request not yet served**, the app's one slot (`App::a11y_pending`). The frame
/// thread parses a request into it (`ingress_token`) and serves it (`report`), so no thread shares it
/// and a test builds its own.
#[derive(Default)]
pub(super) struct Pending(Option<Nonce>);

impl Pending {
    /// **The ingress seam** (`run.rs::ingress_token`, before `dispatch_remote_token`): `Some(accepted)`
    /// when `token` is a `state:` token, `None` for every other token. It touches only this slot, so it
    /// records nothing and notes no damage. A valid nonce replaces any request still waiting (the
    /// latest wins); a malformed one is refused and leaves the waiting request alone.
    pub(super) fn intercept(&mut self, token: &str) -> Option<bool> {
        let nonce = token.strip_prefix("state:")?;
        match Nonce::parse(nonce) {
            Some(nonce) => {
                self.0 = Some(nonce);
                Some(true)
            }
            None => {
                plx_base::eventlog::log(Note::BadNonce.line());
                Some(false)
            }
        }
    }

    /// Take the pending request (the latest nonce), if any.
    pub(super) fn take(&mut self) -> Option<Nonce> {
        self.0.take()
    }

    /// Put a request back (the writer queue was full) unless a newer one has arrived meanwhile.
    fn keep_unless_newer(&mut self, nonce: Nonce) {
        if self.0.is_none() {
            self.0 = Some(nonce);
        }
    }
}

/// Every line this module may log: closed words, so no snapshot text can reach the event log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Note {
    /// A `state:` token whose nonce is not `[A-Za-z0-9_-]{1,32}` (the token is not echoed).
    BadNonce,
    /// The storage worker is stopped or cannot start: the request is dropped.
    WriterStopped,
}

impl Note {
    fn line(self) -> &'static str {
        match self {
            Note::BadNonce => "remote: refused state: token (the nonce is 1-32 of A-Z a-z 0-9 _ -)",
            Note::WriterStopped => "a11y: state not written (the storage worker is not running)",
        }
    }
}

/// The worker's failure line: an `ErrorKind` is a closed word, never a path or a title.
fn write_failed(kind: std::io::ErrorKind) -> String {
    format!("a11y: state write failed ({kind:?})")
}

/// **The frame tail's half** (`run.rs::report`): answer the pending request, if there is one. The
/// snapshot is read here, on the frame thread, and the file is written by one off-frame job.
pub(super) fn serve(app: &mut App) {
    plx_base::step::assert_io_allowed("a11y state runner");
    let Some(nonce) = app.a11y_pending.take() else { return };
    let App { runtime, bridge, pages, .. } = &mut *app;
    let snapshot = pages.a11y_snapshot(&mut runtime.bridge(bridge));
    let body = to_json(&snapshot, nonce.as_str()).into_bytes();
    // The instance root is captured on the app boundary, never inside the worker.
    let dir = plx_base::paths::runtime_dir();
    let job = move || {
        if let Err(error) = write_state(dir, &body) {
            plx_base::eventlog::log(&write_failed(error.kind()));
        }
    };
    match plx_base::storage_worker::submit(job) {
        // Dropping the ticket does not cancel accepted work.
        Ok(_) => {}
        // A full queue is transient: keep the request unless a newer one has arrived.
        Err(SubmitError::Full) => app.a11y_pending.keep_unless_newer(nonce),
        Err(SubmitError::Stopped | SubmitError::StartFailed) => {
            plx_base::eventlog::log(Note::WriterStopped.line());
        }
    }
}

/// The closed word of a role.
fn role_word(role: Role) -> &'static str {
    match role {
        Role::Card => "card",
        Role::Button => "button",
        Role::Row => "row",
        Role::Tab => "tab",
        Role::Field => "field",
        Role::Toggle => "toggle",
        Role::Text => "text",
    }
}

/// A layout float for the file: rounded to a hundredth, so a reader compares positions and the file
/// does not carry an `f32`'s tail. A non-finite value becomes `null` (serde_json's rule).
fn rounded(v: f32) -> f64 {
    (f64::from(v) * 100.0).round() / 100.0
}

/// The state file's text: the snapshot, the frame it was read at and the nonce a reader waits for.
/// A pure function of its arguments.
pub(super) fn to_json(snapshot: &A11ySnapshot, nonce: &str) -> String {
    let groups: Vec<_> = snapshot
        .groups
        .iter()
        .map(|g| {
            json!({
                "group": g.group,
                "prefix": g.prefix,
                "len": g.len,
                "role": g.role.map(role_word),
            })
        })
        .collect();
    let elements: Vec<_> = snapshot
        .elems
        .iter()
        .map(|e| {
            json!({
                "id": e.id,
                "group": e.group,
                "index": e.index,
                "role": role_word(e.role),
                "rect": e.rest_rect.map(|r| [rounded(r.x), rounded(r.y), rounded(r.w), rounded(r.h)]),
                "visible": e.visible,
                "focused": e.focused,
                "loaded": e.loaded,
                "label": e.label,
                "subject": e.subject,
            })
        })
        .collect();
    json!({
        "schema": SCHEMA,
        "nonce": nonce,
        "frame": snapshot.frame,
        "route": snapshot.route,
        "owner": snapshot.owner.word(),
        "ns": snapshot.ns,
        "focused": snapshot.focused,
        "groups": groups,
        "elements": elements,
    })
    .to_string()
}

/// Create the temp file: new (`O_EXCL`, which also refuses a symlink at the name, even a dangling
/// one) and readable by its owner only.
fn create_temp(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

/// **The atomic writer**: leave `dir` holding `plxnative-a11y.json` with exactly `body`, through the
/// fixed temp name. A stale temp (a crash between the write and the rename) is removed first; a
/// symlink there is removed, not followed. On any failure no partial file is left under either name
/// that was not already there. Runs inside the storage worker.
fn write_state(dir: &Path, body: &[u8]) -> std::io::Result<()> {
    plx_base::step::assert_io_allowed("a11y state writer");
    let tmp = dir.join(A11Y_STATE_TMP);
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut file = create_temp(&tmp)?;
    let written = file.write_all(body);
    drop(file);
    written
        .and_then(|()| std::fs::rename(&tmp, dir.join(A11Y_STATE)))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_ui::a11y::{ElemSnapshot, GroupSnapshot, OwnerKind};
    use plx_ui::Rect;

    fn snapshot() -> A11ySnapshot {
        let elem = |id: &str, index: usize, loaded: bool, focused: bool| ElemSnapshot {
            id: id.into(),
            group: 3,
            index,
            role: Role::Card,
            rest_rect: Some(Rect::new(300.0, 10.5, 280.0, 200.0)),
            visible: true,
            focused,
            loaded,
            label: loaded.then(|| "A \"quoted\"\nTitle".to_string()),
            subject: loaded.then(|| "12345".to_string()),
        };
        A11ySnapshot {
            frame: 42,
            route: Some("home"),
            owner: OwnerKind::Page,
            ns: Some("home"),
            focused: Some("home.shelf.0.card.1".into()),
            groups: vec![GroupSnapshot {
                group: 3,
                prefix: "home.shelf.0.card".into(),
                len: 2,
                role: Some(Role::Card),
            }],
            elems: vec![
                elem("home.shelf.0.card.0", 0, true, false),
                elem("home.shelf.0.card.1", 1, false, true),
            ],
        }
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(".plx-a11y-state-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// **The JSON names the focused id** and the nonce a reader waits for, lists every element with
    /// where it rests, and escapes a title the way JSON does.
    #[test]
    fn the_json_names_the_focused_id_and_the_nonce() {
        let text = to_json(&snapshot(), "n-1_a");
        let v: serde_json::Value = serde_json::from_str(&text).expect("the file is JSON");
        assert_eq!(v["schema"], 1);
        assert_eq!(v["nonce"], "n-1_a");
        assert_eq!(v["frame"], 42);
        assert_eq!((v["route"].as_str(), v["owner"].as_str(), v["ns"].as_str()), (Some("home"), Some("page"), Some("home")));
        assert_eq!(v["focused"], "home.shelf.0.card.1");
        assert_eq!(v["groups"][0]["prefix"], "home.shelf.0.card");
        assert_eq!(v["groups"][0]["role"], "card");
        let elements = v["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0]["id"], "home.shelf.0.card.0");
        assert_eq!(elements[0]["rect"], json!([300.0, 10.5, 280.0, 200.0]));
        assert_eq!(elements[0]["label"], "A \"quoted\"\nTitle");
        assert_eq!(elements[0]["subject"], "12345");
        assert_eq!(elements[1]["focused"], true);
        assert_eq!(elements[1]["loaded"], false);
        assert!(elements[1]["label"].is_null() && elements[1]["subject"].is_null());
    }

    /// An owner-less snapshot is valid JSON with nulls, not a missing key.
    #[test]
    fn an_empty_snapshot_keeps_every_key() {
        let empty = A11ySnapshot {
            frame: 0,
            route: None,
            owner: OwnerKind::None,
            ns: None,
            focused: None,
            groups: Vec::new(),
            elems: Vec::new(),
        };
        let v: serde_json::Value = serde_json::from_str(&to_json(&empty, "x")).unwrap();
        assert_eq!(v["owner"], "none");
        for key in ["route", "ns", "focused"] {
            assert!(v.get(key).is_some_and(serde_json::Value::is_null), "{key}");
        }
        assert_eq!(v["elements"], json!([]));
    }

    /// The grammar: 1..=32 of `[A-Za-z0-9_-]`; a `state:` token that breaks it is refused (and
    /// leaves the pending request alone), every other token is not this module's.
    #[test]
    fn a_state_token_is_intercepted_and_only_a_valid_nonce_is_kept() {
        let mut pending = Pending::default();
        assert_eq!(pending.intercept("down"), None);
        assert_eq!(pending.intercept("statefoo"), None, "no colon: not a state token");
        assert_eq!(pending.intercept("txt:state:abc"), None);
        assert_eq!(pending.take(), None);

        assert_eq!(pending.intercept("state:1"), Some(true));
        assert_eq!(pending.intercept("state:Ab_-9"), Some(true));
        assert_eq!(pending.take().as_ref().map(Nonce::as_str), Some("Ab_-9"), "the latest nonce wins");
        assert_eq!(pending.take(), None, "serving consumes it");

        let longest = format!("state:{}", "a".repeat(NONCE_BYTES));
        let too_long = format!("{longest}a");
        assert_eq!(pending.intercept(&longest), Some(true));
        let _ = pending.take();
        assert_eq!(pending.intercept("state:one"), Some(true));
        for bad in ["state:", too_long.as_str(), "state:a.b", "state:a b", "state:é", "state:a/b", "state:a;b"] {
            assert_eq!(pending.intercept(bad), Some(false), "{bad:?}");
        }
        assert_eq!(pending.take().as_ref().map(Nonce::as_str), Some("one"), "a refusal leaves the request");

        // a request the full writer queue handed back waits again, but never over a newer one
        let mut pending = Pending::default();
        let old = Nonce::parse("old").unwrap();
        pending.keep_unless_newer(old);
        assert_eq!(pending.take(), Some(old));
        pending.intercept("state:new");
        pending.keep_unless_newer(old);
        assert_eq!(pending.take().as_ref().map(Nonce::as_str), Some("new"));
    }

    /// **The writer leaves only the two fixed names**: after a write there is the state file and
    /// nothing else; a stale temp from a crash is replaced; a second write replaces the first.
    #[test]
    fn the_writer_leaves_only_the_fixed_names() {
        let dir = scratch("names");
        write_state(&dir, b"{\"nonce\":\"a\"}").unwrap();
        assert_eq!(names(&dir), [A11Y_STATE]);
        assert_eq!(std::fs::read(dir.join(A11Y_STATE)).unwrap(), b"{\"nonce\":\"a\"}");

        std::fs::write(dir.join(A11Y_STATE_TMP), b"half a write").unwrap();
        write_state(&dir, b"{\"nonce\":\"b\"}").unwrap();
        assert_eq!(names(&dir), [A11Y_STATE], "the stale temp is gone, not left beside it");
        assert_eq!(std::fs::read(dir.join(A11Y_STATE)).unwrap(), b"{\"nonce\":\"b\"}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_state_file_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("mode");
        write_state(&dir, b"{}").unwrap();
        let mode = std::fs::metadata(dir.join(A11Y_STATE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A symlinked temp file is refused, never followed**: creating over a planted link fails with
    /// `AlreadyExists` and the file it points at is untouched; a link planted BEFORE a write is
    /// removed (the link, not its target) and the write lands in a fresh file; a link at the target
    /// name is replaced by the rename, not written through.
    #[cfg(unix)]
    #[test]
    fn the_writer_refuses_a_symlinked_temp_and_never_follows_a_link() {
        use std::os::unix::fs::symlink;
        let dir = scratch("symlink");
        let victim = dir.join("victim");
        std::fs::write(&victim, b"precious").unwrap();

        let tmp = dir.join(A11Y_STATE_TMP);
        symlink(&victim, &tmp).unwrap();
        let refused = create_temp(&tmp).expect_err("O_EXCL refuses a symlink at the temp name");
        assert_eq!(refused.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");

        write_state(&dir, b"{\"nonce\":\"c\"}").unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious", "the link's target is not written through");
        assert_eq!(std::fs::read(dir.join(A11Y_STATE)).unwrap(), b"{\"nonce\":\"c\"}");
        assert_eq!(names(&dir), [A11Y_STATE, "victim"], "sorted: the temp link is gone, the target is a plain file");

        std::fs::remove_file(dir.join(A11Y_STATE)).unwrap();
        symlink(&victim, dir.join(A11Y_STATE)).unwrap();
        write_state(&dir, b"{\"nonce\":\"d\"}").unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious", "a link at the target name is replaced");
        assert_eq!(std::fs::read(dir.join(A11Y_STATE)).unwrap(), b"{\"nonce\":\"d\"}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A temp name that cannot be removed (here a non-empty directory) fails the write and writes
    /// nothing under the state name.
    #[test]
    fn a_temp_that_cannot_be_removed_fails_the_write_cleanly() {
        let dir = scratch("blocked");
        std::fs::create_dir(dir.join(A11Y_STATE_TMP)).unwrap();
        std::fs::write(dir.join(A11Y_STATE_TMP).join("keep"), b"x").unwrap();
        assert!(write_state(&dir, b"{}").is_err());
        assert!(!dir.join(A11Y_STATE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Labels never reach a log line.** The only lines this module can log are the closed
    /// [`Note`] words and an `ErrorKind`, and every `eventlog::log` call in the production text takes
    /// one of those: no call names a snapshot, a body or a label.
    #[test]
    fn nothing_but_closed_words_reaches_the_log() {
        for note in [Note::BadNonce, Note::WriterStopped] {
            assert!(note.line().is_ascii() && !note.line().contains("home"));
        }
        assert_eq!(write_failed(std::io::ErrorKind::PermissionDenied), "a11y: state write failed (PermissionDenied)");

        let source = include_str!("a11y_state.rs");
        let production = source.split("#[cfg(test)]\nmod tests").next().unwrap();
        let calls: Vec<&str> = production.lines().filter(|l| l.contains("eventlog::log(")).collect();
        assert_eq!(calls.len(), 3, "a new log call needs a closed-word argument and this test taught about it: {calls:?}");
        for call in calls {
            assert!(
                call.contains(".line()") || call.contains("write_failed("),
                "a log call that is not a closed word: {call}"
            );
        }
    }
}
