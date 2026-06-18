//! Task specification derived from user-authored ledger observations.

use crate::evidence::Denial;
use crate::ledger::{Authority, ObsId, ObservationKind, RealityLedger};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    pub content: String,
    pub source_obs: ObsId,
    pub created_at: DateTime<Utc>,
}

impl TaskSpec {
    pub fn from_user_observation(
        ledger: &RealityLedger,
        source_obs: ObsId,
    ) -> Result<Self, Denial> {
        let observation = ledger
            .get(source_obs)
            .ok_or_else(|| Denial::new(format!("unknown task observation {source_obs}")))?;
        if observation.authority != Authority::User {
            return Err(Denial::new("task spec must come from user authority"));
        }
        let ObservationKind::UserTask { content } = &observation.kind else {
            return Err(Denial::new("task spec source must be a user task"));
        };
        Ok(Self {
            content: content.clone(),
            source_obs,
            created_at: observation.ts,
        })
    }
}
