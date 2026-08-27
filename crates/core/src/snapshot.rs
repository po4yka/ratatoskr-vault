//! Immutable snapshot manifest evidence.

use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// A manifest-signing key whose secret seed never appears in debug output.
pub struct ManifestSigningKey {
    key_pair: Ed25519KeyPair,
}

impl core::fmt::Debug for ManifestSigningKey {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("ManifestSigningKey([REDACTED])")
    }
}

/// A trusted public key used to verify signed manifests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestVerificationKey {
    /// Stable SHA-256 identifier of the public-key bytes.
    pub key_id: String,
    /// Raw Ed25519 public-key bytes.
    pub public_key: Vec<u8>,
}

/// A signed-manifest validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    /// The supplied private seed cannot create an Ed25519 key.
    #[error("manifest signing key is invalid")]
    InvalidSigningKey,
    /// A configured trusted public key is not exactly one Ed25519 key.
    #[error("manifest verification key is invalid")]
    InvalidVerificationKey,
    /// The manifest names no trusted signing key.
    #[error("manifest signing key is not trusted")]
    UnknownSigningKey,
    /// The signature does not authenticate the canonical manifest bytes.
    #[error("manifest signature is invalid")]
    InvalidSignature,
    /// Canonical manifest serialization failed.
    #[error("manifest serialization failed")]
    Serialization,
    /// Git LFS evidence is non-canonical, duplicated, or disagrees with its `BlobRef`.
    #[error("manifest Git LFS evidence is invalid")]
    InvalidLfsEvidence,
    /// A parent manifest could not be resolved from immutable storage.
    #[error("manifest in digest chain is missing")]
    MissingManifest,
    /// A parent link repeats a manifest already visited.
    #[error("manifest digest chain contains a cycle")]
    ChainCycle,
    /// A parent chain exceeded its configured verification bound.
    #[error("manifest digest chain exceeds its depth limit")]
    ChainTooDeep,
}

/// Verifies a manifest and its parent links and returns the visited `BlobRefs` in chain order.
///
/// # Errors
///
/// Returns a signature/trust error from a loaded manifest, [`ManifestError::ChainCycle`] for a
/// repeated digest, [`ManifestError::ChainTooDeep`] at the configured bound, or the resolver's
/// storage error when a manifest is unavailable.
pub fn verify_manifest_chain<F>(
    head: &BlobRef,
    trusted_keys: &[ManifestVerificationKey],
    max_depth: usize,
    mut load: F,
) -> Result<Vec<BlobRef>, ManifestError>
where
    F: FnMut(&BlobRef) -> Result<SnapshotManifest, ManifestError>,
{
    let mut visited = BTreeSet::new();
    let mut chain = Vec::new();
    let mut current = head.clone();

    while chain.len() < max_depth {
        if !visited.insert(current.sha256.clone()) {
            return Err(ManifestError::ChainCycle);
        }
        let manifest = load(&current)?;
        manifest.verify_signature(trusted_keys)?;
        chain.push(current);
        let Some(parent) = manifest.parent_manifest else {
            return Ok(chain);
        };
        current = parent;
    }

    Err(ManifestError::ChainTooDeep)
}

#[derive(serde::Serialize)]
struct UnsignedManifest<'a> {
    schema_version: u16,
    refs: &'a [RefEvidence],
    generator_version: &'a str,
    created_at: &'a str,
    parent_manifest: &'a Option<BlobRef>,
    bundles: &'a [BlobRef],
    lfs: &'a Option<LfsEvidence>,
    ref_set_sha256: &'a str,
    signing_key_id: &'a str,
}

/// A durable reference to immutable Vault-owned bytes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobRef {
    /// Owning service.
    pub owner: String,
    /// SHA-256 digest as lowercase hex.
    pub sha256: String,
    /// IANA media type.
    pub media_type: String,
    /// Content length.
    pub size_bytes: u64,
}

/// Canonical immutable snapshot evidence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotManifest {
    /// Stable format identifier for this evidence document.
    pub schema_version: u16,
    /// Ordered refs in the snapshot.
    pub refs: Vec<RefEvidence>,
    /// Generator package version.
    pub generator_version: String,
    /// UTC creation timestamp supplied by the lifecycle clock.
    pub created_at: String,
    /// Parent manifest when this snapshot follows an earlier one.
    pub parent_manifest: Option<BlobRef>,
    /// Full bundle `BlobRefs`.
    pub bundles: Vec<BlobRef>,
    /// Complete Git LFS object evidence when explicitly collected.
    pub lfs: Option<LfsEvidence>,
    /// SHA-256 digest of canonical refs.
    pub ref_set_sha256: String,
    /// SHA-256 identifier of the Ed25519 public key that signed this manifest.
    pub signing_key_id: String,
    /// Lowercase-hex Ed25519 signature over the canonical unsigned manifest bytes.
    pub signature: String,
}

/// One ref entry in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefEvidence {
    /// Ref name.
    pub name: String,
    /// Object ID named by the ref.
    pub oid: String,
}

/// Canonical complete Git LFS collection evidence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LfsEvidence {
    /// Exact Git LFS client version used for collection.
    pub tool_version: String,
    /// Referenced objects, intended to be ordered by OID.
    pub objects: Vec<LfsObjectEvidence>,
    /// Sum of all object byte lengths.
    pub total_bytes: u64,
    /// SHA-256 of canonical `oid size blob-digest` records.
    pub aggregate_sha256: String,
}

/// One immutable Git LFS object named by the manifest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LfsObjectEvidence {
    /// Lowercase SHA-256 object identifier from the LFS pointer.
    pub oid: String,
    /// Vault-owned content-addressed object bytes.
    pub blob: BlobRef,
}

impl LfsEvidence {
    /// Builds aggregate evidence from the observed object order.
    #[must_use]
    pub fn new(tool_version: String, mut objects: Vec<LfsObjectEvidence>) -> Self {
        objects.sort_by(|left, right| left.oid.cmp(&right.oid));
        let total_bytes = objects.iter().fold(0_u64, |total, object| {
            total.saturating_add(object.blob.size_bytes)
        });
        let aggregate_sha256 = canonical_lfs_digest(&objects);
        Self {
            tool_version,
            objects,
            total_bytes,
            aggregate_sha256,
        }
    }
}

impl SnapshotManifest {
    /// Builds and signs a canonically ordered manifest from full ref evidence.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Serialization`] when canonical serialization fails.
    pub fn new(
        mut refs: Vec<RefEvidence>,
        bundles: Vec<BlobRef>,
        parent_manifest: Option<BlobRef>,
        created_at: String,
        lfs: Option<LfsEvidence>,
        signer: &ManifestSigningKey,
    ) -> Result<Self, ManifestError> {
        refs.sort_by(|left, right| left.name.cmp(&right.name));
        if lfs.as_ref().is_some_and(|evidence| !valid_lfs(evidence)) {
            return Err(ManifestError::InvalidLfsEvidence);
        }
        let ref_set_sha256 = canonical_ref_digest(&refs);
        let verification_key = signer.verification_key();
        let mut manifest = Self {
            schema_version: 1,
            refs,
            generator_version: env!("CARGO_PKG_VERSION").to_owned(),
            created_at,
            parent_manifest,
            bundles,
            lfs,
            ref_set_sha256,
            signing_key_id: verification_key.key_id,
            signature: String::new(),
        };
        let unsigned = manifest.unsigned_bytes()?;
        manifest.signature = encode_hex(signer.key_pair.sign(&unsigned).as_ref());
        Ok(manifest)
    }

    /// Whether this v1 manifest carries a complete explicitly enabled LFS component.
    #[must_use]
    pub const fn includes_lfs(&self) -> bool {
        self.lfs.is_some()
    }

    /// Verifies this manifest against the supplied trusted keys.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::UnknownSigningKey`] when the key id is absent and a signature
    /// error when canonical bytes do not match.
    pub fn verify_signature(
        &self,
        trusted_keys: &[ManifestVerificationKey],
    ) -> Result<(), ManifestError> {
        let trusted_key = trusted_keys
            .iter()
            .find(|key| key.key_id == self.signing_key_id)
            .ok_or(ManifestError::UnknownSigningKey)?;
        let signature = decode_hex(&self.signature).ok_or(ManifestError::InvalidSignature)?;
        let unsigned = self.unsigned_bytes()?;
        UnparsedPublicKey::new(&ED25519, &trusted_key.public_key)
            .verify(&unsigned, &signature)
            .map_err(|_| ManifestError::InvalidSignature)
    }

    fn unsigned_bytes(&self) -> Result<Vec<u8>, ManifestError> {
        serde_json::to_vec(&UnsignedManifest {
            schema_version: self.schema_version,
            refs: &self.refs,
            generator_version: &self.generator_version,
            created_at: &self.created_at,
            parent_manifest: &self.parent_manifest,
            bundles: &self.bundles,
            lfs: &self.lfs,
            ref_set_sha256: &self.ref_set_sha256,
            signing_key_id: &self.signing_key_id,
        })
        .map_err(|_| ManifestError::Serialization)
    }
}

fn valid_lfs(evidence: &LfsEvidence) -> bool {
    if evidence.tool_version.is_empty()
        || evidence.total_bytes
            != evidence.objects.iter().fold(0_u64, |total, object| {
                total.saturating_add(object.blob.size_bytes)
            })
        || evidence.aggregate_sha256 != canonical_lfs_digest(&evidence.objects)
    {
        return false;
    }
    let mut previous = None;
    for object in &evidence.objects {
        if object.oid != object.blob.sha256
            || object.oid.len() != 64
            || !object
                .oid
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            || object.blob.owner != "ratatoskr-vault"
            || object.blob.media_type != "application/octet-stream"
            || previous.is_some_and(|value| value >= object.oid.as_str())
        {
            return false;
        }
        previous = Some(object.oid.as_str());
    }
    true
}

/// Computes the aggregate digest for ordered LFS object evidence.
#[must_use]
pub fn canonical_lfs_digest(objects: &[LfsObjectEvidence]) -> String {
    let mut digest = Sha256::new();
    for object in objects {
        digest.update(object.oid.as_bytes());
        digest.update(b" ");
        digest.update(object.blob.size_bytes.to_string().as_bytes());
        digest.update(b" ");
        digest.update(object.blob.sha256.as_bytes());
        digest.update(b"\n");
    }
    format!("{:x}", digest.finalize())
}

impl ManifestSigningKey {
    /// Creates an Ed25519 signer from one 32-byte seed.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::InvalidSigningKey`] when the seed cannot form a key pair.
    pub fn from_seed(seed: [u8; 32]) -> Result<Self, ManifestError> {
        Ed25519KeyPair::from_seed_unchecked(&seed)
            .map(|key_pair| Self { key_pair })
            .map_err(|_| ManifestError::InvalidSigningKey)
    }

    /// Returns the public verification key and its stable identifier.
    #[must_use]
    pub fn verification_key(&self) -> ManifestVerificationKey {
        let public_key = self.key_pair.public_key().as_ref().to_vec();
        let key_id = format!("{:x}", Sha256::digest(&public_key));
        ManifestVerificationKey { key_id, public_key }
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn decode_hex(encoded: &str) -> Option<Vec<u8>> {
    if !encoded.len().is_multiple_of(2) {
        return None;
    }
    encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_value(*pair.first()?)?;
            let low = hex_value(*pair.get(1)?)?;
            Some((high << 4) | low)
        })
        .collect()
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Computes the SHA-256 digest of the canonical ordered `oid TAB name LF` ref stream.
#[must_use]
pub fn canonical_ref_digest(refs: &[RefEvidence]) -> String {
    let mut digest = Sha256::new();
    for reference in refs {
        digest.update(reference.oid.as_bytes());
        digest.update(b"\t");
        digest.update(reference.name.as_bytes());
        digest.update(b"\n");
    }
    format!("{:x}", digest.finalize())
}
