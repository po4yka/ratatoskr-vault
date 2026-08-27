//! Local immutable `BlobStore` contract tests.

#![allow(clippy::expect_used, reason = "test assertions")]

use std::path::PathBuf;

use ratatoskr_vault_blobstore::{BlobStoreError, LocalBlobStore};
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

fn temporary_root() -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("ratatoskr-vault-blobstore-{nonce}"));
    std::fs::create_dir_all(&root).expect("temporary root must be creatable");
    root
}
