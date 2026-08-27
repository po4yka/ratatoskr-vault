//! Snapshot manifest contract tests.

use ratatoskr_vault_core::snapshot::{BlobRef, RefEvidence, SnapshotManifest};

#[test]
fn manifest_canonically_records_all_refs_digests_generator_parent_and_bundle_ref() {
    let bundle = BlobRef {
        owner: "ratatoskr-vault".to_owned(),
        sha256: "a".repeat(64),
        media_type: "application/vnd.git.bundle".to_owned(),
        size_bytes: 42,
    };
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
    );

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
