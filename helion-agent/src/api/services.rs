use crate::{ api::{ record_action, Action }, auth::Principal, error::AgentError, state::AppState };
use axum::{ extract::{ Path, State }, Extension, Json };
use helion_common::types::ExpectedServicesResponse;

/// 501 without the state database, so the app shows no expectations.
pub async fn list(State(state): State<AppState>) -> Result<Json<ExpectedServicesResponse>, AgentError> {
    let services = state.services.as_ref().ok_or(AgentError::NotEnabled("expected services"))?;
    let list = services.list.borrow().to_vec();
    Ok(Json(ExpectedServicesResponse { services: list }))
}

/// Stops expecting a service. It's learned again if it runs for a while.
pub async fn forget(
    State(state): State<AppState>,
    Extension(by): Extension<Principal>,
    Path(service): Path<String>
) -> Result<Json<ExpectedServicesResponse>, AgentError> {
    let services = state.services.as_ref().ok_or(AgentError::NotEnabled("expected services"))?;
    let result = services.forget(&service).await;
    let action = Action {
        kind: "service_forget",
        done: format!("Stopped expecting {service}"),
        failed: format!("Couldn't stop expecting {service}"),
        subject: service.clone(),
        service: Some(service),
    };
    record_action(&state, &by, action, &result);
    result?;
    Ok(Json(ExpectedServicesResponse { services: services.list.borrow().to_vec() }))
}
