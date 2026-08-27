## ADDED Requirements

### Requirement: Git LFS execution configuration is explicit and finite

When Git LFS collection is enabled for the process, configuration SHALL require an absolute Git LFS executable path, a positive per-run staging-byte ceiling, a positive referenced-object-count ceiling, and a positive finite operation deadline. Unknown, relative, missing, or zero-valued settings MUST fail startup. The LFS byte limit MUST NOT exceed the existing per-mirror or global byte ceilings.

#### Scenario: Unsafe LFS settings fail startup

- **WHEN** LFS collection is configured with a relative executable path, zero staging bytes, zero object count, zero deadline, or a staging ceiling above the mirror quota
- **THEN** startup reports every violated setting before any Git or Git LFS process runs
