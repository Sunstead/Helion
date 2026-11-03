//! Container logs: a REST tail and a WebSocket follow, for one container or
//! for every running container at once.
//!
//! WebSocket rather than SSE for the follow case, for three reasons that have
//! nothing to do with auth headers (`?token=` handles those for both):
//!
//! 1. Docker log frames contain arbitrary bytes including embedded newlines.
//!    SSE would require splitting every newline into its own `data:` line and
//!    reassembling client-side; a WS frame carries the line as-is.
//! 2. `EventSource` reconnects automatically and unconditionally. Against a
//!    crash-looping container that means silently re-opening a follow stream
//!    on the Docker socket forever, with no backoff the client controls.
//! 3. Browsers cap ~6 concurrent HTTP/1.1 connections per origin. Host SSE +
//!    container SSE + three open log panes already reaches it.

use crate::{
    docker::DockerHandle,
    error::{ from_docker, AgentError },
    sample::host::unix_now,
    state::AppState,
};
use axum::{
    extract::{
        ws::{ Message, WebSocket, WebSocketUpgrade },
        Path,
        Query,
        State,
    },
    response::Response,
    Json,
};
use bollard::{ container::LogOutput, Docker };
use helion_common::types::{ LogFrame, LogLine, LogStream, LogsResponse };
use futures_util::{ Stream, StreamExt };
use serde::Deserialize;
use std::pin::Pin;
use tokio_stream::StreamMap;

#[derive(Deserialize)]
pub struct LogQuery {
    #[serde(default = "default_tail")]
    tail: u32,
    /// Unix seconds; 0 means "from the beginning of what Docker retains".
    #[serde(default)]
    since: i64,
}

fn default_tail() -> u32 {
    500
}

/// Hard cap on a single REST tail so a pathological `tail` can't be used to
/// pull a gigabyte through the agent.
const MAX_TAIL: u32 = 10_000;

/// Backlog per container in the all-containers views, so one chatty container
/// can't crowd the others out of the first screen.
const ALL_TAIL: u32 = 200;

/// Docker log reads in flight at once for the all-containers tail.
const ALL_CONCURRENCY: usize = 8;

/// How far back a container that starts mid-stream is read from. The
/// container list updates within an event's latency, so a few seconds covers
/// its first lines without replaying an earlier run's output.
const NEWCOMER_LOOKBACK_SECS: i64 = 3;

/// Why a socket closes when the agent stops.
const RESTARTING: &str = "the agent is restarting";

pub async fn tail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<LogQuery>
) -> Result<Json<LogsResponse>, AgentError> {
    if !state.cfg.docker.allow_logs {
        return Err(AgentError::NotEnabled("container logs"));
    }
    let docker = state.docker.require()?;
    let lines = read_tail(&docker, &id, q.tail.min(MAX_TAIL), q.since).await.map_err(|e|
        from_docker("logs", &id, e)
    )?;
    Ok(Json(LogsResponse { id, lines }))
}

/// A container's last `tail` lines (since `since`, unix seconds; 0 for all
/// Docker kept). Also how the event log keeps a crash's last lines.
pub(crate) async fn read_tail(
    docker: &Docker,
    id: &str,
    tail: u32,
    since: i64
) -> Result<Vec<LogLine>, bollard::errors::Error> {
    let mut stream = docker.logs(id, Some(DockerHandle::logs_options(tail.to_string(), since, false)));
    let mut lines = Vec::with_capacity(tail as usize);
    while let Some(chunk) = stream.next().await {
        lines.extend(parse_output(&chunk?));
    }
    Ok(lines)
}

pub async fn follow(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<LogQuery>,
    ws: WebSocketUpgrade
) -> Result<Response, AgentError> {
    if !state.cfg.docker.allow_logs {
        return Err(AgentError::NotEnabled("container logs"));
    }
    // Fail with a clean 404 before the upgrade, rather than handing the
    // client a websocket that immediately closes.
    state.docker.assert_exists(&id).await?;

    Ok(ws.on_upgrade(move |socket| pump(socket, state, id, q)))
}

async fn pump(mut socket: WebSocket, state: AppState, id: String, q: LogQuery) {
    let Some(docker) = state.docker.client() else {
        let _ = send_frame(
            &mut socket,
            &(LogFrame::Closed { reason: "docker unavailable".into() })
        ).await;
        return;
    };

    let mut logs = docker.logs(
        &id,
        Some(DockerHandle::logs_options(q.tail.min(MAX_TAIL).to_string(), q.since, true))
    );
    let stopping = state.shutdown.clone().wait();
    tokio::pin!(stopping);

    // Each send is awaited, so a client that reads slowly slows the Docker
    // read too; nothing piles up in the agent.
    loop {
        tokio::select! {
            () = &mut stopping => {
                let _ = send_frame(&mut socket, &(LogFrame::Closed { reason: RESTARTING.into() })).await;
                break;
            }

            // Client-initiated close, or a ping we answer implicitly.
            incoming = socket.recv() => {
                match incoming {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => continue,
                }
            }

            chunk = logs.next() => {
                let Some(chunk) = chunk else {
                    let _ = send_frame(
                        &mut socket,
                        &LogFrame::Closed { reason: "stream ended".into() },
                    ).await;
                    break;
                };

                let Ok(output) = chunk else {
                    let _ = send_frame(
                        &mut socket,
                        &LogFrame::Closed { reason: "log stream error".into() },
                    ).await;
                    break;
                };

                for line in parse_output(&output) {
                    if send_frame(&mut socket, &LogFrame::Line(line)).await.is_err() {
                        return;
                    }
                }
            }
        }
    }

    let _ = socket.close().await;
}

// --- every running container ---------------------------------------------

/// The newest lines from every running container, oldest first.
pub async fn tail_all(
    State(state): State<AppState>,
    Query(q): Query<LogQuery>
) -> Result<Json<LogsResponse>, AgentError> {
    if !state.cfg.docker.allow_logs {
        return Err(AgentError::NotEnabled("container logs"));
    }
    let docker = state.docker.require()?;
    let per = q.tail.min(ALL_TAIL).to_string();
    let ids = state.containers_rx.borrow().running.clone();

    let mut lines: Vec<LogLine> = futures_util::stream
        ::iter(ids.iter().cloned())
        .map(|id| {
            let docker = docker.clone();
            let per = per.clone();
            async move {
                let mut out = Vec::new();
                let mut logs = docker.logs(
                    &id,
                    Some(DockerHandle::logs_options(per, q.since, false))
                );
                // A container that stopped in between just contributes nothing.
                while let Some(Ok(output)) = logs.next().await {
                    out.extend(tagged(parse_output(&output), &id));
                }
                out
            }
        })
        .buffer_unordered(ALL_CONCURRENCY)
        .concat().await;

    sort_by_time(&mut lines);
    let excess = lines.len().saturating_sub(MAX_TAIL as usize);
    lines.drain(..excess);

    Ok(Json(LogsResponse { id: "all".into(), lines }))
}

pub async fn follow_all(
    State(state): State<AppState>,
    Query(q): Query<LogQuery>,
    ws: WebSocketUpgrade
) -> Result<Response, AgentError> {
    if !state.cfg.docker.allow_logs {
        return Err(AgentError::NotEnabled("container logs"));
    }
    state.docker.require()?;
    Ok(ws.on_upgrade(move |socket| pump_all(socket, state, q)))
}

type DockerLogs = Pin<Box<dyn Stream<Item = Result<LogOutput, bollard::errors::Error>> + Send>>;

fn open_logs(docker: &Docker, id: &str, tail: String, since: i64) -> DockerLogs {
    Box::pin(docker.logs(id, Some(DockerHandle::logs_options(tail, since, true))))
}

/// One socket for every running container. Each container's Docker stream
/// sits in a `StreamMap`, which polls them fairly and drops one when it ends
/// (the container stopped). Containers that start later are added as the
/// container list changes, so the view never needs reopening.
///
/// The initial backlogs arrive interleaved in no particular order; lines
/// carry Docker's timestamps and the client orders them.
async fn pump_all(mut socket: WebSocket, state: AppState, q: LogQuery) {
    let Some(docker) = state.docker.client() else {
        let _ = send_frame(
            &mut socket,
            &(LogFrame::Closed { reason: "docker unavailable".into() })
        ).await;
        return;
    };

    let per = q.tail.min(ALL_TAIL).to_string();
    let mut containers = state.containers_rx.clone();
    let mut streams: StreamMap<String, DockerLogs> = StreamMap::new();
    for id in containers.borrow_and_update().running.iter() {
        streams.insert(id.clone(), open_logs(&docker, id, per.clone(), q.since));
    }
    let stopping = state.shutdown.clone().wait();
    tokio::pin!(stopping);

    loop {
        tokio::select! {
            () = &mut stopping => {
                let _ = send_frame(&mut socket, &(LogFrame::Closed { reason: RESTARTING.into() })).await;
                break;
            }

            incoming = socket.recv() => {
                match incoming {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => continue,
                }
            }

            changed = containers.changed() => {
                if changed.is_err() {
                    break;
                }
                let running = containers.borrow_and_update().running.clone();
                let since = unix_now() - NEWCOMER_LOOKBACK_SECS;
                for id in running.iter() {
                    if !streams.contains_key(id) {
                        streams.insert(id.clone(), open_logs(&docker, id, "all".into(), since));
                    }
                }
            }

            Some((id, chunk)) = streams.next(), if !streams.is_empty() => {
                let Ok(output) = chunk else {
                    // This container's stream failed; the others carry on.
                    streams.remove(&id);
                    continue;
                };
                for line in tagged(parse_output(&output), &id) {
                    if send_frame(&mut socket, &LogFrame::Line(line)).await.is_err() {
                        return;
                    }
                }
            }
        }
    }

    let _ = socket.close().await;
}

fn tagged(lines: Vec<LogLine>, id: &str) -> impl Iterator<Item = LogLine> + '_ {
    lines.into_iter().map(move |mut l| {
        l.container = Some(id.to_string());
        l
    })
}

/// Docker's timestamps are RFC3339 in UTC with the fraction's trailing zeros
/// trimmed, so they don't sort as strings (`.1Z` after `.12Z`). Seconds
/// compare as text; the fraction is padded to nanoseconds.
fn time_key(ts: Option<&str>) -> (&str, u32) {
    let Some(ts) = ts else {
        return ("", 0);
    };
    let (secs, rest) = ts.split_at(ts.len().min(19));
    let digits: String = rest
        .strip_prefix('.')
        .unwrap_or("")
        .chars()
        .take_while(char::is_ascii_digit)
        .take(9)
        .collect();
    let nanos = format!("{digits:0<9}").parse().unwrap_or(0);
    (secs, nanos)
}

/// Stable, so lines with equal (or no) timestamps keep their arrival order.
fn sort_by_time(lines: &mut [LogLine]) {
    lines.sort_by(|a, b| time_key(a.ts.as_deref()).cmp(&time_key(b.ts.as_deref())));
}

async fn send_frame(socket: &mut WebSocket, frame: &LogFrame) -> Result<(), axum::Error> {
    let text = serde_json::to_string(frame).unwrap_or_else(|_| "{}".into());
    socket.send(Message::Text(text)).await
}

/// Splits a Docker log chunk into lines.
///
/// A single chunk can contain several newline-separated lines, and with
/// `timestamps: true` each is prefixed with an RFC3339 stamp and a space.
fn parse_output(output: &LogOutput) -> Vec<LogLine> {
    let (stream, bytes) = match output {
        LogOutput::StdErr { message } => (LogStream::Stderr, message),
        LogOutput::StdOut { message } | LogOutput::Console { message } =>
            (LogStream::Stdout, message),
        // Stdin echo is not something a log viewer wants.
        LogOutput::StdIn { .. } => {
            return Vec::new();
        }
    };

    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let (ts, text) = split_timestamp(line);
            LogLine { stream, ts, text, container: None }
        })
        .collect()
}

/// Peels off Docker's leading RFC3339 timestamp when present.
///
/// Detected structurally rather than by parsing a date: a leading token that
/// is date-shaped and followed by a space. A log line that merely starts with
/// a number is left alone.
fn split_timestamp(line: &str) -> (Option<String>, String) {
    if let Some((head, rest)) = line.split_once(' ') {
        let b = head.as_bytes();
        let date_shaped =
            b.len() >= 20 &&
            b[4] == b'-' &&
            b[7] == b'-' &&
            b[10] == b'T' &&
            b[..4].iter().all(u8::is_ascii_digit);

        if date_shaped {
            return (Some(head.to_string()), rest.to_string());
        }
    }
    (None, line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    // axum re-exports `bytes::Bytes`, so the test needs no extra dependency.
    use axum::body::Bytes;

    fn stdout(s: &str) -> LogOutput {
        LogOutput::StdOut { message: Bytes::from(s.to_string()) }
    }

    #[test]
    fn splits_a_multi_line_chunk() {
        let lines = parse_output(&stdout("first\nsecond\nthird\n"));
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "first");
        assert_eq!(lines[2].text, "third");
        assert!(lines.iter().all(|l| l.stream == LogStream::Stdout));
    }

    #[test]
    fn extracts_docker_timestamps() {
        let lines = parse_output(&stdout("2024-03-01T12:00:00.123456789Z hello world\n"));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].ts.as_deref(), Some("2024-03-01T12:00:00.123456789Z"));
        assert_eq!(lines[0].text, "hello world");
    }

    #[test]
    fn leaves_lines_without_a_timestamp_intact() {
        let lines = parse_output(&stdout("plain message here\n"));
        assert_eq!(lines[0].ts, None);
        assert_eq!(lines[0].text, "plain message here");
    }

    #[test]
    fn does_not_mistake_a_leading_number_for_a_timestamp() {
        // Regression guard: naive "split on first space" would eat the 200.
        let lines = parse_output(&stdout("200 OK from upstream\n"));
        assert_eq!(lines[0].ts, None);
        assert_eq!(lines[0].text, "200 OK from upstream");
    }

    #[test]
    fn stderr_is_labelled_distinctly() {
        let out = LogOutput::StdErr { message: Bytes::from_static(b"boom\n") };
        assert_eq!(parse_output(&out)[0].stream, LogStream::Stderr);
    }

    #[test]
    fn invalid_utf8_is_replaced_rather_than_dropped() {
        let out = LogOutput::StdOut { message: Bytes::from_static(b"ok \xff\xfe bad\n") };
        let lines = parse_output(&out);
        assert_eq!(lines.len(), 1, "a binary blob must not kill the stream");
        assert!(lines[0].text.contains("ok"));
    }

    fn at(ts: &str) -> LogLine {
        LogLine { stream: LogStream::Stdout, ts: Some(ts.into()), text: ts.into(), container: None }
    }

    #[test]
    fn orders_trimmed_fractions_by_value_not_text() {
        // As text, ".1Z" sorts after ".12Z"; by value it is later still.
        let mut lines = vec![
            at("2024-03-01T12:00:00.1Z"),
            at("2024-03-01T12:00:00.12Z"),
            at("2024-03-01T12:00:00Z"),
            at("2024-03-01T11:59:59.999999999Z"),
        ];
        sort_by_time(&mut lines);
        let order: Vec<_> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(order, [
            "2024-03-01T11:59:59.999999999Z",
            "2024-03-01T12:00:00Z",
            "2024-03-01T12:00:00.1Z",
            "2024-03-01T12:00:00.12Z",
        ]);
    }

    #[test]
    fn lines_without_a_timestamp_sort_first_and_keep_their_order() {
        let mut lines = vec![at("2024-03-01T12:00:00Z"), parse_output(&stdout("a\n")).remove(0), parse_output(&stdout("b\n")).remove(0)];
        sort_by_time(&mut lines);
        assert_eq!(lines[0].text, "a");
        assert_eq!(lines[1].text, "b");
    }

    #[test]
    fn tagging_names_the_container_and_single_streams_do_not() {
        let plain = parse_output(&stdout("x\n"));
        assert_eq!(plain[0].container, None);
        let lines: Vec<_> = tagged(plain, "abc").collect();
        assert_eq!(lines[0].container.as_deref(), Some("abc"));
    }

    #[test]
    fn blank_lines_are_skipped() {
        assert!(parse_output(&stdout("\n\n\n")).is_empty());
    }
}
