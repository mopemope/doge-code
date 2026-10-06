use std::collections::{BTreeMap, BTreeSet};

use super::model::{Change, ChangeState, FileMatch, ReviewChange, Verification};
use crate::features::verification_snapshot::{CurrentState, RunState};

pub(super) fn build(changes: &[Change], verifications: &[Verification]) -> Vec<ReviewChange> {
    // Index only explicit recorded links; never infer ownership from file names.
    let mut by_change: BTreeMap<&str, Vec<&Verification>> = BTreeMap::new();
    for verification in verifications {
        for id in verification
            .observed_change_ids
            .iter()
            .collect::<BTreeSet<_>>()
        {
            by_change.entry(id).or_default().push(verification);
        }
    }
    changes
        .iter()
        .map(|change| {
            let mut row = ReviewChange {
                change_id: change.id.clone(),
                matching_successful_observation_ids: vec![],
                failed_observation_ids: vec![],
                other_successful_observation_ids: vec![],
            };
            for verification in by_change.get(change.id.as_str()).into_iter().flatten() {
                if !verification.outcome.success {
                    // Retain every failure, including older, stale and timed-out runs.
                    row.failed_observation_ids.push(verification.id.clone());
                } else if change.lifecycle_state == ChangeState::Active
                    && change.current_file_match == FileMatch::Matched
                    && verification
                        .execution_workspace
                        .as_ref()
                        .is_some_and(|workspace| workspace.run_state == RunState::StableEndpoints)
                    && verification.current_code_state.state == CurrentState::MatchesStart
                {
                    row.matching_successful_observation_ids
                        .push(verification.id.clone());
                } else {
                    row.other_successful_observation_ids
                        .push(verification.id.clone());
                }
            }
            row
        })
        .collect()
}
