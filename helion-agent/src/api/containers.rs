use crate::{
    api::{ record_action, Action, CachedJson },
    auth::Principal,
    error::AgentError,
    sse,
    state::AppState,
};
use axum::{
    extract::{ Path, Query, State },
    response::sse::{ Event, Sse },
    Extension,
    Json,
};
use helion_common::types::{ ContainerActionRequest, ContainerActionResult };
use futures_util::Stream;
use serde::Deserialize;
use std::convert::Infallible;

pub async fn list(State(state): State<AppState>) -> Result<CachedJson, AgentError> {
    // 503 when Docker is down, so it isn't mistaken for "no containers".
    state.docker.require()?;
    Ok(CachedJson(state.containers_rx.borrow().json.clone()))
}

pub async fn stream(
    State(state): State<AppState>
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    sse::stream_watch(state.containers_rx.clone(), |snap| snap.json.clone(), state.shutdown.clone())
}

/// The container's name and service, for the event log. Looked up before
/// acting, since a removed container is gone from the list after.
fn describe(state: &AppState, id: &str) -> (String, Option<String>) {
    let snap = state.containers_rx.borrow();
    snap.containers
        .iter()
        .find(|c| c.id == id || c.name == id || (id.len() >= 12 && c.id.starts_with(id)))
        .map(|c| (c.name.clone(), c.helion_service.clone()))
        .unwrap_or_else(|| (id.chars().take(12).collect(), None))
}

fn action(state: &AppState, id: &str, kind: &'static str, verb: &str, past: &str) -> Action {
    let (name, service) = describe(state, id);
    Action {
        kind,
        done: format!("{past} {name}"),
        failed: format!("Couldn't {verb} {name}"),
        subject: name,
        service,
    }
}

/// A stop or removal through Helion is deliberate, so the service it leaves
/// without a running container isn't reported missing.
fn stopped_on_purpose<T>(state: &AppState, by: &Principal, action: &Action, result: &Result<T, AgentError>) {
    if let (Ok(_), Some(services), Some(service)) = (result, &state.services, &action.service) {
        services.stopped(service, &by.name);
    }
}

/// `timeout_secs` is optional in the body; an absent body is fine too.
fn timeout_of(body: Option<Json<ContainerActionRequest>>) -> Option<u32> {
    body.and_then(|Json(b)| b.timeout_secs)
}

pub async fn start(
    State(state): State<AppState>,
    Extension(by): Extension<Principal>,
    Path(id): Path<String>
) -> Result<Json<ContainerActionResult>, AgentError> {
    let action = action(&state, &id, "container_start", "start", "Started");
    let result = state.docker.start(&id).await;
    record_action(&state, &by, action, &result);
    result.map(Json)
}

pub async fn stop(
    State(state): State<AppState>,
    Extension(by): Extension<Principal>,
    Path(id): Path<String>,
    body: Option<Json<ContainerActionRequest>>
) -> Result<Json<ContainerActionResult>, AgentError> {
    let action = action(&state, &id, "container_stop", "stop", "Stopped");
    let result = state.docker.stop(&id, timeout_of(body)).await;
    stopped_on_purpose(&state, &by, &action, &result);
    record_action(&state, &by, action, &result);
    result.map(Json)
}

pub async fn restart(
    State(state): State<AppState>,
    Extension(by): Extension<Principal>,
    Path(id): Path<String>,
    body: Option<Json<ContainerActionRequest>>
) -> Result<Json<ContainerActionResult>, AgentError> {
    let action = action(&state, &id, "container_restart", "restart", "Restarted");
    let result = state.docker.restart(&id, timeout_of(body)).await;
    record_action(&state, &by, action, &result);
    result.map(Json)
}

#[derive(Deserialize, Default)]
pub struct RemoveQuery {
    #[serde(default)]
    force: bool,
    /// Also remove anonymous volumes attached to the container.
    #[serde(default)]
    volumes: bool,
}

pub async fn remove(
    State(state): State<AppState>,
    Extension(by): Extension<Principal>,
    Path(id): Path<String>,
    Query(q): Query<RemoveQuery>
) -> Result<Json<ContainerActionResult>, AgentError> {
    let action = action(&state, &id, "container_remove", "remove", "Removed");
    let result = state.docker.remove(&id, q.force, q.volumes).await;
    stopped_on_purpose(&state, &by, &action, &result);
    record_action(&state, &by, action, &result);
    result.map(Json)
}
