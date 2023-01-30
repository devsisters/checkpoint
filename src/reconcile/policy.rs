use std::sync::Arc;

use kube::runtime::controller::Action;

use crate::types::policy::CronPolicy;

use super::ReconcilerContext;

#[derive(Debug, thiserror::Error)]
pub enum Error {}

pub async fn reconcile_cronpolicy(
    cron_policy: Arc<CronPolicy>,
    ctx: Arc<ReconcilerContext>,
) -> Result<Action, Error> {
    todo!()
}
