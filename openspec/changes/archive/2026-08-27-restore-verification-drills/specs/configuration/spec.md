## ADDED Requirements

### Requirement: Verification and restore policy is finite and strict

When scheduled verification is enabled, configuration SHALL require positive verification and drill
frequencies, a positive sample-size ceiling, a positive scratch-byte budget, a finite per-drill time
budget, and a bounded concurrency ceiling. It SHALL require an absolute scratch root distinct from
the configured live-mirror root and trusted manifest verification keys. Missing, zero, overlapping,
relative, or unknown settings MUST fail startup without echoing secret signing material.

#### Scenario: Unsafe restore roots fail configuration

- **WHEN** the restore scratch root equals or is lexically inside the live mirror root
- **THEN** configuration loading fails naming the scratch-root setting and prints no configured key material

#### Scenario: Zero schedule budget fails configuration

- **WHEN** scheduled drills are enabled with a zero sample size, byte budget, time budget, or concurrency limit
- **THEN** configuration loading reports every violated finite-budget rule before any job starts
