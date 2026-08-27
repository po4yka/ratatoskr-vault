## MODIFIED Requirements

### Requirement: Manifest is self-contained immutable snapshot evidence

Every published snapshot manifest SHALL include its current schema/generator version, creation
timestamp, optional parent-snapshot manifest BlobRef, complete ref list and digest, each bundle
BlobRef with its cryptographic digest and byte length, a signing-key identifier, and a signature over
the canonical unsigned manifest bytes. The parent BlobRef SHALL form a verifiable digest chain to an
explicit trust anchor. The manifest bytes themselves SHALL be published as an immutable
content-addressed BlobRef. Verification and restore outcomes SHALL remain separate append-only
reports and MUST NOT be rewritten into the signed snapshot manifest.

#### Scenario: Manifest names every required evidence field

- **WHEN** Vault publishes a snapshot from a healthy fixture mirror
- **THEN** resolving the manifest BlobRef yields the generator version, timestamp, parent linkage,
  signing-key identity and verifiable signature, exact ref evidence, and the digest, media type, and
  byte length of its bundle BlobRef

#### Scenario: Parent linkage is a cryptographic chain

- **WHEN** a signed child manifest names a parent manifest
- **THEN** verification resolves the exact parent BlobRef, verifies its digest and signature, and continues until the configured trust anchor is reached without rewriting either manifest

## ADDED Requirements

### Requirement: Snapshot verification is evidence, not a metadata claim

A stored-byte verification SHALL be called passed only after manifest and bundle bytes pass digest
and size checks and the signature and parent digest chain reach a trusted anchor. A restore drill
SHALL be called passed only after Git verifies the bundle, reconstructs it, checks connectivity, and
matches refs. A later failed attempt SHALL append failure evidence without deleting the snapshot or
changing a prior successful report.

#### Scenario: Metadata cannot convert corruption to success

- **WHEN** stored bundle bytes fail their manifest digest check
- **THEN** no database-only status update can produce verified evidence and the mismatch remains a failed verification report
