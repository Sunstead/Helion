use crate::{ error::AgentError, events::db, state::AppState };
use axum::{ extract::{ Path, Query, State }, Json };
use helion_common::types::{ EventLogs, EventsResponse };
use serde::Deserialize;

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;

#[derive(Deserialize, Default)]
pub struct EventsQuery {
    /// Events after this id, oldest first: what's new since the last poll.
    after: Option<i64>,
    /// Events before this id, newest first: an older page of the timeline.
    before: Option<i64>,
    limit: Option<usize>,
}

/// Reads SQLite on request, like `/v1/metrics`: a query over an indexed
/// column, not sampling. The open problems come from memory.
pub async fn list(
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>
) -> Result<Json<EventsResponse>, AgentError> {
    let events = state.events.as_ref().ok_or(AgentError::NotEnabled("events"))?;
    if q.after.is_some() && q.before.is_some() {
        return Err(AgentError::BadRequest("ask for events after an id or before one, not both".into()));
    }
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let ((page, more), latest_id) = events.store
        .call(move |c| Ok((db::page(c, q.after, q.before, limit)?, db::latest_id(c)?))).await?;
    Ok(
        Json(EventsResponse {
            events: page,
            problems: events.open_problems().to_vec(),
            latest_id,
            more,
            silences: events.active_silences(crate::sample::host::unix_now()),
        })
    )
}

/// A crash's last lines, kept with its event. Logs are on the same terms as
/// the live ones: off when `allow_logs` is.
pub async fn logs(State(state): State<AppState>, Path(id): Path<i64>) -> Result<Json<EventLogs>, AgentError> {
    let events = state.events.as_ref().ok_or(AgentError::NotEnabled("events"))?;
    if !state.cfg.docker.allow_logs {
        return Err(AgentError::NotEnabled("container logs"));
    }
    events.store
        .call(move |c| db::logs(c, id)).await?
        .map(Json)
        .ok_or_else(|| AgentError::NotFound(format!("no lines were kept with event {id}")))
}
