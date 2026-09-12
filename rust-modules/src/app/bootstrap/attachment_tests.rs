use super::*;
use crate::app::{boot, recorder::Recplay};
use crate::plex::session::{self, Session};

fn initial_for(saved: Session, entropy: Option<[u8; 16]>) -> Initial {
    let mut initial = Initial::synthetic_home(17, 32517).unwrap();
    let crate::auth::owner::BootstrapAuthority::DevPms { primary, .. } = initial.session.authority
    else {
        unreachable!()
    };
    initial.session = crate::auth::SessionInit::captured_boot(saved, Some(primary), Vec::new());
    initial.entropy = Entropy::Captured(entropy);
    initial.validate().unwrap();
    initial
}

#[test]
fn concurrent_session_change_after_attachment_rolls_back_only_this_attempt() {
    let _serial = crate::testlock::serial();
    let temp = session::TempSession::new("recording-attachment-race");
    temp.assert_only_target();
    let root = temp.path().parent().unwrap().to_path_buf();
    let latest = root.join("plxnative-recordings/latest");
    std::fs::create_dir_all(&latest).unwrap();
    std::fs::write(
        latest.join("preexisting-sentinel"),
        b"keep existing artifact",
    )
    .unwrap();
    let external = root.join("external");
    std::fs::create_dir(&external).unwrap();
    std::fs::write(external.join("sentinel"), b"keep external target").unwrap();
    std::os::unix::fs::symlink(&external, latest.join("preexisting-link")).unwrap();
    std::fs::write(root.join("plxnative-recplay"), external.to_str().unwrap()).unwrap();

    let (saved, entropy, deferred) = session::load_capturing_entropy();
    let initial = initial_for(saved, entropy);
    let mut rec = Recplay::recording_with_sink(
        &initial,
        Box::new(crate::ui::rec::DirSink::create(&latest).unwrap()),
    )
    .unwrap();
    assert!(latest.join("manifest.json").exists());
    // Deterministic injection at the real production seam: the recorder is attached, but
    // deferred persistence has not run. No timing sleeps or mid-history owner replacement.
    session::save(&Session {
        client_id: "synthetic-newer-client".into(),
        ..Default::default()
    });
    let newer = std::fs::read(temp.path()).unwrap();
    assert!(boot::apply_deferred_capture(&mut rec, deferred).is_err());
    drop(rec); // the production refusal drops App; no later drop may recreate capture files
    assert!(std::fs::read(temp.path()).unwrap() == newer);
    assert!(
        !latest.join("manifest.json").exists(),
        "refused attempt must not retain typed credentials or block create_new"
    );
    assert!(!latest.join("rec-0000.jsonl").exists());
    assert!(crate::ui::rec::Recording::load(&latest, crate::app::recorder::state_fp()).is_err());
    assert_eq!(
        std::fs::read(latest.join("preexisting-sentinel")).unwrap(),
        b"keep existing artifact"
    );
    assert!(std::fs::symlink_metadata(latest.join("preexisting-link"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        std::fs::read(external.join("sentinel")).unwrap(),
        b"keep external target"
    );
    assert!(root.join("plxnative-recplay").exists());

    let (saved, entropy, deferred) = session::load_capturing_entropy();
    let initial = initial_for(saved, entropy);
    let mut retry = Recplay::recording_with_sink(
        &initial,
        Box::new(crate::ui::rec::DirSink::create(&latest).unwrap()),
    )
    .expect("immediate next attempt can open");
    boot::apply_deferred_capture(&mut retry, deferred).unwrap();
    retry.tick(0, 0.0);
    assert!(!retry.finish());
}
