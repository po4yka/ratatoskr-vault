# Vault threat model

## Assets

Private repository content, credentials, host filesystem/network, Git process integrity, snapshot/off-host integrity, retention policy, and restore evidence.

## Threats and controls

- **Malicious hooks/filters/config/helpers:** sanitized environment/config, disabled hooks/filters, no arbitrary helper execution.
- **Path traversal/symlink/device abuse:** canonical validated roots, no source-derived filesystem names, safe archive handling.
- **Resource bomb:** disk/object/ref/file/process/time/network quotas and backpressure.
- **Command injection:** no shell; fixed executable and validated args.
- **Credential exfiltration:** scoped non-interactive credentials, isolated process, redacted output, restricted egress where feasible.
- **Corrupt/tampered backup:** cryptographic hashes, Git verification, immutable replicas, manifest comparison, restore drills.
- **Accidental/destructive deletion:** pin precedence, grace period, tombstone, approval, replica checks, audit.
- **Cross-user restore leak:** owner authorization and isolated destination.

Re-review for new collectors, arbitrary submodule recursion, server-side restore download, remote executors, WORM storage, or automatic physical deletion.
