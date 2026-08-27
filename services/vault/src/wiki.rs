//! Wiki sibling discovery followed by the ordinary independently quota-admitted mirror lifecycle.

use ratatoskr_vault_core::error::VaultError;
use ratatoskr_vault_gitrunner::{GitRunner, SourceUrl, WikiProbeResult};
use ratatoskr_vault_persistence::{Database, WikiDiscovery};
use uuid::Uuid;

use crate::mirror_lifecycle::{LifecycleOutcome, MirrorLifecycle, MirrorRequest};

/// Terminal result of one policy-authorized wiki discovery pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikiMirrorOutcome {
    /// The provider confirmed that the sibling does not exist.
    Absent,
    /// Discovery failed closed and no target was enrolled.
    Failed,
    /// The sibling was enrolled and passed through the shared mirror lifecycle.
    Mirrored {
        /// Stable sibling target identity.
        child_target_id: Uuid,
        /// Independent mirror result and evidence.
        lifecycle: LifecycleOutcome,
    },
}

/// Coordinates a typed no-checkout probe with durable sibling enrollment and shared mirroring.
#[derive(Debug, Clone)]
pub struct WikiCoordinator {
    database: Database,
    probe_runner: GitRunner,
    lifecycle: MirrorLifecycle,
}

impl WikiCoordinator {
    /// Creates a coordinator from already bounded execution components.
    #[must_use]
    pub const fn new(
        database: Database,
        probe_runner: GitRunner,
        lifecycle: MirrorLifecycle,
    ) -> Self {
        Self {
            database,
            probe_runner,
            lifecycle,
        }
    }

    /// Discovers and, when present, mirrors the wiki as a sibling target.
    ///
    /// The caller invokes this only after successful parent access and explicit wiki policy.
    /// Ambiguous, authentication, timeout, and transport failures are never treated as absence.
    ///
    /// # Errors
    ///
    /// Returns invalid delivery for an unsafe source or storage failure when terminal evidence
    /// cannot commit.
    pub async fn discover_and_mirror(
        &self,
        parent_target_id: Uuid,
        parent_source: &str,
        reservation_bytes: u64,
    ) -> Result<WikiMirrorOutcome, VaultError> {
        let parent = SourceUrl::parse(parent_source).map_err(|_| VaultError::InvalidDelivery {
            field: "source_url",
        })?;
        match self.probe_runner.probe_wiki(&parent, true).await {
            WikiProbeResult::Present { .. } => {
                let sibling = parent
                    .wiki_sibling()
                    .map_err(|_| VaultError::InvalidDelivery {
                        field: "source_url",
                    })?;
                let record = self
                    .database
                    .record_wiki_discovery(parent_target_id, WikiDiscovery::Present)
                    .await?;
                let child_target_id = record.child_target_id.ok_or(VaultError::StorageFailed)?;
                let lifecycle = self
                    .lifecycle
                    .run(MirrorRequest::new(
                        child_target_id,
                        sibling.as_str().to_owned(),
                        reservation_bytes,
                    ))
                    .await?;
                Ok(WikiMirrorOutcome::Mirrored {
                    child_target_id,
                    lifecycle,
                })
            }
            WikiProbeResult::Absent => {
                self.database
                    .record_wiki_discovery(parent_target_id, WikiDiscovery::Absent)
                    .await?;
                Ok(WikiMirrorOutcome::Absent)
            }
            WikiProbeResult::AuthenticationFailed { .. } => {
                self.failed(parent_target_id, "auth").await
            }
            WikiProbeResult::TimedOut => self.failed(parent_target_id, "timeout").await,
            WikiProbeResult::OtherFailure { .. } => {
                self.failed(parent_target_id, "probe_failed").await
            }
        }
    }

    async fn failed(
        &self,
        parent_target_id: Uuid,
        failure_class: &'static str,
    ) -> Result<WikiMirrorOutcome, VaultError> {
        self.database
            .record_wiki_discovery(
                parent_target_id,
                WikiDiscovery::Failed {
                    failure_class: failure_class.to_owned(),
                },
            )
            .await?;
        Ok(WikiMirrorOutcome::Failed)
    }
}
