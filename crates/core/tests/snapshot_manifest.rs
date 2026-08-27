//! Snapshot manifest contract tests.

use ratatoskr_vault_core::snapshot::{
    BlobRef, ManifestError, ManifestSigningKey, RefEvidence, SnapshotManifest,
    verify_manifest_chain,
};

#[test]
fn manifest_canonically_records_all_refs_digests_generator_parent_and_bundle_ref() {
    let bundle = BlobRef {
        owner: "ratatoskr-vault".to_owned(),
        sha256: "a".repeat(64),
        media_type: "application/vnd.git.bundle".to_owned(),
        size_bytes: 42,
    };
    let signer = ManifestSigningKey::from_seed([5; 32]).expect("fixture signing key must load");
    let manifest = SnapshotManifest::new(
        vec![
            RefEvidence {
                name: "refs/tags/v1".to_owned(),
                oid: "b".repeat(40),
            },
            RefEvidence {
                name: "refs/heads/main".to_owned(),
                oid: "a".repeat(40),
            },
        ],
        vec![bundle.clone()],
        None,
        "2026-08-26T00:00:00Z".to_owned(),
        &signer,
    )
    .expect("fixture manifest must sign");

    assert_eq!(
        manifest.refs.len(),
        2,
        "complete ref evidence must be retained"
    );
    assert!(manifest.refs[0].name < manifest.refs[1].name);
    assert_eq!(manifest.generator_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest.created_at, "2026-08-26T00:00:00Z");
    assert!(manifest.parent_manifest.is_none());
    assert_eq!(manifest.bundles, vec![bundle]);
    assert_eq!(manifest.ref_set_sha256.len(), 64);
}

#[test]
fn manifest_chain_rejects_unknown_key_cycle_and_broken_parent() {
    let signer = ManifestSigningKey::from_seed([9; 32]).expect("fixture signing key must load");
    let trusted_key = signer.verification_key();
    let head = BlobRef {
        owner: "ratatoskr-vault".to_owned(),
        sha256: "1".repeat(64),
        media_type: "application/json".to_owned(),
        size_bytes: 10,
    };
    let mut manifest = SnapshotManifest::new(
        Vec::new(),
        vec![BlobRef {
            owner: "ratatoskr-vault".to_owned(),
            sha256: "2".repeat(64),
            media_type: "application/vnd.git.bundle".to_owned(),
            size_bytes: 20,
        }],
        None,
        "2026-08-27T00:00:00Z".to_owned(),
        &signer,
    )
    .expect("fixture manifest must serialize");

    assert_eq!(
        verify_manifest_chain(&head, &[], 4, |_| Ok(manifest.clone())),
        Err(ManifestError::UnknownSigningKey)
    );

    manifest.parent_manifest = Some(head.clone());
    let manifest = SnapshotManifest::new(
        manifest.refs,
        manifest.bundles,
        manifest.parent_manifest,
        manifest.created_at,
        &signer,
    )
    .expect("cyclic fixture must sign");
    assert_eq!(
        verify_manifest_chain(&head, std::slice::from_ref(&trusted_key), 4, |_| {
            Ok(manifest.clone())
        }),
        Err(ManifestError::ChainCycle)
    );

    let missing = BlobRef {
        sha256: "3".repeat(64),
        ..head.clone()
    };
    let child = SnapshotManifest::new(
        Vec::new(),
        Vec::new(),
        Some(missing),
        "2026-08-27T00:00:01Z".to_owned(),
        &signer,
    )
    .expect("child fixture must sign");
    assert_eq!(
        verify_manifest_chain(&head, &[trusted_key], 4, |reference| {
            if reference == &head {
                Ok(child.clone())
            } else {
                Err(ManifestError::MissingManifest)
            }
        }),
        Err(ManifestError::MissingManifest)
    );
}

#[test]
fn signed_manifest_rejects_tampered_payload() {
    let signer = ManifestSigningKey::from_seed([7; 32]).expect("fixture signing key must load");
    let trusted_key = signer.verification_key();
    let mut manifest = SnapshotManifest::new(
        vec![RefEvidence {
            name: "refs/heads/main".to_owned(),
            oid: "a".repeat(40),
        }],
        vec![BlobRef {
            owner: "ratatoskr-vault".to_owned(),
            sha256: "b".repeat(64),
            media_type: "application/vnd.git.bundle".to_owned(),
            size_bytes: 42,
        }],
        None,
        "2026-08-27T00:00:00Z".to_owned(),
        &signer,
    )
    .expect("fixture manifest must serialize");
    manifest
        .verify_signature(std::slice::from_ref(&trusted_key))
        .expect("untouched manifest must verify");

    manifest.refs[0].oid = "c".repeat(40);

    assert_eq!(
        manifest.verify_signature(&[trusted_key]),
        Err(ManifestError::InvalidSignature),
        "changing signed evidence must invalidate its signature"
    );
}
