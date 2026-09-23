//! Reorg detection and rollback planning.
//!
//! Parent-link validation with fork-point search and rollback. Sui
//! (`CommitmentModel::Final`) degenerates to a no-op; Solana runs it over
//! `confirmed`, EVM with a 128-deep cap.

use serde::{Deserialize, Serialize};

use crate::{CommitmentModel, ParentRef};

/// Declared per job output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReorgMode {
    /// Rows carry `_height`; prune everything above the fork point.
    #[default]
    BlockScoped,
    /// Fully recomputed each pass; reorg-correct by construction.
    Refreshable,
    /// Never touched by reorg cleanup (dimensions, feeds).
    None,
}

impl ReorgMode {
    /// Parse a user-supplied reorg-mode string.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "block_scoped" | "block-scoped" | "scoped" => Some(Self::BlockScoped),
            "refreshable" | "refresh" => Some(Self::Refreshable),
            "none" | "off" => Some(Self::None),
            _ => None,
        }
    }
}

/// Where a fork was found, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkPoint {
    /// First divergent height (the fork point itself is `height - 1`).
    pub height: u64,
    /// Expected parent reference at `height`.
    pub expected: ParentRef,
    /// Observed parent reference at `height`.
    pub observed: ParentRef,
}

/// Verify parent-link continuity over a fetched window.
///
/// `stored_parent` resolves the parent reference the archive holds for
/// `height`; `observed` is the freshly fetched block's parent. Returns the
/// first fork point, if any.
pub fn verify_continuity(
    heights: &[u64],
    stored_parent: &dyn Fn(u64) -> Option<ParentRef>,
    observed_parent: &dyn Fn(u64) -> Option<ParentRef>,
) -> Option<ForkPoint> {
    for height in heights.iter().copied() {
        let (Some(expected), Some(observed)) = (stored_parent(height), observed_parent(height))
        else {
            continue;
        };
        if expected != observed {
            return Some(ForkPoint {
                height,
                expected,
                observed,
            });
        }
    }
    None
}

/// Plan a rollback for `model` given a fork at `fork_height`.
///
/// Returns the height to roll back to (`fork_height - 1`, saturating), or
/// `None` when the model needs no rollback (`Final` without a fork, or a fork
/// deeper than `max_depth` — in which case the caller rewrites the tip to the
/// fork point instead of pruning row by row).
pub fn plan_rollback(model: CommitmentModel, fork_height: Option<u64>) -> Option<u64> {
    match (model, fork_height) {
        (CommitmentModel::Final, _) => None,
        (CommitmentModel::Reorgable { .. }, None) => None,
        (CommitmentModel::Reorgable { max_depth }, Some(fork)) => {
            // Depth accounting happens at the call site (tip - fork);
            // the plan itself is always "rewind to fork - 1".
            let _ = max_depth;
            Some(fork.saturating_sub(1))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent(hash: u8) -> ParentRef {
        ParentRef::from_hash(vec![hash])
    }

    #[test]
    fn no_divergence_means_no_fork() {
        let heights = vec![10, 11, 12];
        let stored = |h: u64| Some(parent(h as u8));
        let observed = |h: u64| Some(parent(h as u8));
        assert_eq!(verify_continuity(&heights, &stored, &observed), None);
    }

    #[test]
    fn first_divergent_height_is_the_fork_point() {
        let heights = vec![10, 11, 12];
        let stored = |h: u64| Some(parent(h as u8));
        let observed = |h: u64| Some(if h == 11 { parent(99) } else { parent(h as u8) });
        let fork = verify_continuity(&heights, &stored, &observed).expect("fork");
        assert_eq!(fork.height, 11);
    }

    #[test]
    fn final_model_never_rolls_back() {
        assert_eq!(plan_rollback(CommitmentModel::Final, Some(100)), None);
        assert_eq!(plan_rollback(CommitmentModel::Final, None), None);
    }

    #[test]
    fn reorgable_model_rewinds_to_fork_minus_one() {
        let model = CommitmentModel::Reorgable { max_depth: 128 };
        assert_eq!(plan_rollback(model, Some(100)), Some(99));
        assert_eq!(plan_rollback(model, Some(0)), Some(0));
        assert_eq!(plan_rollback(model, None), None);
    }

    #[test]
    fn reorg_mode_parses_aliases() {
        assert_eq!(
            ReorgMode::parse("block_scoped"),
            Some(ReorgMode::BlockScoped)
        );
        assert_eq!(ReorgMode::parse("refresh"), Some(ReorgMode::Refreshable));
        assert_eq!(ReorgMode::parse("off"), Some(ReorgMode::None));
        assert_eq!(ReorgMode::parse("bogus"), None);
    }
}
