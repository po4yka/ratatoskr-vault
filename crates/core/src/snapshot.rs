//! Immutable snapshot manifest evidence.

use sha2::{Digest, Sha256};

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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// SHA-256 digest of canonical refs.
    pub ref_set_sha256: String,
}

/// One ref entry in the manifest.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefEvidence {
    /// Ref name.
    pub name: String,
    /// Object ID named by the ref.
    pub oid: String,
}

impl SnapshotManifest {
    /// Builds a canonically ordered manifest from full ref evidence.
    #[must_use]
    pub fn new(
        mut refs: Vec<RefEvidence>,
        bundles: Vec<BlobRef>,
        parent_manifest: Option<BlobRef>,
        created_at: String,
    ) -> Self {
        refs.sort_by(|left, right| left.name.cmp(&right.name));
        let ref_set_sha256 = canonical_ref_digest(&refs);
        Self {
            schema_version: 1,
            refs,
            generator_version: env!("CARGO_PKG_VERSION").to_owned(),
            created_at,
            parent_manifest,
            bundles,
            ref_set_sha256,
        }
    }
}

fn canonical_ref_digest(refs: &[RefEvidence]) -> String {
    let mut digest = Sha256::new();
    for reference in refs {
        digest.update(reference.oid.as_bytes());
        digest.update(b"\t");
        digest.update(reference.name.as_bytes());
        digest.update(b"\n");
    }
    format!("{:x}", digest.finalize())
}
