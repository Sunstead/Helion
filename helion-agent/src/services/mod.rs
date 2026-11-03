//! Expected services: a service that has been running and stops being there
//! is reported, which nothing watching containers can see (a removed container
//! sends no `die`, and its uptime check pauses with it).
//!
//! One task owns the list, like Wake-on-LAN's and uptime's: it ticks over the
//! container snapshot, runs the pure [`tracker`], stores what changed and
//! reports to the event log. Handlers read its snapshot, or send it a command
//! and wait for it to answer, so a GET right after a write reflects it.

pub mod tracker;

use crate::{
    error::AgentError,
    events::EventsHandle,
    sample::{ docker::ContainerSnapshot, host::unix_now },
    store::Store,
};
use helion_common::types::ExpectedService;
use rusqlite::{ params, Connection };
use std::{ sync::Arc, time::Duration };
use tokio::sync::{ mpsc, oneshot, watch };
use tracker::{ Entry, Tracker };

const TICK: Duration = Duration::from_secs(15);

enum Command {
    Stopped {
        service: String,
        by: String,
    },
    Forget {
        service: String,
        done: oneshot::Sender<bool>,
    },
}

#[derive(Clone)]
pub struct ServicesHandle {
    tx: mpsc::Sender<Command>,
    pub list: watch::Receiver<Arc<[ExpectedService]>>,
}

impl ServicesHandle {
    /// Something stopped or removed one of the service's containers through
    /// Helion. Best effort: the next tick sees the stop either way, and a
    /// marker lost here only means the absence is reported.
    pub fn stopped(&self, service: &str, by: &str) {
        let _ = self.tx.try_send(Command::Stopped { service: service.into(), by: by.into() });
    }

    /// Stops expecting the service. `NotFound` if it wasn't expected.
    pub async fn forget(&self, service: &str) -> Result<(), AgentError> {
        let (done, wait) = oneshot::channel();
        self.tx
            .send(Command::Forget { service: service.into(), done }).await
            .map_err(|_| AgentError::Unavailable("expected services are unavailable".into()))?;
        match wait.await {
            Ok(true) => Ok(()),
            Ok(false) => Err(AgentError::NotFound(format!("{service} is not an expected service"))),
            Err(_) => Err(AgentError::Unavailable("expected services are unavailable".into())),
        }
    }
}

pub async fn spawn(
    store: Store,
    events: Option<EventsHandle>,
    mut containers: watch::Receiver<Arc<ContainerSnapshot>>
) -> Result<ServicesHandle, AgentError> {
    let entries = store.call(load).await?;
    let mut tracker = Tracker::new(entries, unix_now());
    let (list_tx, list) = watch::channel::<Arc<[ExpectedService]>>(tracker.list().into());
    let (tx, mut rx) = mpsc::channel(16);

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    let snap = containers.borrow_and_update().clone();
                    // Nothing sampled yet. While Docker is down the snapshot
                    // is the last list known, so an outage changes nothing.
                    if snap.sampled_at == 0 {
                        continue;
                    }
                    let open = events.as_ref().map(EventsHandle::open_problems).unwrap_or_default();
                    let out = tracker.observe(&snap.containers, &open, unix_now());
                    if !out.save.is_empty() {
                        if let Err(e) = store.call(move |c| save(c, &out.save)).await {
                            tracing::warn!(error = %e, "cannot store expected services");
                        }
                    }
                    if let Some(events) = &events {
                        events.report_all(out.reports);
                    }
                }
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { return; };
                    match cmd {
                        Command::Stopped { service, by } => {
                            if let Some(e) = tracker.stopped(&service, &by, unix_now()) {
                                if let Err(e) = store.call(move |c| save(c, &[e])).await {
                                    tracing::warn!(error = %e, "cannot store expected services");
                                }
                            }
                        }
                        Command::Forget { service, done } => {
                            let found = tracker.forget(&service);
                            if found {
                                if let Err(e) = store.call(move |c| delete(c, &service)).await {
                                    tracing::warn!(error = %e, "cannot forget expected service");
                                }
                            }
                            let _ = done.send(found);
                        }
                    }
                }
            }
            let _ = list_tx.send(tracker.list().into());
        }
    });

    Ok(ServicesHandle { tx, list })
}

fn load(conn: &Connection) -> Result<Vec<Entry>, AgentError> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT service, first_seen, last_running, stopped_by, stopped_at
             FROM expected_services ORDER BY service"
        )
        .map_err(AgentError::internal)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Entry {
                service: r.get(0)?,
                first_seen: r.get(1)?,
                last_running: r.get(2)?,
                stopped_by: r.get(3)?,
                stopped_at: r.get(4)?,
            })
        })
        .map_err(AgentError::internal)?;
    rows.collect::<rusqlite::Result<_>>().map_err(AgentError::internal)
}

fn save(conn: &Connection, entries: &[Entry]) -> Result<(), AgentError> {
    let tx = conn.unchecked_transaction().map_err(AgentError::internal)?;
    {
        let mut stmt = tx
            .prepare_cached(
                "INSERT INTO expected_services (service, first_seen, last_running, stopped_by, stopped_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(service) DO UPDATE SET last_running = excluded.last_running,
                     stopped_by = excluded.stopped_by, stopped_at = excluded.stopped_at"
            )
            .map_err(AgentError::internal)?;
        for e in entries {
            stmt.execute(params![e.service, e.first_seen, e.last_running, e.stopped_by, e.stopped_at]).map_err(
                AgentError::internal
            )?;
        }
    }
    tx.commit().map_err(AgentError::internal)
}

fn delete(conn: &Connection, service: &str) -> Result<(), AgentError> {
    conn.execute("DELETE FROM expected_services WHERE service = ?1", [service]).map_err(AgentError::internal)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_round_trip_and_forget_deletes() {
        let store = Store::in_memory();
        let e = Entry {
            service: "web".into(),
            first_seen: 1,
            last_running: 2,
            stopped_by: Some("Pat".into()),
            stopped_at: Some(3),
        };
        store.with(|c| save(c, std::slice::from_ref(&e))).unwrap();
        let moved = Entry { last_running: 9, stopped_by: None, stopped_at: None, ..e.clone() };
        store.with(|c| save(c, std::slice::from_ref(&moved))).unwrap();
        assert_eq!(store.with(load).unwrap(), [moved]);
        store.with(|c| delete(c, "web")).unwrap();
        assert!(store.with(load).unwrap().is_empty());
    }
}
