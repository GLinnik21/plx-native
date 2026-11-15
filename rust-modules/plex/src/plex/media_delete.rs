//! Server-owned media deletion. The caller verifies ownership; PMS enforces the active token's
//! permission. A missing response is uncertain, so the caller must never automatically retry.
use super::{Client, ClientKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteResult {
    Deleted,
    Forbidden,
    Missing,
    Unavailable,
    IdentityChanged,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn server(allow: serde_json::Value, status: Option<u16>, rotate: bool)
        -> (Arc<Client>, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let client = Arc::new(Client::new(super::super::ServerId::UNSET, "fixture",
            super::super::Origin::http("127.0.0.1", listener.local_addr().unwrap().port() as i32), "", "delete-fixture"));
        let captured = client.clone();
        let worker = std::thread::spawn(move || {
            let mut requests = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(3);
            let count = if status.is_some() && !rotate { 2 } else { 1 };
            while requests.len() < count && Instant::now() < deadline {
                let Ok((mut socket, _)) = plx_base::testnet::accept(&listener) else {
                    std::thread::sleep(Duration::from_millis(5)); continue;
                };
                socket.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut buf = [0; 1024];
                    let n = socket.read(&mut buf).unwrap();
                    if n == 0 { break; }
                    request.extend_from_slice(&buf[..n]);
                }
                let root = requests.is_empty();
                requests.push(String::from_utf8(request).unwrap());
                if root && rotate { captured.set_token("replacement"); }
                let body = if root { serde_json::json!({"MediaContainer":{"allowMediaDeletion":allow}}).to_string() } else { String::new() };
                let code = if root { 200 } else { status.unwrap() };
                write!(socket, "HTTP/1.1 {code} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            requests
        });
        (client, worker)
    }

    #[test]
    fn deletion_keeps_http_outcomes_and_sends_one_bodyless_delete() {
        for (status, expected) in [(200, DeleteResult::Deleted), (204, DeleteResult::Deleted),
            (403, DeleteResult::Forbidden), (404, DeleteResult::Missing),
            (503, DeleteResult::Unavailable), (302, DeleteResult::Unavailable)] {
            let (client, worker) = server(serde_json::json!("1"), Some(status), false);
            assert_eq!(client.delete_metadata("42", ClientKey::of(&client)), expected);
            let requests = worker.join().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(requests[0].starts_with("GET /?X-Plex-Token= "));
            assert!(requests[1].starts_with("DELETE /library/metadata/42?X-Plex-Token= "));
            assert!(requests[1].ends_with("\r\n\r\n"));
        }
    }

    #[test]
    fn unknown_or_disabled_permission_never_sends_delete() {
        for allow in [serde_json::Value::Null, serde_json::json!(false), serde_json::json!("0")] {
            let (client, worker) = server(allow, None, false);
            assert_eq!(client.delete_metadata("42", ClientKey::of(&client)), DeleteResult::Forbidden);
            assert_eq!(worker.join().unwrap().len(), 1);
        }
    }

    #[test]
    fn a_credential_change_during_permission_read_cancels_the_write() {
        let (client, worker) = server(serde_json::json!(true), None, true);
        assert_eq!(client.delete_metadata("42", ClientKey::of(&client)), DeleteResult::IdentityChanged);
        assert_eq!(worker.join().unwrap().len(), 1);
    }

    #[test]
    fn only_one_numeric_rating_key_is_accepted() {
        let client = Client::new(super::super::ServerId::UNSET, "fixture", super::super::Origin::http("127.0.0.1", 9), "", "delete-fixture");
        for key in ["", "1,2", "../1", "42?all=1", "1/2"] {
            assert_eq!(client.delete_metadata(key, ClientKey::of(&client)), DeleteResult::Forbidden);
        }
        let key = ClientKey::of(&client);
        client.set_token("changed");
        assert_eq!(client.delete_metadata("42", key), DeleteResult::IdentityChanged);
    }
}

impl Client {
    /// Delete one item and every media version beneath it. A show/season includes its episodes.
    /// Re-read permission before writing, and bind the write to the confirmation's credential.
    pub fn delete_metadata(&self, rk: &str, key: ClientKey) -> DeleteResult {
        if rk.is_empty() || !rk.bytes().all(|b| b.is_ascii_digit()) {
            return DeleteResult::Forbidden;
        }
        if ClientKey::of(self) != key { return DeleteResult::IdentityChanged; }
        let Some(root) = self.server_root() else { return DeleteResult::Unavailable; };
        if !root.allow_media_deletion.is_some_and(|v| v != 0) { return DeleteResult::Forbidden; }
        if ClientKey::of(self) != key { return DeleteResult::IdentityChanged; }
        match self.delete_status(&format!("/library/metadata/{rk}"), key) {
            Some(200..=299) => DeleteResult::Deleted,
            Some(401 | 403) => DeleteResult::Forbidden,
            Some(404) => DeleteResult::Missing,
            _ => DeleteResult::Unavailable,
        }
    }
}
