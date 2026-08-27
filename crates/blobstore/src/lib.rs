#![forbid(unsafe_code)]

//! Immutable, Vault-owned content-addressed blob storage.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ratatoskr_vault_core::snapshot::BlobRef;
use sha2::{Digest, Sha256};

const OWNER: &str = "ratatoskr-vault";
const DIGEST_BYTES: usize = 32;
const COPY_BUFFER_BYTES: usize = 16 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A local content-addressed store rooted in Vault-owned storage.
#[derive(Debug, Clone)]
pub struct LocalBlobStore {
    root: PathBuf,
    max_bytes: u64,
}

/// The publication error returned by the test seam before the implementation exists.
#[derive(Debug)]
pub enum BlobStoreError {
    /// The configured root, source, or reference violates the store contract.
    InvalidInput,
    /// An I/O operation for owned storage failed.
    Io(std::io::Error),
    /// Streamed bytes do not match the reference supplied by the caller.
    DigestMismatch,
    /// The stream exceeds the configured finite artifact limit.
    SizeLimitExceeded,
    /// Existing content at a content-addressed key does not match its reference.
    ExistingContentMismatch,
}

impl core::fmt::Display for BlobStoreError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidInput => formatter.write_str("invalid blob store input"),
            Self::Io(_) => formatter.write_str("blob store I/O failed"),
            Self::DigestMismatch => formatter.write_str("blob bytes do not match their digest"),
            Self::SizeLimitExceeded => {
                formatter.write_str("blob exceeds the configured size limit")
            }
            Self::ExistingContentMismatch => {
                formatter.write_str("existing content does not match its blob reference")
            }
        }
    }
}

impl std::error::Error for BlobStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for BlobStoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl LocalBlobStore {
    /// The canonical storage root used to confine immutable artifact operands.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Builds a local store rooted at the supplied Vault-owned directory.
    ///
    /// # Errors
    ///
    /// Returns [`BlobStoreError::InvalidInput`] for a non-absolute root or zero limit, and
    /// [`BlobStoreError::Io`] when the root cannot be created.
    pub fn new(root: PathBuf, max_bytes: u64) -> Result<Self, BlobStoreError> {
        if !root.is_absolute() || max_bytes == 0 {
            return Err(BlobStoreError::InvalidInput);
        }
        std::fs::create_dir_all(&root)?;
        Ok(Self { root, max_bytes })
    }

    /// Publishes an expected reference from an owned source file.
    ///
    /// The source must be a Vault-owned regular file. The reference supplies the expected digest
    /// and length, so a retry cannot overwrite a content-addressed artifact with other bytes.
    ///
    /// # Errors
    ///
    /// Returns a [`BlobStoreError`] when the input, streamed bytes, or existing artifact violates
    /// the immutable publication contract.
    pub fn publish_file(
        &self,
        expected: &BlobRef,
        source: &Path,
    ) -> Result<BlobRef, BlobStoreError> {
        Self::validate_reference(expected)?;
        let source_metadata = std::fs::symlink_metadata(source)?;
        if !source_metadata.is_file() || source_metadata.len() > self.max_bytes {
            return Err(BlobStoreError::InvalidInput);
        }
        if source_metadata.len() != expected.size_bytes {
            return Err(BlobStoreError::DigestMismatch);
        }

        let destination = self.path_for(expected)?;
        let parent = destination.parent().ok_or(BlobStoreError::InvalidInput)?;
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(
            ".publish-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = self.stream_to_temporary(expected, source, &temporary);
        if let Err(error) = result {
            let _ignored = std::fs::remove_file(&temporary);
            return Err(error);
        }

        match std::fs::hard_link(&temporary, &destination) {
            Ok(()) => {
                std::fs::remove_file(&temporary)?;
                File::open(parent)?.sync_all()?;
                Ok(expected.clone())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ignored = std::fs::remove_file(&temporary);
                self.verify_existing(expected)?;
                Ok(expected.clone())
            }
            Err(error) => {
                let _ignored = std::fs::remove_file(&temporary);
                Err(BlobStoreError::Io(error))
            }
        }
    }

    /// Calculates the `BlobRef` for a finite regular file before it is published.
    ///
    /// # Errors
    ///
    /// Returns a [`BlobStoreError`] when the source is not a finite regular file within the
    /// configured limit or cannot be read.
    pub fn reference_for_file(
        &self,
        source: &Path,
        media_type: String,
    ) -> Result<BlobRef, BlobStoreError> {
        let metadata = std::fs::symlink_metadata(source)?;
        if !metadata.is_file() || metadata.len() > self.max_bytes || media_type.is_empty() {
            return Err(BlobStoreError::InvalidInput);
        }
        let mut reader = BufReader::new(File::open(source)?);
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; COPY_BUFFER_BYTES];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            let bytes = buffer.get(..count).ok_or(BlobStoreError::InvalidInput)?;
            digest.update(bytes);
        }
        Ok(BlobRef {
            owner: OWNER.to_owned(),
            sha256: hex_digest(digest.finalize()),
            media_type,
            size_bytes: metadata.len(),
        })
    }

    /// Resolves a reference to its Vault-owned content-addressed path after validation.
    ///
    /// # Errors
    ///
    /// Returns a [`BlobStoreError`] when the reference is invalid or its stored bytes are absent.
    pub fn resolve(&self, reference: &BlobRef) -> Result<PathBuf, BlobStoreError> {
        Self::validate_reference(reference)?;
        let path = self.path_for(reference)?;
        if !std::fs::metadata(&path)?.is_file() {
            return Err(BlobStoreError::InvalidInput);
        }
        Ok(path)
    }

    /// Re-reads immutable stored bytes and verifies their expected length and SHA-256 digest.
    ///
    /// # Errors
    ///
    /// Returns [`BlobStoreError::DigestMismatch`] when bytes differ from the reference and another
    /// [`BlobStoreError`] when the reference or stored path is invalid or unreadable.
    pub fn verify(&self, reference: &BlobRef) -> Result<(), BlobStoreError> {
        let path = self.resolve(reference)?;
        let metadata = std::fs::metadata(&path)?;
        if metadata.len() != reference.size_bytes || metadata.len() > self.max_bytes {
            return Err(BlobStoreError::DigestMismatch);
        }
        let mut reader = BufReader::new(File::open(path)?);
        let mut digest = Sha256::new();
        let mut read = 0_u64;
        let mut buffer = [0_u8; COPY_BUFFER_BYTES];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            read = read
                .checked_add(u64::try_from(count).map_err(|_| BlobStoreError::SizeLimitExceeded)?)
                .ok_or(BlobStoreError::SizeLimitExceeded)?;
            if read > self.max_bytes {
                return Err(BlobStoreError::SizeLimitExceeded);
            }
            digest.update(buffer.get(..count).ok_or(BlobStoreError::DigestMismatch)?);
        }
        if read == reference.size_bytes && hex_digest(digest.finalize()) == reference.sha256 {
            Ok(())
        } else {
            Err(BlobStoreError::DigestMismatch)
        }
    }

    fn stream_to_temporary(
        &self,
        expected: &BlobRef,
        source: &Path,
        temporary: &Path,
    ) -> Result<(), BlobStoreError> {
        let mut reader = BufReader::new(File::open(source)?);
        let mut writer = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temporary)?;
        let mut digest = Sha256::new();
        let mut written = 0_u64;
        let mut buffer = [0_u8; COPY_BUFFER_BYTES];

        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            written = written
                .checked_add(u64::try_from(count).map_err(|_| BlobStoreError::SizeLimitExceeded)?)
                .ok_or(BlobStoreError::SizeLimitExceeded)?;
            if written > self.max_bytes {
                return Err(BlobStoreError::SizeLimitExceeded);
            }
            let bytes = buffer.get(..count).ok_or(BlobStoreError::InvalidInput)?;
            digest.update(bytes);
            writer.write_all(bytes)?;
        }
        writer.sync_all()?;

        if written != expected.size_bytes || hex_digest(digest.finalize()) != expected.sha256 {
            return Err(BlobStoreError::DigestMismatch);
        }
        Ok(())
    }

    fn verify_existing(&self, expected: &BlobRef) -> Result<(), BlobStoreError> {
        let path = self.path_for(expected)?;
        let metadata = std::fs::metadata(&path)?;
        if !metadata.is_file() || metadata.len() != expected.size_bytes {
            return Err(BlobStoreError::ExistingContentMismatch);
        }
        let mut reader = BufReader::new(File::open(path)?);
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; COPY_BUFFER_BYTES];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            let bytes = buffer
                .get(..count)
                .ok_or(BlobStoreError::ExistingContentMismatch)?;
            digest.update(bytes);
        }
        if hex_digest(digest.finalize()) == expected.sha256 {
            Ok(())
        } else {
            Err(BlobStoreError::ExistingContentMismatch)
        }
    }

    fn validate_reference(reference: &BlobRef) -> Result<(), BlobStoreError> {
        let valid_hex = reference.sha256.len() == DIGEST_BYTES * 2
            && reference.sha256.bytes().all(is_lowercase_hex);
        if reference.owner != OWNER || !valid_hex || reference.media_type.is_empty() {
            return Err(BlobStoreError::InvalidInput);
        }
        Ok(())
    }

    fn path_for(&self, reference: &BlobRef) -> Result<PathBuf, BlobStoreError> {
        let path = self.root.join(OWNER).join("sha256").join(&reference.sha256);
        if path.starts_with(&self.root) {
            Ok(path)
        } else {
            Err(BlobStoreError::InvalidInput)
        }
    }
}

fn is_lowercase_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let bytes = digest.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use core::fmt::Write as _;
        let _ignored = write!(output, "{byte:02x}");
    }
    output
}
