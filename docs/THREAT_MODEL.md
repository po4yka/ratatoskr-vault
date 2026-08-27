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
- **Off-host credential/endpoint abuse:** credentials exist only in environment-backed secret fields; endpoints reject user information/query/fragment and plaintext non-loopback HTTP; object keys derive only from immutable digests.
- **Remote outage or stalled multipart:** independent finite worker permits/deadlines, bounded durable backlog, closed failure classes, cooperative multipart abort, expiring recoverable leases, and unchanged local verification evidence.
- **Accidental/destructive deletion:** pin precedence, grace period, tombstone, approval, replica checks, audit.
- **Cross-user restore leak:** owner authorization and isolated destination.

Remote bucket policy, encryption, versioning, Object Lock, and lifecycle configuration remain operator controls and require a real-provider smoke test. Lifecycle may clean incomplete multipart uploads only after the longest Vault attempt; it must not expire completed objects before Vault retention authorizes deletion. Vault performs no remote deletion in item 7.

Re-review for new collectors, arbitrary submodule recursion, server-side restore download, remote executors, WORM storage, or automatic physical deletion.
