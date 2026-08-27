## ADDED Requirements

### Requirement: Typed full-bundle and ref operations

The Git runner SHALL expose full-bundle creation and complete-ref enumeration only as typed,
allowlisted direct-argument operations. A bundle destination SHALL be a confined Vault-owned path;
ref names and object IDs emitted by Git SHALL remain opaque data and SHALL NOT be interpolated into
commands, paths, or storage keys.

#### Scenario: Full bundle receives no shell-shaped input

- **WHEN** a healthy mirror has ref names containing shell metacharacters and Vault creates its
  full bundle and enumerates its refs
- **THEN** Git receives only the typed all-ref operation and a confined destination, the bundle
  preserves the refs, and no embedded command is executed
