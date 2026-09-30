//! Install-identity code generation, shared by the two build scripts that need it.
//!
//! `src/storage/state.rs` is compiled into BOTH the app library and the `plxnative-storage` helper
//! (the helper is its own workspace package and does not depend on the app crate), and it
//! `include!`s the `Flavor` enum generated here from `ci/install-identities.json`. Each package's
//! `build.rs` pulls this file in with `#[path]` and calls [`emit`], so the schema cannot drift
//! between the client and the helper: a single generator, two callers, no copy.

use std::path::{Path, PathBuf};

/// Packaging owns the identities; generating the schema here makes adding an install update
/// the client and helper together, without a Python interpreter in the Rust build or on the TV.
pub fn emit(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
    let identities: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(path).expect("install identities")).unwrap();
    let mut variants = String::new();
    let mut apps = String::new();
    let mut objects = String::new();
    let mut object_ids = std::collections::HashSet::new();
    for identity in identities {
        let name = identity["name"].as_str().expect("flavor name");
        assert!(!name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase()));
        let variant = name[..1].to_ascii_uppercase() + &name[1..];
        let app = identity["app_id"].as_str().expect("app id");
        let object = identity["object_id"].as_str().expect("DB8 object id");
        // Device (webOS 4.10.2): 16 bytes = generated base64 ID;
        // 17+ bytes fail DB8 put with -3968 "Invalid _id length".
        assert!(
            (1..=15).contains(&object.len()),
            "invalid DB8 object_id {object:?} for flavor {name}: custom IDs must be 1..=15 bytes"
        );
        // DB8 IDs are global across kinds, so every install must use a distinct ID.
        assert!(
            object_ids.insert(object.to_owned()),
            "duplicate DB8 object_id {object:?} for flavor {name}"
        );
        variants.push_str(&format!("{variant},\n"));
        apps.push_str(&format!("{app:?} => Some(Self::{variant}),\n"));
        objects.push_str(&format!("Self::{variant} => {object:?},\n"));
    }
    let generated = format!(
        "#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
         #[serde(rename_all = \"snake_case\")]
         pub(crate) enum Flavor {{ {variants} }}
         impl Flavor {{
             pub(crate) fn from_app_id(id: &str) -> Option<Self> {{
                 match id {{ {apps} _ => None }}
             }}
             pub(crate) fn object_id(self) -> &'static str {{ match self {{ {objects} }} }}
         }}"
    );
    std::fs::write(
        PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("install_identities.rs"),
        generated,
    )
    .unwrap();
}
