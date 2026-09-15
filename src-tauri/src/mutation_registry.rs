use std::{collections::HashSet, sync::Mutex};

use serde::Serialize;

use crate::{change_sets::RepositoryChanges, error::OrbitError};

pub const MAX_CONCURRENT_MUTATIONS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MutationOperation {
    StageFile,
    UnstageFile,
    StageAll,
    UnstageAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MutationOutcome {
    Applied,
    Rejected,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationReceipt {
    pub operation: MutationOperation,
    pub outcome: MutationOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<OrbitError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_changes: Option<RepositoryChanges>,
    pub refresh_required: bool,
    pub head_changed: bool,
}

#[derive(Debug, Default)]
pub struct MutationRegistry {
    in_flight: Mutex<HashSet<String>>,
}

#[derive(Debug)]
pub struct MutationLease<'a> {
    registry: &'a MutationRegistry,
    repository_id: String,
}

impl MutationRegistry {
    pub fn acquire(&self, repository_id: &str) -> Result<MutationLease<'_>, OrbitError> {
        let mut in_flight = self.in_flight.lock().map_err(|_| {
            OrbitError::internal(
                "mutate_repository",
                "Repository mutation state is unavailable.",
            )
        })?;

        if in_flight.contains(repository_id) || in_flight.len() >= MAX_CONCURRENT_MUTATIONS {
            return Err(OrbitError::mutation_in_progress());
        }

        in_flight.insert(repository_id.to_owned());
        Ok(MutationLease {
            registry: self,
            repository_id: repository_id.to_owned(),
        })
    }

    fn release(&self, repository_id: &str) {
        if let Ok(mut in_flight) = self.in_flight.lock() {
            in_flight.remove(repository_id);
        }
    }
}

impl Drop for MutationLease<'_> {
    fn drop(&mut self) {
        self.registry.release(&self.repository_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_each_repository_and_releases_with_raii() {
        let registry = MutationRegistry::default();
        let lease = registry
            .acquire("repository-0000000000000001")
            .expect("first lease");
        let error = registry
            .acquire("repository-0000000000000001")
            .expect_err("duplicate repository lease");
        assert_eq!(error.code, "mutation_in_progress");

        drop(lease);
        registry
            .acquire("repository-0000000000000001")
            .expect("lease should be released");
    }

    #[test]
    fn enforces_the_process_wide_bound_without_evicting_active_leases() {
        let registry = MutationRegistry::default();
        let leases = (0..MAX_CONCURRENT_MUTATIONS)
            .map(|index| {
                registry
                    .acquire(&format!("repository-{index:016x}"))
                    .expect("bounded lease")
            })
            .collect::<Vec<_>>();

        let error = registry
            .acquire("repository-000000000000ffff")
            .expect_err("fifth lease must be rejected");
        assert_eq!(error.code, "mutation_in_progress");
        assert_eq!(
            registry.in_flight.lock().expect("registry state").len(),
            MAX_CONCURRENT_MUTATIONS
        );

        drop(leases);
        registry
            .acquire("repository-000000000000ffff")
            .expect("capacity should return after drop");
    }
}
