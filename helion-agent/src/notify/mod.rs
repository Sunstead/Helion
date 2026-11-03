//! Notifications: every event the log stores is offered to each channel
//! (ntfy or a webhook), which takes it if its rules want it and the flood
//! guard allows.
//!
//! Channels are edited in the app and kept in `state.db`. A channel's secret
//! (an ntfy access token, or a webhook's bearer token) is the one credential
//! the agent stores: it's never sent back out, and `Debug` hides it. It only
//! lets the agent publish to your own topics.
//!
//! Delivery happens off the loop, a task per message, so a slow or dead
//! server never holds up the next event. There is no queue that outlives the
//! retries: the event log still has everything.

pub mod db;
pub mod rules;
pub mod send;
pub mod silence;

use crate::{ error::AgentError, events::EventsHandle, peers::PeersHandle, sample::host::unix_now, store::Store };
use helion_common::types::{
    ChannelKind,
    ChannelStatus,
    Event,
    EventCategory,
    NotifyChannel,
    NotifyChannelInput,
    NotifyResponse,
    NotifySettings,
    Severity,
};
use rules::{ FloodGuard, PeerHolds };
use std::{ collections::HashMap, fmt, sync::Arc, time::Duration };
use tokio::sync::{ broadcast, mpsc, oneshot, watch };

const MAX_NAME: usize = 64;
const MAX_TOPIC: usize = 64;

/// A channel as stored, secret included.
#[derive(Clone, PartialEq)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    pub kind: ChannelKind,
    pub url: String,
    pub topic: Option<String>,
    pub secret: Option<String>,
    pub enabled: bool,
    pub min_severity: Severity,
    pub recoveries: bool,
    pub categories: Vec<EventCategory>,
}

impl fmt::Debug for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("url", &self.url)
            .field("topic", &self.topic)
            .field("secret", &self.secret.as_ref().map(|_| "<hidden>"))
            .field("enabled", &self.enabled)
            .field("min_severity", &self.min_severity)
            .field("recoveries", &self.recoveries)
            .field("categories", &self.categories)
            .finish()
    }
}

impl Channel {
    /// What the API shows: everything but the secret.
    pub fn public(&self, status: ChannelStatus) -> NotifyChannel {
        NotifyChannel {
            id: self.id.to_string(),
            name: self.name.clone(),
            kind: self.kind,
            url: self.url.clone(),
            topic: self.topic.clone(),
            has_secret: self.secret.is_some(),
            enabled: self.enabled,
            min_severity: self.min_severity,
            recoveries: self.recoveries,
            categories: self.categories.clone(),
            status,
        }
    }
}

fn http_url(url: &str, what: &str) -> Result<String, AgentError> {
    let url = url.trim();
    let parsed = reqwest::Url
        ::parse(url)
        .map_err(|_| AgentError::BadRequest(format!("{what} {url:?} is not a URL")))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(AgentError::BadRequest(format!("{what} must be an http or https URL")));
    }
    Ok(url.to_string())
}

/// Checks an edit and builds the channel to store. `saved_secret` is the
/// secret already stored, kept when the input leaves it out.
pub fn validate(input: NotifyChannelInput, id: i64, saved_secret: Option<String>) -> Result<Channel, AgentError> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(AgentError::BadRequest("a channel needs a name".into()));
    }
    if name.chars().count() > MAX_NAME {
        return Err(AgentError::BadRequest(format!("names are at most {MAX_NAME} characters")));
    }
    let url = http_url(&input.url, "the address")?;
    let topic = match input.kind {
        ChannelKind::Ntfy => {
            let topic = input.topic.as_deref().map(str::trim).unwrap_or_default();
            let valid =
                !topic.is_empty() &&
                topic.len() <= MAX_TOPIC &&
                topic.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !valid {
                return Err(
                    AgentError::BadRequest(format!("an ntfy topic is 1 to {MAX_TOPIC} letters, digits, - or _"))
                );
            }
            Some(topic.to_string())
        }
        ChannelKind::Webhook => None,
    };
    let secret = match input.secret {
        None => saved_secret,
        Some(s) if s.trim().is_empty() => None,
        Some(s) => Some(s.trim().to_string()),
    };
    let mut categories = input.categories;
    let mut seen = Vec::new();
    categories.retain(|c| {
        let fresh = !seen.contains(c);
        seen.push(*c);
        fresh
    });
    Ok(Channel {
        id,
        name,
        kind: input.kind,
        url,
        topic,
        secret,
        enabled: input.enabled,
        min_severity: input.min_severity,
        recoveries: input.recoveries,
        categories,
    })
}

pub fn validate_settings(s: NotifySettings) -> Result<NotifySettings, AgentError> {
    let link_url = match s.link_url.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(u) => Some(http_url(u, "the link")?),
    };
    Ok(NotifySettings { link_url })
}

enum Command {
    Reload(oneshot::Sender<()>),
    Delivered {
        id: i64,
        at: i64,
        result: Result<(), String>,
    },
}

#[derive(Clone)]
pub struct NotifyHandle {
    pub store: Store,
    tx: mpsc::Sender<Command>,
    status: watch::Receiver<Arc<HashMap<i64, ChannelStatus>>>,
    client: reqwest::Client,
    node: Arc<str>,
}

impl NotifyHandle {
    /// After a channel or the settings change. Returns once the notifier
    /// uses the new ones.
    pub async fn reload(&self) -> Result<(), AgentError> {
        let (done, wait) = oneshot::channel();
        self.tx.send(Command::Reload(done)).await.map_err(|_| AgentError::Unavailable("the notifier stopped".into()))?;
        wait.await.map_err(|_| AgentError::Unavailable("the notifier stopped".into()))
    }

    /// Sends a test message now, whatever the channel's rules, and records
    /// how it went. One try, so a wrong address or token shows at once.
    pub async fn test(&self, ch: &Channel) -> Result<(), String> {
        let link = self.store.call(db::settings).await.ok().and_then(|s| s.link_url);
        let event = Event {
            id: 0,
            at: unix_now(),
            category: EventCategory::Agent,
            kind: "test".into(),
            severity: Severity::Info,
            subject: self.node.to_string(),
            title: "Test notification".into(),
            detail: Some(format!("If you can read this, the {} channel works.", ch.name)),
            actor: None,
            service: None,
            problem: None,
        };
        let result = send::deliver_once(&self.client, ch, &event, &self.node, link.as_deref()).await;
        let _ = self.tx.send(Command::Delivered { id: ch.id, at: unix_now(), result: result.clone() }).await;
        result
    }

    pub async fn response(&self) -> Result<NotifyResponse, AgentError> {
        let (channels, settings) = self.store.call(|c| Ok((db::list(c)?, db::settings(c)?))).await?;
        let status = self.status.borrow().clone();
        Ok(NotifyResponse {
            channels: channels
                .iter()
                .map(|c| c.public(status.get(&c.id).cloned().unwrap_or_default()))
                .collect(),
            settings,
        })
    }
}

async fn load(store: &Store) -> (Vec<Channel>, Option<String>) {
    match store.call(|c| Ok((db::list(c)?, db::settings(c)?.link_url))).await {
        Ok(loaded) => loaded,
        Err(e) => {
            tracing::error!(error = %e, "cannot read notification channels");
            (Vec::new(), None)
        }
    }
}

pub fn spawn(store: Store, events: &EventsHandle, node: String, peers: Option<PeersHandle>) -> NotifyHandle {
    let (tx, mut rx) = mpsc::channel::<Command>(64);
    let (status_tx, status) = watch::channel(Arc::new(HashMap::new()));
    let handle = NotifyHandle { store: store.clone(), tx: tx.clone(), status, client: send::client(), node: node.into() };

    let mut stored = events.subscribe();
    let events = events.clone();
    let out = handle.clone();
    tokio::spawn(async move {
        let (mut channels, mut link) = load(&store).await;
        let mut guard = FloodGuard::default();
        let mut holds = PeerHolds::default();
        let mut mutes = silence::Mutes::default();
        let explains = |peer: &str| peers.as_ref().is_some_and(|p| p.explains(peer, unix_now()));
        let mut statuses: HashMap<i64, ChannelStatus> = HashMap::new();
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let deliver = |ch: &Channel, event: Event, link: Option<String>| {
            let (ch, out, tx) = (ch.clone(), out.clone(), tx.clone());
            tokio::spawn(async move {
                let result = send::deliver(&out.client, &ch, &event, &out.node, link.as_deref()).await;
                if let Err(e) = &result {
                    tracing::warn!(channel = %ch.name, error = %e, "notification not delivered");
                }
                let _ = tx.send(Command::Delivered { id: ch.id, at: unix_now(), result }).await;
            });
        };

        loop {
            tokio::select! {
                event = stored.recv() => {
                    let event = match event {
                        Ok(e) => e,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(missed = n, "the notifier fell behind; some events weren't offered");
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                    };
                    // Acknowledged and silenced first: someone asked not to
                    // hear about it.
                    let now = unix_now();
                    if !mutes.check(&event, &events.open_problems(), &events.active_silences(now), now) {
                        continue;
                    }
                    // A problem that goes through an unreachable peer waits:
                    // the peer's own problem says it.
                    let depends_on = event.problem.as_ref().and_then(|p| {
                        events.open_problems().iter().find(|o| o.key == p.key).and_then(|o| o.depends_on.clone())
                    });
                    if holds.hold(&event, depends_on.as_deref(), explains) || holds.cleared(&event) {
                        continue;
                    }
                    for ch in &channels {
                        if rules::wants(ch, &event) && guard.allow(ch.id, &event, now) {
                            deliver(ch, (*event).clone(), link.clone());
                        }
                    }
                }
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { return; };
                    match cmd {
                        Command::Reload(done) => {
                            (channels, link) = load(&store).await;
                            statuses.retain(|id, _| channels.iter().any(|c| c.id == *id));
                            let _ = status_tx.send(Arc::new(statuses.clone()));
                            let _ = done.send(());
                        }
                        Command::Delivered { id, at, result } => {
                            let s = statuses.entry(id).or_default();
                            match result {
                                Ok(()) => s.last_ok_at = Some(at),
                                Err(e) => {
                                    s.last_error_at = Some(at);
                                    s.last_error = Some(e);
                                }
                            }
                            let _ = status_tx.send(Arc::new(statuses.clone()));
                        }
                    }
                }
                _ = tick.tick() => {
                    let open = events.open_problems();
                    let now = unix_now();
                    let silences = events.active_silences(now);
                    let released = holds.release(|key| open.iter().any(|p| p.key == key), explains);
                    let late: Vec<Event> = mutes
                        .release(&open, &silences, now)
                        .into_iter()
                        .chain(released.into_iter().filter(|e| mutes.check(e, &open, &silences, now)))
                        .collect();
                    for event in late {
                        for ch in &channels {
                            if rules::wants(ch, &event) && guard.allow(ch.id, &event, now) {
                                deliver(ch, event.clone(), link.clone());
                            }
                        }
                    }
                    let due = guard.due(now, |key| open.iter().any(|p| p.key == key));
                    for (id, event) in due.into_iter().filter(|(_, e)| mutes.check(e, &open, &silences, now)) {
                        if let Some(ch) = channels.iter().find(|c| c.id == id && rules::wants(c, &event)) {
                            deliver(ch, event, link.clone());
                        }
                    }
                }
            }
        }
    });

    handle
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> NotifyChannelInput {
        NotifyChannelInput {
            name: " phone ".into(),
            kind: ChannelKind::Ntfy,
            url: "https://ntfy.example.com".into(),
            topic: Some("cosmos-jupiter".into()),
            secret: Some("tk_abc".into()),
            enabled: true,
            min_severity: Severity::Warning,
            recoveries: true,
            categories: vec![EventCategory::Backup, EventCategory::Backup, EventCategory::Disk],
        }
    }

    #[test]
    fn a_valid_channel_is_tidied() {
        let ch = validate(input(), 0, None).unwrap();
        assert_eq!(ch.name, "phone");
        assert_eq!(ch.secret.as_deref(), Some("tk_abc"));
        assert_eq!(ch.categories, [EventCategory::Backup, EventCategory::Disk]);
    }

    #[test]
    fn the_secret_is_kept_unless_replaced_or_cleared() {
        let saved = Some("tk_saved".to_string());
        let kept = validate(NotifyChannelInput { secret: None, ..input() }, 1, saved.clone()).unwrap();
        assert_eq!(kept.secret, saved);
        let cleared = validate(NotifyChannelInput { secret: Some(" ".into()), ..input() }, 1, saved.clone()).unwrap();
        assert_eq!(cleared.secret, None);
        let replaced = validate(input(), 1, saved).unwrap();
        assert_eq!(replaced.secret.as_deref(), Some("tk_abc"));
    }

    #[test]
    fn bad_input_is_refused() {
        let bad = [
            NotifyChannelInput { name: " ".into(), ..input() },
            NotifyChannelInput { url: "ntfy.example.com".into(), ..input() },
            NotifyChannelInput { url: "ftp://ntfy.example.com".into(), ..input() },
            NotifyChannelInput { topic: None, ..input() },
            NotifyChannelInput { topic: Some("has space".into()), ..input() },
        ];
        for b in bad {
            assert!(matches!(validate(b, 0, None), Err(AgentError::BadRequest(_))));
        }
        let hook = validate(NotifyChannelInput { kind: ChannelKind::Webhook, topic: None, ..input() }, 0, None).unwrap();
        assert_eq!(hook.topic, None);
    }

    #[test]
    fn the_secret_never_shows() {
        let ch = validate(input(), 3, None).unwrap();
        assert!(!format!("{ch:?}").contains("tk_abc"));
        let public = serde_json::to_string(&ch.public(ChannelStatus::default())).unwrap();
        assert!(!public.contains("tk_abc"));
        assert!(public.contains(r#""has_secret":true"#));
    }

    #[test]
    fn the_link_must_be_a_web_address() {
        assert_eq!(validate_settings(NotifySettings { link_url: Some(" ".into()) }).unwrap().link_url, None);
        assert!(validate_settings(NotifySettings { link_url: Some("javascript:alert(1)".into()) }).is_err());
        let ok = validate_settings(NotifySettings { link_url: Some("https://helion.example.com/events".into()) });
        assert_eq!(ok.unwrap().link_url.as_deref(), Some("https://helion.example.com/events"));
    }
}
