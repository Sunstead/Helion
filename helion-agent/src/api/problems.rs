//! Acknowledging problems and silencing notifications. Both change delivery
//! only: the event log keeps everything.

use crate::{
    api::{ record_action, Action },
    auth::Principal,
    error::AgentError,
    sample::host::unix_now,
    state::AppState,
};
use axum::{ extract::{ Path, State }, http::StatusCode, Extension, Json };
use helion_common::types::{ Silence, SilenceInput, SilenceScope };

/// The longest a silence with an end can be: a week. One meant to last longer
/// is one meant to be ended by hand.
const MAX_MINUTES: u32 = 7 * 24 * 60;

fn events(state: &AppState) -> Result<&crate::events::EventsHandle, AgentError> {
    state.events.as_ref().ok_or(AgentError::NotEnabled("events"))
}

/// The problem's service and title, for the event log.
fn describe(state: &AppState, key: &str) -> Result<(String, Option<String>), AgentError> {
    events(state)?
        .open_problems()
        .iter()
        .find(|p| p.key == key)
        .map(|p| (p.title.clone(), p.service.clone()))
        .ok_or_else(|| AgentError::NotFound(format!("no open problem {key}")))
}

pub async fn ack(
    State(state): State<AppState>,
    Extension(by): Extension<Principal>,
    Path(key): Path<String>
) -> Result<StatusCode, AgentError> {
    let (title, service) = describe(&state, &key)?;
    let result = events(&state)?.ack(&key, Some(&by.name)).await;
    let action = Action {
        kind: "problem_ack",
        done: format!("Acknowledged: {title}"),
        failed: format!("Couldn't acknowledge: {title}"),
        subject: key,
        service,
    };
    record_action(&state, &by, action, &result);
    result.map(|()| StatusCode::NO_CONTENT)
}

pub async fn unack(
    State(state): State<AppState>,
    Extension(by): Extension<Principal>,
    Path(key): Path<String>
) -> Result<StatusCode, AgentError> {
    let (title, service) = describe(&state, &key)?;
    let result = events(&state)?.ack(&key, None).await;
    let action = Action {
        kind: "problem_unack",
        done: format!("No longer acknowledged: {title}"),
        failed: format!("Couldn't clear the acknowledgement: {title}"),
        subject: key,
        service,
    };
    record_action(&state, &by, action, &result);
    result.map(|()| StatusCode::NO_CONTENT)
}

fn what(scope: SilenceScope, target: Option<&str>) -> String {
    match (scope, target) {
        (SilenceScope::Service, Some(t)) => t.to_string(),
        _ => "this node".to_string(),
    }
}

pub async fn silence(
    State(state): State<AppState>,
    Extension(by): Extension<Principal>,
    Json(input): Json<SilenceInput>
) -> Result<Json<Silence>, AgentError> {
    let events = events(&state)?;
    let target = input.target
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from);
    match (input.scope, &target) {
        (SilenceScope::Service, None) => {
            return Err(AgentError::BadRequest("a service silence needs the service".into()));
        }
        (SilenceScope::Node, Some(_)) => {
            return Err(AgentError::BadRequest("a node silence takes no target".into()));
        }
        _ => {}
    }
    if input.minutes.is_some_and(|m| m == 0 || m > MAX_MINUTES) {
        return Err(AgentError::BadRequest(format!("minutes must be between 1 and {MAX_MINUTES}")));
    }
    let now = unix_now();
    let reason = input.reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(|r| r.chars().take(200).collect());
    let silence = Silence {
        id: 0,
        scope: input.scope,
        until: input.minutes.map(|m| now + i64::from(m) * 60),
        reason,
        created_by: by.name.clone(),
        created_at: now,
        target: target.clone(),
    };
    let result = events.silence(silence).await;
    let name = what(input.scope, target.as_deref());
    let action = Action {
        kind: "silence",
        done: format!("Silenced {name}"),
        failed: format!("Couldn't silence {name}"),
        subject: name,
        service: target,
    };
    record_action(&state, &by, action, &result);
    result.map(Json)
}

pub async fn end_silence(
    State(state): State<AppState>,
    Extension(by): Extension<Principal>,
    Path(id): Path<i64>
) -> Result<StatusCode, AgentError> {
    let events = events(&state)?;
    let found = events
        .active_silences(unix_now())
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| AgentError::NotFound(format!("no silence {id} in force")))?;
    let result = events.end_silence(id).await;
    let name = what(found.scope, found.target.as_deref());
    let action = Action {
        kind: "silence_end",
        done: format!("Ended the silence on {name}"),
        failed: format!("Couldn't end the silence on {name}"),
        subject: name,
        service: found.target,
    };
    record_action(&state, &by, action, &result);
    result.map(|()| StatusCode::NO_CONTENT)
}
