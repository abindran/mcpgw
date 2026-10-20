//! Gateway tasks (SEP-2663, M4-09): a `tools/call` from a client that
//! declared the `io.modelcontextprotocol/tasks` extension, and that is still
//! running after [`TASK_AFTER`], becomes a task. The client gets the task
//! handle at once and polls `tasks/get`; the status message follows the
//! call's progress; `tasks/cancel` stops the call, which cancels it at the
//! backend too. A backend that runs the call as its own task is followed
//! by the backend connection, so both kinds look the same to the client.
//!
//! Task ids are 128 random bits, and each task belongs to the endpoint
//! (scope) that created it: another endpoint gets "no such task".

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use rmcp::model::{DetailedTask, JsonObject, Task, TaskPayload, TaskStatus};

use crate::progress::Progress;

/// How long a call runs before it becomes a task.
pub const TASK_AFTER: Duration = Duration::from_secs(1);

/// How long a task is kept, from its creation.
pub const TASK_TTL: Duration = Duration::from_secs(3600);

/// The poll interval suggested to clients.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

struct Entry {
    scope: String,
    task: Task,
    payload: TaskPayload,
    abort: Option<tokio::task::AbortHandle>,
    expires: Instant,
}

/// Every task of this process (fronts are created per request, tasks
/// outlive them).
#[derive(Default)]
pub struct Tasks {
    entries: Mutex<HashMap<String, Entry>>,
}

fn now() -> String {
    rmcp::task_manager::current_timestamp()
}

fn new_id() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("system randomness");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A task that can't be found (or belongs to another endpoint).
#[derive(Debug, Clone, PartialEq)]
pub struct NoSuchTask(pub String);

impl Tasks {
    /// The process-wide store.
    pub fn global() -> Arc<Tasks> {
        static T: OnceLock<Arc<Tasks>> = OnceLock::new();
        T.get_or_init(Arc::default).clone()
    }

    fn purge(entries: &mut HashMap<String, Entry>) {
        let now = Instant::now();
        entries.retain(|_, e| e.expires > now);
    }

    /// Register a working task for `scope`; returns its seed state.
    pub fn create(&self, scope: &str, abort: tokio::task::AbortHandle) -> Task {
        let at = now();
        let task = Task::new(new_id(), TaskStatus::Working, at.clone(), at)
            .with_ttl_ms(TASK_TTL.as_millis() as u64)
            .with_poll_interval_ms(POLL_INTERVAL.as_millis() as u64);
        let mut entries = self.entries.lock().expect("tasks");
        Self::purge(&mut entries);
        entries.insert(
            task.task_id.clone(),
            Entry {
                scope: scope.to_string(),
                task: task.clone(),
                payload: TaskPayload::Working,
                abort: Some(abort),
                expires: Instant::now() + TASK_TTL,
            },
        );
        task
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Entry)) {
        if let Some(e) = self.entries.lock().expect("tasks").get_mut(id)
            && !e.task.status.is_terminal()
        {
            f(e);
            e.task.status = e.payload.status();
            e.task.last_updated_at = now();
        }
    }

    /// The call reported progress: it becomes the status message.
    pub fn progress(&self, id: &str, p: &Progress) {
        let text = match (&p.message, p.total) {
            (Some(m), Some(t)) => format!("{m} ({}/{})", p.progress, t),
            (Some(m), None) => m.clone(),
            (None, Some(t)) => format!("{}/{}", p.progress, t),
            (None, None) => format!("{}", p.progress),
        };
        self.update(id, |e| e.task.status_message = Some(text));
    }

    /// The call finished with a `CallToolResult`.
    pub fn complete(&self, id: &str, result: JsonObject) {
        self.update(id, |e| {
            e.payload = TaskPayload::Completed { result };
            e.abort = None;
        });
    }

    /// The call failed with a JSON-RPC error (`{code, message, data}`).
    pub fn fail(&self, id: &str, error: JsonObject) {
        self.update(id, |e| {
            e.payload = TaskPayload::Failed { error };
            e.abort = None;
        });
    }

    /// The task's current state, for `tasks/get`.
    pub fn get(&self, scope: &str, id: &str) -> Result<DetailedTask, NoSuchTask> {
        let mut entries = self.entries.lock().expect("tasks");
        Self::purge(&mut entries);
        match entries.get(id) {
            Some(e) if e.scope == scope => Ok(DetailedTask::new(e.task.clone(), e.payload.clone())),
            _ => Err(NoSuchTask(id.to_string())),
        }
    }

    /// `tasks/cancel`: stop the call. A finished task stays as it is.
    pub fn cancel(&self, scope: &str, id: &str) -> Result<(), NoSuchTask> {
        let mut entries = self.entries.lock().expect("tasks");
        match entries.get_mut(id) {
            Some(e) if e.scope == scope => {
                if !e.task.status.is_terminal() {
                    if let Some(a) = e.abort.take() {
                        a.abort();
                    }
                    e.payload = TaskPayload::Cancelled;
                    e.task.status = TaskStatus::Cancelled;
                    e.task.status_message = Some("cancelled by the client".into());
                    e.task.last_updated_at = now();
                }
                Ok(())
            }
            _ => Err(NoSuchTask(id.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lifecycle_scope_and_cancel() {
        let tasks = Tasks::default();
        let job = tokio::spawn(std::future::pending::<()>());
        let t = tasks.create("profile a", job.abort_handle());
        assert_eq!(t.task_id.len(), 32);
        assert_eq!(t.status, TaskStatus::Working);
        assert_eq!(t.poll_interval_ms, Some(500));

        tasks.progress(
            &t.task_id,
            &Progress {
                progress: 3.0,
                total: Some(10.0),
                message: Some("copying".into()),
            },
        );
        let got = tasks.get("profile a", &t.task_id).unwrap();
        assert_eq!(got.task.status_message.as_deref(), Some("copying (3/10)"));
        // another endpoint can't see or cancel it
        assert!(tasks.get("profile b", &t.task_id).is_err());
        assert!(tasks.cancel("profile b", &t.task_id).is_err());

        tasks.cancel("profile a", &t.task_id).unwrap();
        assert!(job.await.unwrap_err().is_cancelled());
        let got = tasks.get("profile a", &t.task_id).unwrap();
        assert_eq!(got.status(), TaskStatus::Cancelled);
        // terminal states stick
        tasks.complete(&t.task_id, JsonObject::new());
        assert_eq!(
            tasks.get("profile a", &t.task_id).unwrap().status(),
            TaskStatus::Cancelled
        );

        let job = tokio::spawn(std::future::pending::<()>());
        let t2 = tasks.create("profile a", job.abort_handle());
        let mut r = JsonObject::new();
        r.insert("content".into(), serde_json::json!([]));
        tasks.complete(&t2.task_id, r.clone());
        let got = tasks.get("profile a", &t2.task_id).unwrap();
        assert_eq!(got.payload, TaskPayload::Completed { result: r });
        tasks.cancel("profile a", &t2.task_id).unwrap(); // no-op
        assert_eq!(
            tasks.get("profile a", &t2.task_id).unwrap().status(),
            TaskStatus::Completed
        );
    }
}
