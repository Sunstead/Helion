use crate::{ error::AgentError, icons::ink::Background, state::AppState };
use axum::{
    extract::{ Path, Query, State },
    http::{ header, HeaderValue },
    response::{ IntoResponse, Response },
};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct IconQuery {
    /// The theme the icon is shown on, so a white-ink icon isn't sent to a
    /// light page. Anything but `light` is dark.
    #[serde(default)]
    theme: String,
}

/// One icon's SVG, as `ContainerInfo.icon` names it. Only icons the agent
/// already holds, so a name that was never offered is a 404.
pub async fn get(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<IconQuery>
) -> Result<Response, AgentError> {
    let icons = state.icons.as_ref().ok_or(AgentError::NotEnabled("icons"))?;
    let background = if q.theme == "light" { Background::Light } else { Background::Dark };
    let svg = icons
        .svg(&name, background).await
        .ok_or_else(|| AgentError::NotFound(format!("no icon {name}")))?;
    Ok(
        (
            [
                (header::CONTENT_TYPE, HeaderValue::from_static("image/svg+xml")),
                // Shown through an <img>, which runs nothing; this is for the
                // case of someone opening the URL itself.
                (
                    header::CONTENT_SECURITY_POLICY,
                    HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
                ),
                // The file for a name changes only when the source does.
                (header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400")),
            ],
            svg,
        ).into_response()
    )
}
