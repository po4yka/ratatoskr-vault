## ADDED Requirements

### Requirement: Off-host replica configuration is strict, finite, and environment-only

Each replica target SHALL be configured through the existing `RATATOSKR__` environment-only tree with a stable target name, HTTPS S3-compatible endpoint, bucket, region, optional object-key prefix, access-key secret, secret-access-key secret, and optional session-token secret. The configuration SHALL also require positive transfer deadlines, a positive byte ceiling, and bounded backlog and concurrency limits. Plain HTTP MUST be refused except for a loopback test endpoint. Vault MUST construct the client from these explicit values and MUST NOT consult credential files, instance metadata, container metadata, or an ambient provider credential chain.

#### Scenario: Missing credentials and zero limits fail startup without leakage

- **WHEN** a replica target omits a credential or sets any transfer, byte, backlog, or concurrency limit to zero
- **THEN** startup reports every invalid setting by key and environment-variable name without rendering endpoint credentials or secret values

#### Scenario: Non-loopback plaintext endpoint is refused

- **WHEN** a replica target uses an HTTP endpoint whose host is not loopback
- **THEN** startup fails before creating an S3 client and names the endpoint rule without echoing credentials
