//! Bounded S3-compatible off-host replica storage.

use std::path::Path as FilePath;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use object_store::aws::AmazonS3Builder;
use object_store::path::Path as ObjectPath;
use object_store::{ClientOptions, ObjectStore, ObjectStoreExt as _, RetryConfig};
use ratatoskr_vault_core::config::ReplicaTargetConfig;
use ratatoskr_vault_core::snapshot::BlobRef;
use secrecy::ExposeSecret as _;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const OWNER: &str = "ratatoskr-vault";
const MULTIPART_CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// A verified remote object placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaPlacement {
    /// Stable target identity from configuration.
    pub target: String,
    /// Content-derived object key, never a repository-controlled name.
    pub object_key: String,
    /// Re-verified remote byte count.
    pub size_bytes: u64,
    /// Re-verified remote digest.
    pub sha256: String,
}

/// Closed replica transfer failures; provider text and credentials are never retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReplicaError {
    /// The reference, source, target identity, or key prefix is invalid.
    #[error("invalid replica input")]
    InvalidInput,
    /// Local source bytes cannot be read.
    #[error("replica source I/O failed")]
    SourceIo,
    /// The remote adapter rejected or failed a request.
    #[error("replica remote request failed")]
    Remote,
    /// The content-derived object does not exist at this target.
    #[error("remote replica object is absent")]
    NotFound,
    /// The absolute transfer deadline expired.
    #[error("replica transfer deadline expired")]
    Timeout,
    /// Streamed bytes differ from the immutable artifact reference.
    #[error("replica bytes do not match their digest")]
    DigestMismatch,
    /// A completed remote read did not match the expected content identity.
    #[error("remote replica bytes do not match their digest")]
    RemoteChecksumMismatch,
    /// The remote body ended before the immutable expected length.
    #[error("remote replica body is truncated")]
    RemoteTruncated,
    /// The remote body exceeded the immutable expected length or configured ceiling.
    #[error("remote replica body is oversized")]
    RemoteOversized,
    /// The source or response exceeded the finite configured ceiling.
    #[error("replica bytes exceed the configured size limit")]
    SizeLimitExceeded,
    /// Graceful worker shutdown cancelled the transfer.
    #[error("replica transfer cancelled")]
    Cancelled,
}

/// One explicitly configured S3-compatible target.
#[derive(Debug, Clone)]
pub struct ReplicaStore {
    target: String,
    store: Arc<dyn ObjectStore>,
    key_prefix: String,
    max_object_bytes: u64,
    request_timeout: Duration,
}

impl ReplicaStore {
    /// Builds an S3 client from the supplied target only. This deliberately never calls
    /// `AmazonS3Builder::from_env`, so credential profiles and metadata services are not consulted.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::InvalidInput`] when the explicit endpoint/client cannot be built.
    pub fn new(target: String, config: ReplicaTargetConfig) -> Result<Self, ReplicaError> {
        if target.is_empty() || config.max_object_bytes == 0 {
            return Err(ReplicaError::InvalidInput);
        }
        let _already_installed = rustls::crypto::ring::default_provider().install_default();
        let allow_http = config.endpoint.scheme() == "http";
        let client_options = ClientOptions::new()
            .with_allow_http(allow_http)
            .with_connect_timeout(Duration::from_secs(config.connect_timeout_seconds))
            .with_timeout(Duration::from_secs(config.attempt_timeout_seconds));
        let retry = RetryConfig {
            max_retries: 2,
            retry_timeout: Duration::from_secs(config.attempt_timeout_seconds),
            ..RetryConfig::default()
        };
        let mut builder = AmazonS3Builder::new()
            .with_endpoint(config.endpoint.as_str())
            .with_bucket_name(config.bucket)
            .with_region(config.region)
            .with_access_key_id(config.access_key.expose_secret())
            .with_secret_access_key(config.secret_access_key.expose_secret())
            .with_virtual_hosted_style_request(false)
            .with_client_options(client_options)
            .with_retry(retry);
        if let Some(token) = config.session_token.as_ref() {
            builder = builder.with_token(token.expose_secret());
        }
        let store = builder.build().map_err(|_| ReplicaError::InvalidInput)?;
        Ok(Self {
            target,
            store: Arc::new(store),
            key_prefix: config.key_prefix.trim_matches('/').to_owned(),
            max_object_bytes: config.max_object_bytes,
            request_timeout: Duration::from_secs(config.request_timeout_seconds),
        })
    }

    /// Uploads a local immutable artifact, downloads it again, and verifies its exact identity.
    /// An existing content-derived object is re-read rather than overwritten.
    ///
    /// # Errors
    ///
    /// Returns a closed transfer failure without provider-controlled diagnostics.
    pub async fn upload_and_verify(
        &self,
        reference: &BlobRef,
        source: &FilePath,
    ) -> Result<ReplicaPlacement, ReplicaError> {
        tokio::time::timeout(
            self.request_timeout,
            self.upload_and_verify_inner(reference, source),
        )
        .await
        .map_err(|_| ReplicaError::Timeout)?
    }

    /// Uploads with a cooperative shutdown signal, explicitly aborting an owned multipart upload
    /// when cancellation arrives after it has been created.
    ///
    /// # Errors
    ///
    /// Returns a closed transfer, verification, cancellation, or local source failure.
    pub async fn upload_and_verify_cancellable(
        &self,
        reference: &BlobRef,
        source: &FilePath,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<ReplicaPlacement, ReplicaError> {
        tokio::time::timeout(
            self.request_timeout,
            self.upload_and_verify_cancellable_inner(reference, source, &mut shutdown),
        )
        .await
        .map_err(|_| ReplicaError::Timeout)?
    }

    async fn upload_and_verify_cancellable_inner(
        &self,
        reference: &BlobRef,
        source: &FilePath,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<ReplicaPlacement, ReplicaError> {
        if *shutdown.borrow() {
            return Err(ReplicaError::Cancelled);
        }
        validate_reference(reference, self.max_object_bytes)?;
        let key = object_key(&self.key_prefix, reference)?;
        let location = ObjectPath::parse(&key).map_err(|_| ReplicaError::InvalidInput)?;
        let initial = tokio::select! {
            biased;
            _ = shutdown.changed() => {
                return Err(ReplicaError::Cancelled);
            }
            result = self.verify_remote(reference, &location) => result,
        };
        match initial {
            Ok(()) => return Ok(self.placement(reference, key)),
            Err(ReplicaError::NotFound) => {}
            Err(error) => return Err(error),
        }
        self.upload_source_cancellable(reference, source, &location, shutdown)
            .await?;
        tokio::select! {
            biased;
            _ = shutdown.changed() => {
                Err(ReplicaError::Cancelled)
            }
            result = self.verify_remote(reference, &location) => {
                result?;
                Ok(self.placement(reference, key))
            }
        }
    }

    async fn upload_source_cancellable(
        &self,
        reference: &BlobRef,
        source: &FilePath,
        location: &ObjectPath,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), ReplicaError> {
        let metadata = tokio::fs::symlink_metadata(source)
            .await
            .map_err(|_| ReplicaError::SourceIo)?;
        if !metadata.is_file() || metadata.len() > self.max_object_bytes {
            return Err(ReplicaError::InvalidInput);
        }
        if metadata.len() != reference.size_bytes {
            return Err(ReplicaError::DigestMismatch);
        }
        let mut upload = tokio::select! {
            biased;
            _ = shutdown.changed() => {
                return Err(ReplicaError::Cancelled);
            }
            result = self.store.put_multipart(location) => {
                result.map_err(|_| ReplicaError::Remote)?
            }
        };
        let result = self
            .stream_parts_cancellable(reference, source, upload.as_mut(), shutdown)
            .await;
        if let Err(error) = result {
            let _aborted = upload.abort().await;
            return Err(error);
        }
        let completed = tokio::select! {
            biased;
            _ = shutdown.changed() => {
                Err(ReplicaError::Cancelled)
            }
            result = upload.complete() => result.map(|_| ()).map_err(|_| ReplicaError::Remote),
        };
        if let Err(error) = completed {
            let _aborted = upload.abort().await;
            return Err(error);
        }
        Ok(())
    }

    async fn stream_parts_cancellable(
        &self,
        reference: &BlobRef,
        source: &FilePath,
        upload: &mut dyn object_store::MultipartUpload,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), ReplicaError> {
        let mut file = tokio::fs::File::open(source)
            .await
            .map_err(|_| ReplicaError::SourceIo)?;
        let mut buffer = vec![0_u8; MULTIPART_CHUNK_BYTES];
        let mut digest = Sha256::new();
        let mut read = 0_u64;
        loop {
            if *shutdown.borrow() {
                return Err(ReplicaError::Cancelled);
            }
            let count = file
                .read(&mut buffer)
                .await
                .map_err(|_| ReplicaError::SourceIo)?;
            if count == 0 {
                break;
            }
            read = read
                .checked_add(u64::try_from(count).map_err(|_| ReplicaError::SizeLimitExceeded)?)
                .ok_or(ReplicaError::SizeLimitExceeded)?;
            if read > self.max_object_bytes {
                return Err(ReplicaError::SizeLimitExceeded);
            }
            let bytes = buffer
                .get(..count)
                .ok_or(ReplicaError::InvalidInput)?
                .to_vec();
            digest.update(&bytes);
            tokio::select! {
                biased;
                _ = shutdown.changed() => {
                    return Err(ReplicaError::Cancelled);
                }
                result = upload.put_part(bytes.into()) => {
                    result.map_err(|_| ReplicaError::Remote)?;
                }
            }
        }
        if read == reference.size_bytes && hex_digest(digest.finalize()) == reference.sha256 {
            Ok(())
        } else {
            Err(ReplicaError::DigestMismatch)
        }
    }

    async fn upload_and_verify_inner(
        &self,
        reference: &BlobRef,
        source: &FilePath,
    ) -> Result<ReplicaPlacement, ReplicaError> {
        validate_reference(reference, self.max_object_bytes)?;
        let key = object_key(&self.key_prefix, reference)?;
        let location = ObjectPath::parse(&key).map_err(|_| ReplicaError::InvalidInput)?;

        match self.verify_remote(reference, &location).await {
            Ok(()) => return Ok(self.placement(reference, key)),
            Err(ReplicaError::NotFound) => {}
            Err(error) => return Err(error),
        }

        self.upload_source(reference, source, &location).await?;
        self.verify_remote(reference, &location).await?;
        Ok(self.placement(reference, key))
    }

    async fn upload_source(
        &self,
        reference: &BlobRef,
        source: &FilePath,
        location: &ObjectPath,
    ) -> Result<(), ReplicaError> {
        let metadata = tokio::fs::symlink_metadata(source)
            .await
            .map_err(|_| ReplicaError::SourceIo)?;
        if !metadata.is_file() || metadata.len() > self.max_object_bytes {
            return Err(ReplicaError::InvalidInput);
        }
        if metadata.len() != reference.size_bytes {
            return Err(ReplicaError::DigestMismatch);
        }

        let mut upload = self
            .store
            .put_multipart(location)
            .await
            .map_err(|_| ReplicaError::Remote)?;
        let result = self.stream_parts(reference, source, upload.as_mut()).await;
        if let Err(error) = result {
            let _ignored = upload.abort().await;
            return Err(error);
        }
        if upload.complete().await.is_err() {
            let _ignored = upload.abort().await;
            return Err(ReplicaError::Remote);
        }
        Ok(())
    }

    async fn stream_parts(
        &self,
        reference: &BlobRef,
        source: &FilePath,
        upload: &mut dyn object_store::MultipartUpload,
    ) -> Result<(), ReplicaError> {
        let mut file = tokio::fs::File::open(source)
            .await
            .map_err(|_| ReplicaError::SourceIo)?;
        let mut buffer = vec![0_u8; MULTIPART_CHUNK_BYTES];
        let mut digest = Sha256::new();
        let mut read = 0_u64;
        loop {
            let count = file
                .read(&mut buffer)
                .await
                .map_err(|_| ReplicaError::SourceIo)?;
            if count == 0 {
                break;
            }
            read = read
                .checked_add(u64::try_from(count).map_err(|_| ReplicaError::SizeLimitExceeded)?)
                .ok_or(ReplicaError::SizeLimitExceeded)?;
            if read > self.max_object_bytes {
                return Err(ReplicaError::SizeLimitExceeded);
            }
            let bytes = buffer
                .get(..count)
                .ok_or(ReplicaError::InvalidInput)?
                .to_vec();
            digest.update(&bytes);
            upload
                .put_part(bytes.into())
                .await
                .map_err(|_| ReplicaError::Remote)?;
        }
        if read == reference.size_bytes && hex_digest(digest.finalize()) == reference.sha256 {
            Ok(())
        } else {
            Err(ReplicaError::DigestMismatch)
        }
    }

    async fn verify_remote(
        &self,
        reference: &BlobRef,
        location: &ObjectPath,
    ) -> Result<(), ReplicaError> {
        let result = match self.store.get(location).await {
            Ok(result) => result,
            Err(object_store::Error::NotFound { .. }) => return Err(ReplicaError::NotFound),
            Err(_) => return Err(ReplicaError::Remote),
        };
        let mut stream = result.into_stream();
        let mut digest = Sha256::new();
        let mut read = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ReplicaError::Remote)?;
            read = read
                .checked_add(
                    u64::try_from(chunk.len()).map_err(|_| ReplicaError::SizeLimitExceeded)?,
                )
                .ok_or(ReplicaError::SizeLimitExceeded)?;
            if read > self.max_object_bytes || read > reference.size_bytes {
                return Err(ReplicaError::RemoteOversized);
            }
            digest.update(&chunk);
        }
        if read < reference.size_bytes {
            Err(ReplicaError::RemoteTruncated)
        } else if hex_digest(digest.finalize()) != reference.sha256 {
            Err(ReplicaError::RemoteChecksumMismatch)
        } else {
            Ok(())
        }
    }

    /// Streams the content-derived remote object into a caller-owned create-new scratch file and
    /// verifies exact length and SHA-256 before returning it.
    ///
    /// # Errors
    ///
    /// Returns a closed transfer or local scratch error. A failed transfer removes only the file
    /// this call created; an existing destination is never overwritten.
    pub async fn download_verified(
        &self,
        reference: &BlobRef,
        destination: &FilePath,
    ) -> Result<ReplicaPlacement, ReplicaError> {
        tokio::time::timeout(
            self.request_timeout,
            self.download_verified_inner(reference, destination),
        )
        .await
        .map_err(|_| ReplicaError::Timeout)?
    }

    async fn download_verified_inner(
        &self,
        reference: &BlobRef,
        destination: &FilePath,
    ) -> Result<ReplicaPlacement, ReplicaError> {
        validate_reference(reference, self.max_object_bytes)?;
        let key = object_key(&self.key_prefix, reference)?;
        let location = ObjectPath::parse(&key).map_err(|_| ReplicaError::InvalidInput)?;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .await
            .map_err(|_| ReplicaError::SourceIo)?;
        let result = self
            .download_remote_to(reference, &location, &mut file)
            .await;
        if result.is_err() {
            drop(file);
            let _ignored = tokio::fs::remove_file(destination).await;
            return result.map(|()| self.placement(reference, key));
        }
        if file.sync_all().await.is_err() {
            drop(file);
            let _ignored = tokio::fs::remove_file(destination).await;
            return Err(ReplicaError::SourceIo);
        }
        Ok(self.placement(reference, key))
    }

    async fn download_remote_to(
        &self,
        reference: &BlobRef,
        location: &ObjectPath,
        destination: &mut tokio::fs::File,
    ) -> Result<(), ReplicaError> {
        let result = match self.store.get(location).await {
            Ok(result) => result,
            Err(object_store::Error::NotFound { .. }) => return Err(ReplicaError::NotFound),
            Err(_) => return Err(ReplicaError::Remote),
        };
        let mut stream = result.into_stream();
        let mut digest = Sha256::new();
        let mut read = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ReplicaError::Remote)?;
            read = read
                .checked_add(u64::try_from(chunk.len()).map_err(|_| ReplicaError::RemoteOversized)?)
                .ok_or(ReplicaError::RemoteOversized)?;
            if read > self.max_object_bytes || read > reference.size_bytes {
                return Err(ReplicaError::RemoteOversized);
            }
            digest.update(&chunk);
            destination
                .write_all(&chunk)
                .await
                .map_err(|_| ReplicaError::SourceIo)?;
        }
        if read < reference.size_bytes {
            Err(ReplicaError::RemoteTruncated)
        } else if hex_digest(digest.finalize()) != reference.sha256 {
            Err(ReplicaError::RemoteChecksumMismatch)
        } else {
            Ok(())
        }
    }

    fn placement(&self, reference: &BlobRef, object_key: String) -> ReplicaPlacement {
        ReplicaPlacement {
            target: self.target.clone(),
            object_key,
            size_bytes: reference.size_bytes,
            sha256: reference.sha256.clone(),
        }
    }
}

fn validate_reference(reference: &BlobRef, max_object_bytes: u64) -> Result<(), ReplicaError> {
    let valid_digest = reference.sha256.len() == 64
        && reference
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
    if reference.owner != OWNER
        || !valid_digest
        || reference.media_type.is_empty()
        || reference.size_bytes > max_object_bytes
    {
        Err(ReplicaError::InvalidInput)
    } else {
        Ok(())
    }
}

fn object_key(prefix: &str, reference: &BlobRef) -> Result<String, ReplicaError> {
    if !prefix.is_empty()
        && prefix
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(ReplicaError::InvalidInput);
    }
    let shard: String = reference.sha256.chars().take(2).collect();
    let key = if prefix.is_empty() {
        format!("sha256/{shard}/{}", reference.sha256)
    } else {
        format!("{prefix}/sha256/{shard}/{}", reference.sha256)
    };
    Ok(key)
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
