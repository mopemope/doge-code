//! Event subscriptions of one connection (`subscribe` / `unsubscribe`).
//!
//! A subscription is a task that forwards agent events to its connection as
//! `event` notifications. It is keyed by its agent filter (`None` = all
//! agents); subscribing again with the same key replaces the old task.
//!
//! - The task waits until the response to its `subscribe` request has been
//!   queued ([`Subscriptions::release`]), so no notification precedes that
//!   response (clients buffer only a bounded number of notifications while
//!   waiting for a response).
//! - The task takes its broadcast receiver from
//!   [`AgentManager::subscribe`] before backfilling, so nothing emitted during
//!   the backfill is missed. With `since_seq` (one-agent subscriptions only)
//!   it first sends the stored events after `since_seq`, page by page, waiting
//!   for room in the outbound queue. Live events with a `seq` at or below the
//!   last one sent for their agent are skipped, which removes the overlap at
//!   the backfill/live seam.
//! - Live events are queued without waiting. When the outbound queue is full
//!   (the client is not reading) or the broadcast reports `Lagged`, the
//!   subscription ends after one `lagged` notification (queued with waiting):
//!   `{agent_id, next_seq}` with the first seq not sent for a one-agent
//!   subscription, `{agent_id: null, next_seq: 0}` for an all-agents one.
//!   The client recovers with `agent.events` and subscribes again.
//! - A task ends when its connection closes, on `unsubscribe`, or when
//!   [`Subscriptions::clear`] runs at connection end.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dgc_protocol::event::AgentEvent;
use dgc_protocol::ids::AgentId;
use dgc_protocol::methods::{
    AgentEventsParams, LaggedParams, MAX_EVENTS_LIMIT, NOTIFY_EVENT, NOTIFY_LAGGED,
};
use dgc_protocol::rpc::Notification;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use tracing::{debug, warn};

use crate::agent::AgentManager;
use crate::server::ConnCtx;

/// The subscription tasks of one connection.
#[derive(Debug, Default)]
pub struct Subscriptions {
    tasks: Mutex<HashMap<Option<AgentId>, AbortHandle>>,
    /// Tasks started by the request being answered; opened by `release`.
    gates: Mutex<Vec<oneshot::Sender<()>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Subscriptions {
    fn tasks(&self) -> MutexGuard<'_, HashMap<Option<AgentId>, AbortHandle>> {
        lock(&self.tasks)
    }

    /// Start a subscription to `agent` (all agents when `None`), replacing
    /// any existing one with the same filter.
    pub fn start(
        &self,
        agents: Arc<AgentManager>,
        ctx: ConnCtx,
        agent: Option<AgentId>,
        since_seq: Option<u64>,
    ) {
        let receiver = agents.subscribe(agent.clone());
        let (open, gate) = oneshot::channel();
        lock(&self.gates).push(open);
        let task = tokio::spawn(run(agents, ctx, agent.clone(), since_seq, receiver, gate));
        if let Some(old) = self.tasks().insert(agent, task.abort_handle()) {
            old.abort();
        }
    }

    /// Stop the subscription to `agent`, or every subscription when `None`.
    pub fn stop(&self, agent: Option<&AgentId>) {
        let mut tasks = self.tasks();
        match agent {
            Some(id) => {
                if let Some(task) = tasks.remove(&Some(id.clone())) {
                    task.abort();
                }
            }
            None => {
                for (_, task) in tasks.drain() {
                    task.abort();
                }
            }
        }
    }

    /// Let subscriptions started by the request just answered begin
    /// sending; call after its response has been queued.
    pub fn release(&self) {
        for open in lock(&self.gates).drain(..) {
            let _ = open.send(());
        }
    }

    /// Stop everything (the connection is closing).
    pub fn clear(&self) {
        lock(&self.gates).clear();
        self.stop(None);
    }
}

fn event_notification(event: &AgentEvent) -> Option<Notification> {
    match serde_json::to_value(event) {
        Ok(params) => Some(Notification::new(NOTIFY_EVENT, params)),
        Err(err) => {
            warn!(agent_id = %event.agent_id, %err, "cannot encode an event");
            None
        }
    }
}

struct Forwarder {
    ctx: ConnCtx,
    filter: Option<AgentId>,
    /// Highest seq sent per agent.
    sent: HashMap<AgentId, u64>,
}

impl Forwarder {
    fn already_sent(&self, event: &AgentEvent) -> bool {
        self.sent
            .get(&event.agent_id)
            .is_some_and(|seq| event.seq <= *seq)
    }

    /// Send the `lagged` notice for this subscription.
    async fn lagged(&self) {
        let params = match &self.filter {
            Some(id) => LaggedParams {
                agent_id: Some(id.clone()),
                next_seq: self.sent.get(id).copied().unwrap_or(0) + 1,
            },
            None => LaggedParams {
                agent_id: None,
                next_seq: 0,
            },
        };
        debug!(conn = self.ctx.id(), agent_id = ?params.agent_id, next_seq = params.next_seq, "subscriber lagged");
        match serde_json::to_value(&params) {
            Ok(value) => {
                self.ctx
                    .notify_wait(&Notification::new(NOTIFY_LAGGED, value))
                    .await;
            }
            Err(err) => warn!(%err, "cannot encode a lagged notice"),
        }
    }

    /// Stored events after `since`, waiting for queue room. False when the
    /// connection is gone or the log cannot be read.
    async fn backfill(&mut self, agents: &AgentManager, agent: &AgentId, since: u64) -> bool {
        self.sent.insert(agent.clone(), since);
        let mut cursor = since;
        loop {
            let params = AgentEventsParams {
                agent_id: agent.clone(),
                since_seq: cursor,
                limit: MAX_EVENTS_LIMIT,
                tail: None,
            };
            let page = match agents.events_page(&params) {
                Ok(page) => page,
                Err(err) => {
                    warn!(conn = self.ctx.id(), agent_id = %agent, error = %err, "backfill failed");
                    return false;
                }
            };
            // A page is also cut by its byte size, so only an empty page
            // means the backlog is done.
            if page.is_empty() {
                return true;
            }
            for event in &page {
                // Advance even past an event that cannot be encoded, so the
                // loop always makes progress.
                cursor = event.seq;
                let Some(note) = event_notification(event) else {
                    continue;
                };
                if !self.ctx.notify_wait(&note).await {
                    return false;
                }
                self.sent.insert(agent.clone(), event.seq);
            }
        }
    }
}

async fn run(
    agents: Arc<AgentManager>,
    ctx: ConnCtx,
    filter: Option<AgentId>,
    since_seq: Option<u64>,
    mut receiver: broadcast::Receiver<AgentEvent>,
    gate: oneshot::Receiver<()>,
) {
    tokio::select! {
        () = ctx.closed() => return,
        opened = gate => if opened.is_err() {
            return;
        },
    }
    let mut forwarder = Forwarder {
        ctx: ctx.clone(),
        filter: filter.clone(),
        sent: HashMap::new(),
    };
    if let (Some(agent), Some(since)) = (&filter, since_seq)
        && !forwarder.backfill(&agents, agent, since).await
    {
        return;
    }
    drop(agents);
    loop {
        let received = tokio::select! {
            () = ctx.closed() => return,
            received = receiver.recv() => received,
        };
        let event = match received {
            Ok(event) => event,
            Err(RecvError::Lagged(missed)) => {
                debug!(
                    conn = ctx.id(),
                    missed, "subscription fell behind the event stream"
                );
                forwarder.lagged().await;
                return;
            }
            Err(RecvError::Closed) => return,
        };
        if filter.as_ref().is_some_and(|id| *id != event.agent_id) || forwarder.already_sent(&event)
        {
            continue;
        }
        let Some(note) = event_notification(&event) else {
            continue;
        };
        if !ctx.notify(&note) {
            forwarder.lagged().await;
            return;
        }
        forwarder.sent.insert(event.agent_id.clone(), event.seq);
    }
}
