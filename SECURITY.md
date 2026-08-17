# Security Policy for Ratatoskr Vault

Report vulnerabilities privately. Do not publish repository credentials, private clone URLs, manifests containing sensitive metadata, storage keys, restored private contents, or exploit repositories.

Security review is mandatory for process execution, Git configuration, hooks/filters, credential helpers, paths, symlinks, archives, storage backends, retention/deletion, restore targets, and network policy.

Baseline: dedicated unprivileged identity; no shell interpolation; sanitized environment; disabled hooks and unsafe filters; validated paths; quotas/timeouts/process-group cancellation; encrypted least-privilege credentials; immutable content-addressed artifacts; checksum/bundle/fsck verification; staged deletion with audit.
