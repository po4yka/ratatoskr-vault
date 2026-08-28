//! Local immutable `BlobStore` contract tests.

#![allow(clippy::expect_used, reason = "test assertions")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatoskr_vault_blobstore::{BlobStoreError, LocalBlobStore, LocalDeleteOutcome};
use ratatoskr_vault_core::snapshot::BlobRef;

#[test]
fn existing_blob_is_immutable_and_identical_retry_is_deterministic() {
    let root = temporary_root();
    let source = root.join("source.bundle");
    std::fs::write(&source, b"payload").expect("fixture bytes must be writable");
    let expected = BlobRef {
        owner: "ratatoskr-vault".to_owned(),
        sha256: "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5".to_owned(),
        media_type: "application/vnd.git.bundle".to_owned(),
        size_bytes: 7,
    };
    let store =
        LocalBlobStore::new(root.join("blobs"), 1_024).expect("store root must be initializable");

    let first = store
        .publish_file(&expected, &source)
        .expect("initial publication must succeed");
    let retry = store
        .publish_file(&expected, &source)
        .expect("identical publication must return the existing reference");
    let altered = root.join("altered.bundle");
    std::fs::write(&altered, b"altered").expect("altered bytes must be writable");
    let conflict = store.publish_file(&expected, &altered);

    assert_eq!(retry, first);
    assert!(conflict.is_err());
    assert_eq!(
        std::fs::read(store.resolve(&first).expect("published blob must resolve"))
            .expect("published bytes must remain readable"),
        b"payload"
    );
}

#[test]
fn stored_blob_hash_mismatch_is_detected() {
    let root = temporary_root();
    let source = root.join("source.bundle");
    std::fs::write(&source, b"payload").expect("fixture bytes must be writable");
    let store =
        LocalBlobStore::new(root.join("blobs"), 1_024).expect("store root must be initializable");
    let reference = store
        .reference_for_file(&source, "application/vnd.git.bundle".to_owned())
        .expect("fixture reference must be calculable");
    store
        .publish_file(&reference, &source)
        .expect("fixture blob must publish");
    let stored_path = store
        .resolve(&reference)
        .expect("published blob must resolve");
    std::fs::write(stored_path, b"corrupt").expect("test must be able to inject corruption");

    assert!(matches!(
        store.verify(&reference),
        Err(BlobStoreError::DigestMismatch)
    ));
}

#[test]
fn delete_verified_blob_is_confined_idempotent_and_absent() {
    let root = temporary_root();
    let source = root.join("delete-source.bundle");
    std::fs::write(&source, b"delete me").expect("fixture bytes must be writable");
    let store =
        LocalBlobStore::new(root.join("blobs"), 1_024).expect("store root must be initializable");
    let reference = store
        .reference_for_file(&source, "application/vnd.git.bundle".to_owned())
        .expect("fixture reference must be calculable");
    store
        .publish_file(&reference, &source)
        .expect("fixture blob must publish");

    assert!(matches!(
        store.delete_verified_blob(&reference),
        Ok(LocalDeleteOutcome::Deleted)
    ));
    assert!(
        store.resolve(&reference).is_err(),
        "deleted bytes must be absent"
    );
    assert!(matches!(
        store.delete_verified_blob(&reference),
        Ok(LocalDeleteOutcome::AlreadyAbsent)
    ));

    let hostile_source = root.join("hostile-source.bundle");
    let outside = root.join("outside-evidence.bundle");
    std::fs::write(&hostile_source, b"hostile").expect("hostile source must be writable");
    std::fs::write(&outside, b"outside").expect("outside evidence must be writable");
    let hostile_reference = store
        .reference_for_file(&hostile_source, "application/vnd.git.bundle".to_owned())
        .expect("hostile reference must be calculable");
    store
        .publish_file(&hostile_reference, &hostile_source)
        .expect("hostile fixture must publish");
    let hostile_path = store
        .resolve(&hostile_reference)
        .expect("hostile fixture must resolve");
    std::fs::remove_file(&hostile_path).expect("fixture blob must be replaceable");
    std::os::unix::fs::symlink(&outside, &hostile_path).expect("fixture symlink must be creatable");

    assert!(matches!(
        store.delete_verified_blob(&hostile_reference),
        Err(BlobStoreError::InvalidInput)
    ));
    assert_eq!(
        std::fs::read(outside).expect("outside evidence must remain readable"),
        b"outside"
    );
}

fn temporary_root() -> PathBuf {
    static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_nanos();
    let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "ratatoskr-vault-blobstore-{}-{nonce}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("temporary root must be creatable");
    root
}
