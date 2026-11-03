use crate::{
    error::AgentError,
    history::containers::{ Select, MAX_CONTAINERS },
    sample::host::unix_now,
    state::AppState,
};
use axum::{ extract::{ Query, State }, Json };
use helion_common::types::{ ContainerRank, ContainerSeries, MetricSeries, MetricStep };
use serde::Deserialize;

#[derive(Deserialize)]
pub struct RangeQuery {
    /// Unix seconds. Defaults to one hour ago.
    from: Option<i64>,
    /// Unix seconds. Defaults to now.
    to: Option<i64>,
    #[serde(default = "default_step")]
    step: MetricStep,
    #[serde(default = "default_max_points")]
    max_points: usize,
}

fn default_step() -> MetricStep {
    MetricStep::Auto
}
fn default_max_points() -> usize {
    1000
}

pub async fn range(
    State(state): State<AppState>,
    Query(q): Query<RangeQuery>
) -> Result<Json<MetricSeries>, AgentError> {
    let history = state.history.clone().ok_or(AgentError::NotEnabled("metrics history"))?;

    let to = q.to.unwrap_or_else(unix_now);
    let from = q.from.unwrap_or(to - 3_600);

    // rusqlite is blocking and `Connection` is !Sync, so the query runs off
    // the async runtime rather than holding a worker for the duration.
    tokio::task
        ::spawn_blocking(move || history.query(from, to, q.step, q.max_points)).await
        .map_err(AgentError::internal)?
        .map(Json)
}

#[derive(Deserialize)]
pub struct ContainersQuery {
    from: Option<i64>,
    to: Option<i64>,
    #[serde(default = "default_step")]
    step: MetricStep,
    #[serde(default = "default_max_points")]
    max_points: usize,
    /// Comma-separated container names. Without it, the top `limit` by `top`.
    names: Option<String>,
    #[serde(default = "default_rank")]
    top: ContainerRank,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_rank() -> ContainerRank {
    ContainerRank::Cpu
}
fn default_limit() -> usize {
    8
}

pub async fn containers(
    State(state): State<AppState>,
    Query(q): Query<ContainersQuery>
) -> Result<Json<ContainerSeries>, AgentError> {
    let history = state.history.clone().ok_or(AgentError::NotEnabled("metrics history"))?;

    let to = q.to.unwrap_or_else(unix_now);
    let from = q.from.unwrap_or(to - 3_600);
    let select = match q.names.as_deref().filter(|n| !n.trim().is_empty()) {
        Some(names) => {
            let names: Vec<String> = names
                .split(',')
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(String::from)
                .collect();
            if names.len() > MAX_CONTAINERS {
                return Err(
                    AgentError::BadRequest(format!("at most {MAX_CONTAINERS} names at once"))
                );
            }
            Select::Names(names)
        }
        None => Select::Top(q.top, q.limit),
    };

    tokio::task
        ::spawn_blocking(move || {
            history.query_containers(from, to, q.step, q.max_points, &select)
        }).await
        .map_err(AgentError::internal)?
        .map(Json)
}
