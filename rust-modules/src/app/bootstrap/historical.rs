//! Diagnostic-only identity of reviewed immutable historical recordings. Never replay authority.
use std::{collections::BTreeMap, io::Read, path::Path};
use plx_ui::rec::{Recording, CAP_BYTES};

pub(super) const REFUSAL: &str = "incompatible owner-input/presentation state; requires V13 capture";
const INVALID: &str = "invalid or incompatible recording";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    schema: u32,
    state_fp: u64,
    sha256: BTreeMap<String, String>,
}

/// Called only after the current-profile loader refused. Parse the entire bounded historical
/// stream, then authenticate its exact raw bytes. Mutations and unknown profiles stay generic.
pub(super) fn refusal(path: &Path, error: plx_ui::rec::RecError) -> &'static str {
    let plx_ui::rec::RecError::StateShape { theirs, .. } = error else { return INVALID };
    if !matches!(theirs, 4239819007634745513 | 13512213413816145485) { return INVALID; }
    let Ok(recording) = Recording::load(path, theirs) else { return INVALID };
    let profiles: Vec<Profile> = serde_json::from_str(include_str!("../../../../tests/replay-historical-profiles.json"))
        .expect("reviewed historical recording metadata");
    let Ok(hashes) = hashes(path) else { return INVALID };
    if profiles.iter().any(|profile| profile.schema == recording.header.schema
        && profile.state_fp == theirs && profile.sha256 == hashes) { REFUSAL } else { INVALID }
}

fn hashes(path: &Path) -> std::io::Result<BTreeMap<String, String>> {
    let mut hashes = BTreeMap::new();
    let mut remaining = CAP_BYTES;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| std::io::Error::other("recording filename"))?;
        if name != "manifest.json" && !(name.starts_with("rec-") && name.ends_with(".jsonl")) { continue; }
        let mut bytes = Vec::new();
        std::fs::File::open(entry.path())?.take((remaining + 1) as u64).read_to_end(&mut bytes)?;
        remaining = remaining.checked_sub(bytes.len()).ok_or_else(|| std::io::Error::other("recording cap"))?;
        let digest = plx_base::sha256::sha256(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        hashes.insert(name, digest);
    }
    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exact_reviewed_historical_bytes_receive_the_typed_diagnostic() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/replay");
        for name in ["1-boot-home-chip-grid", "6-settings-family", "12-filmography-detail-return", "12-filmography-detail-return-current"] {
            let path = root.join(name);
            let error = Recording::load(&path, super::super::super::recorder::state_fp()).err().unwrap();
            assert_eq!(refusal(&path, error), REFUSAL);
            assert_eq!(refusal(&path, plx_ui::rec::RecError::StateShape { theirs: 7, ours: 8 }), INVALID);
        }
        let temp = std::env::temp_dir().join(format!("plx-historical-refusal-{}", std::process::id()));
        std::fs::create_dir(&temp).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
        let _cleanup = Cleanup(temp.clone());
        let original = root.join("1-boot-home-chip-grid");
        std::fs::copy(original.join("manifest.json"), temp.join("manifest.json")).unwrap();
        std::fs::copy(original.join("rec-0000.jsonl"), temp.join("rec-0000.jsonl")).unwrap();
        let error = || plx_ui::rec::RecError::StateShape { theirs: 13512213413816145485, ours: 0 };
        assert_eq!(refusal(&temp, error()), REFUSAL, "directory identity is irrelevant");
        let manifest = std::fs::read(temp.join("manifest.json")).unwrap();
        for mutated in [manifest[..manifest.len()/2].to_vec(), [manifest.as_slice(), b" "].concat()] {
            std::fs::write(temp.join("manifest.json"), mutated).unwrap();
            assert_eq!(refusal(&temp, error()), INVALID, "even valid whitespace changes are unreviewed bytes");
        }
        std::fs::write(temp.join("manifest.json"), manifest).unwrap();
        std::fs::write(temp.join("rec-0000.jsonl"), b"{\"t\":\"tick\"").unwrap();
        assert_eq!(refusal(&temp, error()), INVALID, "truncated segments are not expected refusals");
    }
}
