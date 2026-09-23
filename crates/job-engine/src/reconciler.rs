//! Reconciler: desired state in, running tasks out.
//!
//! One loop owns desired state:
//! `loop { desired = load(); diff(desired, running); start/stop/update }`.
//! Every scan checks its shutdown flag between chunks so a hot update stops
//! the old version promptly.

use std::collections::{HashMap, HashSet};

use sui_indexer_config::JobSpec;
use tracing::{info, warn};

/// A running job version tracked by the reconciler.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunningVersion {
    /// Job name.
    pub job: String,
    /// Running version.
    pub version: u32,
}

/// What the reconciler decided for one job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileAction {
    /// Start this version (not running, desired `active`).
    Start {
        /// Job name.
        job: String,
        /// Version to run.
        version: u32,
    },
    /// Stop this version (no longer desired).
    Stop {
        /// Job name.
        job: String,
        /// Version to stop.
        version: u32,
    },
    /// Old version superseded: stop it after the new one catches up.
    /// (Emitted as Stop for the old version alongside Start for the new one.)
    Replace {
        /// Job name.
        job: String,
        /// Version to stop.
        old: u32,
        /// Version to start.
        new: u32,
    },
}

/// Desired-state snapshot: `(job, version, desired)` per job.
#[derive(Debug, Clone, Default)]
pub struct DesiredState {
    entries: Vec<(String, u32, String)>,
}

impl DesiredState {
    /// Build from job specs (version + desired state per spec).
    #[must_use]
    pub fn from_specs(specs: &[JobSpec]) -> Self {
        Self {
            entries: specs
                .iter()
                .map(|s| (s.name.clone(), s.version, s.desired.clone()))
                .collect(),
        }
    }

    /// Active jobs only.
    pub fn active(&self) -> Vec<RunningVersion> {
        self.entries
            .iter()
            .filter(|(_, _, desired)| desired == "active")
            .map(|(job, version, _)| RunningVersion {
                job: job.clone(),
                version: *version,
            })
            .collect()
    }
}

/// Reconciler: diffs desired state against running tasks.
#[derive(Debug, Default)]
pub struct Reconciler {
    running: HashMap<RunningVersion, RunningState>,
}

/// Liveness of one running version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunningState {
    /// Task spawned and healthy.
    Running,
    /// Stop requested; waiting for the task to exit.
    Stopping,
}

impl Reconciler {
    /// Create an empty reconciler.
    #[must_use]
    pub fn new() -> Self {
        Self {
            running: HashMap::new(),
        }
    }

    /// Currently tracked versions.
    #[must_use]
    pub fn running(&self) -> HashSet<RunningVersion> {
        self.running.keys().cloned().collect()
    }

    /// Diff `desired` against running tasks and return the actions to take.
    ///
    /// Start actions come before stop actions so a replacement never leaves
    /// a job with zero versions during the swap (the alias still points at
    /// the old version until the new one catches up).
    pub fn reconcile(&mut self, desired: &DesiredState) -> Vec<ReconcileAction> {
        let want: HashSet<RunningVersion> = desired.active().into_iter().collect();
        let have: HashSet<RunningVersion> = self.running.keys().cloned().collect();
        let mut actions = Vec::new();

        // Group running versions by job to detect replacements.
        let mut running_by_job: HashMap<&str, Vec<u32>> = HashMap::new();
        for r in &have {
            running_by_job
                .entry(r.job.as_str())
                .or_default()
                .push(r.version);
        }

        for version in &want {
            if !have.contains(version) {
                let olds: Vec<u32> = running_by_job
                    .get(version.job.as_str())
                    .cloned()
                    .unwrap_or_default();
                if olds.is_empty() {
                    actions.push(ReconcileAction::Start {
                        job: version.job.clone(),
                        version: version.version,
                    });
                } else {
                    for old in olds {
                        actions.push(ReconcileAction::Replace {
                            job: version.job.clone(),
                            old,
                            new: version.version,
                        });
                    }
                }
                self.running.insert(version.clone(), RunningState::Running);
                info!(job = %version.job, version = version.version, "job version started");
            }
        }
        for version in &have {
            if !want.contains(version)
                && !actions.iter().any(|a| match a {
                    ReconcileAction::Replace { job, old, .. } => {
                        job == &version.job && old == &version.version
                    }
                    _ => false,
                })
            {
                actions.push(ReconcileAction::Stop {
                    job: version.job.clone(),
                    version: version.version,
                });
                self.running.remove(version);
                warn!(job = %version.job, version = version.version, "job version stopped");
            }
        }
        // Starts/replaces before stops.
        actions.sort_by_key(|a| match a {
            ReconcileAction::Start { .. } | ReconcileAction::Replace { .. } => 0,
            ReconcileAction::Stop { .. } => 1,
        });
        actions
    }

    /// Mark a version stopped (called when its task exits).
    pub fn mark_stopped(&mut self, version: &RunningVersion) {
        self.running.remove(version);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sui_indexer_config::OutputConfig;

    fn spec(name: &str, version: u32, desired: &str) -> JobSpec {
        JobSpec {
            name: name.to_owned(),
            version,
            output: OutputConfig {
                table: format!("job_{name}"),
                ..OutputConfig::default()
            },
            sql: "SELECT 1".to_owned(),
            desired: desired.to_owned(),
            ..JobSpec::default()
        }
    }

    #[test]
    fn empty_desired_starts_nothing() {
        let mut reconciler = Reconciler::new();
        let actions = reconciler.reconcile(&DesiredState::from_specs(&[]));
        assert!(actions.is_empty());
    }

    #[test]
    fn new_active_job_starts() {
        let mut reconciler = Reconciler::new();
        let actions = reconciler.reconcile(&DesiredState::from_specs(&[spec("a", 1, "active")]));
        assert_eq!(
            actions,
            vec![ReconcileAction::Start {
                job: "a".to_owned(),
                version: 1
            }]
        );
        assert!(reconciler.running().contains(&RunningVersion {
            job: "a".to_owned(),
            version: 1,
        }));
        // Second tick is quiet.
        let again = reconciler.reconcile(&DesiredState::from_specs(&[spec("a", 1, "active")]));
        assert!(again.is_empty());
    }

    #[test]
    fn mark_stopped_forgets_the_version() {
        let mut reconciler = Reconciler::new();
        reconciler.reconcile(&DesiredState::from_specs(&[spec("a", 1, "active")]));
        let version = RunningVersion {
            job: "a".to_owned(),
            version: 1,
        };
        reconciler.mark_stopped(&version);
        assert!(!reconciler.running().contains(&version));
        // A forgotten version restarts on the next tick.
        let actions = reconciler.reconcile(&DesiredState::from_specs(&[spec("a", 1, "active")]));
        assert_eq!(
            actions,
            vec![ReconcileAction::Start {
                job: "a".to_owned(),
                version: 1
            }]
        );
    }

    #[test]
    fn replace_for_one_job_does_not_suppress_other_stops() {
        let mut reconciler = Reconciler::new();
        reconciler.reconcile(&DesiredState::from_specs(&[
            spec("a", 9, "active"),
            spec("b", 9, "active"),
        ]));
        // b bumps to v10 while a is removed: both actions fire.
        let actions = reconciler.reconcile(&DesiredState::from_specs(&[spec("b", 10, "active")]));
        assert_eq!(
            actions,
            vec![
                ReconcileAction::Replace {
                    job: "b".to_owned(),
                    old: 9,
                    new: 10,
                },
                ReconcileAction::Stop {
                    job: "a".to_owned(),
                    version: 9,
                },
            ]
        );
    }

    #[test]
    fn removed_job_stops_without_restart() {
        let mut reconciler = Reconciler::new();
        reconciler.reconcile(&DesiredState::from_specs(&[spec("a", 1, "active")]));
        let actions = reconciler.reconcile(&DesiredState::from_specs(&[]));
        assert_eq!(
            actions,
            vec![ReconcileAction::Stop {
                job: "a".to_owned(),
                version: 1
            }]
        );
    }

    #[test]
    fn version_bump_replaces_old_with_new() {
        let mut reconciler = Reconciler::new();
        reconciler.reconcile(&DesiredState::from_specs(&[spec("a", 1, "active")]));
        let actions = reconciler.reconcile(&DesiredState::from_specs(&[spec("a", 2, "active")]));
        assert_eq!(
            actions,
            vec![ReconcileAction::Replace {
                job: "a".to_owned(),
                old: 1,
                new: 2
            }]
        );
    }

    #[test]
    fn paused_job_stops_and_resume_restarts() {
        let mut reconciler = Reconciler::new();
        reconciler.reconcile(&DesiredState::from_specs(&[spec("a", 1, "active")]));
        let pause = reconciler.reconcile(&DesiredState::from_specs(&[spec("a", 1, "paused")]));
        assert_eq!(
            pause,
            vec![ReconcileAction::Stop {
                job: "a".to_owned(),
                version: 1
            }]
        );
        let resume = reconciler.reconcile(&DesiredState::from_specs(&[spec("a", 1, "active")]));
        assert_eq!(
            resume,
            vec![ReconcileAction::Start {
                job: "a".to_owned(),
                version: 1
            }]
        );
    }
}
