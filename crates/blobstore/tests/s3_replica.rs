//! S3-compatible replica round-trip and hostile-response tests.

#![allow(clippy::expect_used, reason = "fixture setup failures must fail tests")]

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;

use ratatoskr_vault_blobstore::replica::{ReplicaDeleteOutcome, ReplicaError, ReplicaStore};
use ratatoskr_vault_core::config::{ReplicaTargetConfig, ReplicasConfig};
use ratatoskr_vault_core::snapshot::BlobRef;
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use url::Url;

use support::S3Fixture;

#[tokio::test]
async fn upload_redownload_round_trip_digest_equality() {
    let fixture = S3Fixture::start().await;
    let root = fixture_root("round-trip");
    std::fs::create_dir_all(&root).expect("fixture root must be created");
    let source = root.join("bundle.bin");
    let bytes = b"verified off-host snapshot bundle";
    std::fs::write(&source, bytes).expect("fixture bundle must be written");
    let reference = BlobRef {
        owner: "ratatoskr-vault".to_owned(),
        sha256: hex_digest(Sha256::digest(bytes)),
        media_type: "application/x-git-bundle".to_owned(),
        size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    };
    let store = ReplicaStore::new("offsite".to_owned(), target(fixture.endpoint()))
        .expect("fixture replica store must build");

    let result = store.upload_and_verify(&reference, &source).await;

    assert_eq!(
        result
            .as_ref()
            .map(|placement| (&placement.sha256, placement.size_bytes)),
        Ok((&reference.sha256, reference.size_bytes)),
        "uploaded bytes must be downloaded and re-hashed; requests={:?}",
        fixture.requests()
    );
    let _ignored = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn remote_corruption_never_returns_verified_placement() {
    let fixture = S3Fixture::start().await;
    fixture.corrupt_next_get();
    let root = fixture_root("remote-corruption");
    std::fs::create_dir_all(&root).expect("fixture root must be created");
    let source = root.join("bundle.bin");
    let bytes = b"remote response must be independently checked";
    std::fs::write(&source, bytes).expect("fixture bundle must be written");
    let reference = BlobRef {
        owner: "ratatoskr-vault".to_owned(),
        sha256: hex_digest(Sha256::digest(bytes)),
        media_type: "application/x-git-bundle".to_owned(),
        size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    };
    let store = ReplicaStore::new("offsite".to_owned(), target(fixture.endpoint()))
        .expect("fixture replica store must build");

    let result = store.upload_and_verify(&reference, &source).await;

    assert_eq!(
        result,
        Err(ReplicaError::RemoteChecksumMismatch),
        "corrupted re-download must have its closed remote checksum class; requests={:?}",
        fixture.requests()
    );
    let _ignored = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn delete_requires_verified_remote_absence() {
    let fixture = S3Fixture::start().await;
    let root = fixture_root("delete-absence");
    std::fs::create_dir_all(&root).expect("fixture root must be created");
    let source = root.join("bundle.bin");
    let bytes = b"delete acknowledgement is not absence proof";
    std::fs::write(&source, bytes).expect("fixture bundle must be written");
    let reference = BlobRef {
        owner: "ratatoskr-vault".to_owned(),
        sha256: hex_digest(Sha256::digest(bytes)),
        media_type: "application/x-git-bundle".to_owned(),
        size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    };
    let store = ReplicaStore::new("offsite".to_owned(), target(fixture.endpoint()))
        .expect("fixture replica store must build");
    let placement = store
        .upload_and_verify(&reference, &source)
        .await
        .expect("fixture placement must verify");
    fixture.retain_next_delete();

    assert_eq!(
        store.delete_verified(&reference, &placement).await,
        Err(ReplicaError::RemoteStillPresent),
        "DELETE acknowledgement is insufficient; requests={:?}",
        fixture.requests()
    );
    assert_eq!(
        store.delete_verified(&reference, &placement).await,
        Ok(ReplicaDeleteOutcome::Deleted)
    );
    assert_eq!(
        store.delete_verified(&reference, &placement).await,
        Ok(ReplicaDeleteOutcome::AlreadyAbsent)
    );
    let _ignored = std::fs::remove_dir_all(root);
}

fn target(endpoint: &str) -> ReplicaTargetConfig {
    let mut targets = BTreeMap::new();
    let target = ReplicaTargetConfig {
        endpoint: Url::parse(endpoint).expect("fixture endpoint must parse"),
        bucket: "vault-fixtures".to_owned(),
        region: "fixture-1".to_owned(),
        key_prefix: "test".to_owned(),
        enabled: true,
        required: true,
        access_key: SecretString::from("fixture-access"),
        secret_access_key: SecretString::from("fixture-secret"),
        session_token: None,
        connect_timeout_seconds: 2,
        request_timeout_seconds: 10,
        attempt_timeout_seconds: 5,
        max_object_bytes: 1024 * 1024,
        max_backlog_items: 8,
        max_backlog_bytes: 8 * 1024 * 1024,
        max_concurrent: 2,
    };
    targets.insert("offsite".to_owned(), target.clone());
    let _config = ReplicasConfig { targets };
    target
}

fn fixture_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ratatoskr-vault-s3-{label}-{}", std::process::id()))
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let mut output = String::new();
    for byte in digest.as_ref() {
        use core::fmt::Write as _;
        let _ignored = write!(output, "{byte:02x}");
    }
    output
}
