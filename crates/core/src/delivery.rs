//! The desired-state delivery boundary: raw records enter, validated records leave.
//!
//! Vault defines its own input contract (design D1): the live publisher does not exist yet, and
//! importing catalog types would couple this crate to star/list models the bounded-context rules
//! forbid. Deliveries arrive shaped like messages, are validated once, and only the validated
//! form may drive planning or persistence.

use crate::error::VaultError;

/// A desired-state record exactly as delivered, before any validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredStateDelivery {
    /// Preservation level exactly as delivered; must name one of the five policy levels.
    pub preservation_level: String,
    /// Whether the target resists automatic exclusion; absent means the delivery omitted it.
    pub pinned: Option<bool>,
    /// Whether the wiki repository is included; absent means the delivery omitted it.
    pub include_wiki: Option<bool>,
    /// Whether releases and their assets are included; absent means the delivery omitted it.
    pub include_releases: Option<bool>,
    /// Whether issues and comments are included; absent means the delivery omitted it.
    pub include_issues: Option<bool>,
    /// Whether an off-host copy is required; absent means the delivery omitted it.
    pub offsite_required: Option<bool>,
    /// Identifier correlating this delivery with its origin request.
    pub correlation_id: String,
    /// Monotonic per-target revision; an absent revision cannot be ordered against stored state.
    pub policy_revision: Option<u64>,
}

/// The validated form of a delivery; only this may drive planning and persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedDelivery {
    /// Preservation level exactly as delivered.
    pub preservation_level: String,
    /// Whether the target resists automatic exclusion.
    pub pinned: Option<bool>,
    /// Whether the wiki repository is included.
    pub include_wiki: Option<bool>,
    /// Whether releases and their assets are included.
    pub include_releases: Option<bool>,
    /// Whether issues and comments are included.
    pub include_issues: Option<bool>,
    /// Whether an off-host copy is required.
    pub offsite_required: Option<bool>,
    /// Identifier correlating this delivery with its origin request.
    pub correlation_id: String,
    /// Monotonic per-target revision.
    pub policy_revision: Option<u64>,
}

/// Validates a delivered desired-state record against the input contract.
///
/// A refusal names the rejected field: an unknown `preservation_level`, a missing
/// `policy_revision`, or a blank `correlation_id` each yield [`VaultError::InvalidDelivery`].
///
/// # Errors
///
/// Returns [`VaultError::InvalidDelivery`] naming the first rejected field, in contract order:
/// `preservation_level`, then `policy_revision`, then `correlation_id`.
///
/// The optional inclusion flags pass through unresolved; resolving their defaults is a concern
/// of the stage that consumes the validated record, not of validity itself.
pub fn validate_delivery(delivery: &DesiredStateDelivery) -> Result<ValidatedDelivery, VaultError> {
    const PRESERVATION_LEVELS: [&str; 5] = [
        "none",
        "metadata_only",
        "git_mirror",
        "git_mirror_with_lfs",
        "complete_archive",
    ];

    if !PRESERVATION_LEVELS.contains(&delivery.preservation_level.as_str()) {
        return Err(VaultError::InvalidDelivery {
            field: "preservation_level",
        });
    }
    if delivery.policy_revision.is_none() {
        return Err(VaultError::InvalidDelivery {
            field: "policy_revision",
        });
    }
    if delivery.correlation_id.is_empty() {
        return Err(VaultError::InvalidDelivery {
            field: "correlation_id",
        });
    }

    Ok(ValidatedDelivery {
        preservation_level: delivery.preservation_level.clone(),
        pinned: delivery.pinned,
        include_wiki: delivery.include_wiki,
        include_releases: delivery.include_releases,
        include_issues: delivery.include_issues,
        offsite_required: delivery.offsite_required,
        correlation_id: delivery.correlation_id.clone(),
        policy_revision: delivery.policy_revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_delivery() -> DesiredStateDelivery {
        DesiredStateDelivery {
            preservation_level: "git_mirror".to_owned(),
            pinned: Some(false),
            include_wiki: Some(false),
            include_releases: Some(true),
            include_issues: Some(true),
            offsite_required: Some(true),
            correlation_id: "corr-1".to_owned(),
            policy_revision: Some(7),
        }
    }

    #[test]
    fn malformed_deliveries_are_rejected() {
        let mut unknown_level = valid_delivery();
        unknown_level.preservation_level = "starred".to_owned();

        let mut missing_revision = valid_delivery();
        missing_revision.policy_revision = None;

        let mut blank_correlation = valid_delivery();
        blank_correlation.correlation_id = String::new();

        assert_eq!(
            validate_delivery(&unknown_level),
            Err(VaultError::InvalidDelivery {
                field: "preservation_level"
            })
        );
        assert_eq!(
            validate_delivery(&missing_revision),
            Err(VaultError::InvalidDelivery {
                field: "policy_revision"
            })
        );
        assert_eq!(
            validate_delivery(&blank_correlation),
            Err(VaultError::InvalidDelivery {
                field: "correlation_id"
            })
        );
    }
}
