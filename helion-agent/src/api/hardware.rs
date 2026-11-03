use crate::{ api::CachedJson, error::AgentError, state::AppState };
use axum::extract::State;

/// 501 without `[hardware] disk_health`, so the app hides the drives.
pub async fn disks(State(state): State<AppState>) -> Result<CachedJson, AgentError> {
    let rx = state.disk_health_rx.as_ref().ok_or(AgentError::NotEnabled("disk health"))?;
    let json = rx.borrow().json.clone();
    Ok(CachedJson(json))
}
