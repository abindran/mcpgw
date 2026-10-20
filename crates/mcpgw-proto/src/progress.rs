//! Long calls (M4-09): progress, cancellation and tasks across the gateway.
//!
//! - **Progress.** A front call that carries a `progressToken` gets a
//!   [`ProgressSink`]; it is in scope ([`with_progress`]) while the call
//!   runs, so the backend connection that ends up serving the call picks it
//!   up ([`current`]) without every layer in between passing it along. The
//!   backend's `notifications/progress` go into the sink and from there to
//!   the client, under the client's own token. Progress also keeps a call
//!   alive: the call timeout counts from the last sign of life.
//! - **Cancellation** is dropping the call's future. The backend request is
//!   then cancelled with `notifications/cancelled` (or `tasks/cancel` for a
//!   backend task).
//! - **Tasks** (SEP-2663) are in [`crate::tasks`].

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use rmcp::model::ProgressToken;
use tokio::sync::mpsc;

/// One `notifications/progress`, without the token.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// Progress so far; increases with every notification.
    pub progress: f64,
    /// The total, when known.
    pub total: Option<f64>,
    /// What is happening.
    pub message: Option<String>,
}

/// Where a call's progress goes. Inactive by default; cheap to clone.
#[derive(Clone, Default)]
pub struct ProgressSink(Option<mpsc::UnboundedSender<Progress>>);

impl ProgressSink {
    /// A sink and the receiving end.
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<Progress>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self(Some(tx)), rx)
    }

    /// Report progress (dropped when nobody listens).
    pub fn send(&self, p: Progress) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(p);
        }
    }

    /// Whether anyone listens.
    pub fn is_active(&self) -> bool {
        self.0.as_ref().is_some_and(|tx| !tx.is_closed())
    }
}

impl std::fmt::Debug for ProgressSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = if self.is_active() {
            "active"
        } else {
            "inactive"
        };
        write!(f, "ProgressSink({state})")
    }
}

tokio::task_local! {
    static PROGRESS: ProgressSink;
}

/// Run `fut` with `sink` as the current call's progress sink.
pub async fn with_progress<F: std::future::Future>(sink: ProgressSink, fut: F) -> F::Output {
    PROGRESS.scope(sink, fut).await
}

/// The current call's progress sink (inactive outside [`with_progress`]).
/// Read it before handing the call to another task.
pub fn current() -> ProgressSink {
    PROGRESS.try_with(Clone::clone).unwrap_or_default()
}

/// How many early notifications are kept for tokens not yet registered.
const EARLY_MAX: usize = 64;

/// Routes one backend connection's progress notifications to the calls
/// waiting for them, by the progress token each request was sent with.
#[derive(Default)]
pub(crate) struct Routes {
    inner: Mutex<RoutesInner>,
}

#[derive(Default)]
struct RoutesInner {
    sinks: HashMap<ProgressToken, ProgressSink>,
    // a notification can beat the registration (rmcp picks the token while
    // sending); the latest one per token waits here
    early: HashMap<ProgressToken, Progress>,
}

impl Routes {
    /// Send progress for `token` to `sink` until the guard drops.
    pub(crate) fn register(self: &Arc<Self>, token: ProgressToken, sink: ProgressSink) -> Route {
        let mut inner = self.inner.lock().expect("progress routes");
        if let Some(p) = inner.early.remove(&token) {
            sink.send(p);
        }
        inner.sinks.insert(token.clone(), sink);
        Route {
            routes: self.clone(),
            token,
        }
    }

    pub(crate) fn deliver(&self, token: ProgressToken, p: Progress) {
        let mut inner = self.inner.lock().expect("progress routes");
        match inner.sinks.get(&token) {
            Some(sink) => sink.send(p),
            None => {
                if inner.early.len() >= EARLY_MAX {
                    inner.early.clear();
                }
                inner.early.insert(token, p);
            }
        }
    }
}

/// Removes a registration when dropped.
pub(crate) struct Route {
    routes: Arc<Routes>,
    token: ProgressToken,
}

impl Drop for Route {
    fn drop(&mut self) {
        let mut inner = self.routes.inner.lock().expect("progress routes");
        inner.sinks.remove(&self.token);
        inner.early.remove(&self.token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(n: i64) -> ProgressToken {
        ProgressToken(rmcp::model::NumberOrString::Number(n))
    }

    fn p(n: f64) -> Progress {
        Progress {
            progress: n,
            total: None,
            message: None,
        }
    }

    #[tokio::test]
    async fn routes_by_token_and_keeps_early_ones() {
        let routes = Arc::new(Routes::default());
        let (sink, mut rx) = ProgressSink::channel();
        routes.deliver(tok(1), p(1.0)); // before the registration
        let route = routes.register(tok(1), sink);
        routes.deliver(tok(1), p(2.0));
        routes.deliver(tok(2), p(9.0)); // someone else's
        assert_eq!(rx.recv().await, Some(p(1.0)));
        assert_eq!(rx.recv().await, Some(p(2.0)));
        drop(route);
        routes.deliver(tok(1), p(3.0));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn the_current_sink_is_scoped() {
        assert!(!current().is_active());
        let (sink, mut rx) = ProgressSink::channel();
        with_progress(sink, async {
            current().send(p(5.0));
        })
        .await;
        assert_eq!(rx.recv().await, Some(p(5.0)));
        assert!(!current().is_active());
    }
}
