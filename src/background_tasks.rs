//! Shared background task tracking, admission, and shutdown coordination.

use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use parking_lot::Mutex;
use tokio::{sync::Notify, task::JoinSet};
use tracing::warn;

#[cfg(test)]
use crate::clock::SystemClock;
use crate::{
    clock::{Clock, Sleep},
    error::EngineError,
};

/// Categorizes tracked background work for diagnostics.
#[derive(Debug, Clone, Copy)]
pub(crate) enum BackgroundTaskKind {
    Embed,
    AccessTracking,
    #[cfg(test)]
    Test,
}

impl std::fmt::Display for BackgroundTaskKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Embed => f.write_str("embed"),
            Self::AccessTracking => f.write_str("access_tracking"),
            #[cfg(test)]
            Self::Test => f.write_str("test"),
        }
    }
}

/// Coordinates tracked background work across the engine and orchestrator.
#[derive(Debug)]
pub(crate) struct BackgroundTasks {
    clock: Arc<dyn Clock>,
    tasks: Mutex<JoinSet<()>>,
    shutting_down: AtomicBool,
    admitted_spawns_closed: AtomicBool,
    reset_after_admission_timeout: AtomicBool,
    embed_admissions: AtomicUsize,
    embed_admissions_notify: Notify,
}

/// Admission guard for work that must be able to queue embed tasks before shutdown proceeds.
#[derive(Debug)]
pub(crate) struct EmbedAdmission {
    background_tasks: Arc<BackgroundTasks>,
}

impl BackgroundTasks {
    /// Create a new shared background task coordinator.
    #[cfg(test)]
    pub(crate) fn new() -> Arc<Self> {
        Self::new_with_clock(Arc::new(SystemClock::new()))
    }

    /// Create a coordinator driven by an injected clock.
    pub(crate) fn new_with_clock(clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            tasks: Mutex::new(JoinSet::new()),
            shutting_down: AtomicBool::new(false),
            admitted_spawns_closed: AtomicBool::new(false),
            reset_after_admission_timeout: AtomicBool::new(false),
            embed_admissions: AtomicUsize::new(0),
            embed_admissions_notify: Notify::new(),
        })
    }

    /// Returns `true` when shutdown has started.
    #[must_use]
    pub(crate) fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }

    /// Reserve the right to enqueue embed tasks for the current operation.
    pub(crate) fn begin_embed_admission(self: &Arc<Self>) -> Result<EmbedAdmission, EngineError> {
        if self.is_shutting_down() {
            return Err(EngineError::ShuttingDown);
        }

        let _previous = self.embed_admissions.fetch_add(1, Ordering::AcqRel);
        if self.is_shutting_down() {
            let previous = self.embed_admissions.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0, "embed admission count must stay positive");
            if previous == 1 {
                self.embed_admissions_notify.notify_waiters();
            }
            return Err(EngineError::ShuttingDown);
        }

        Ok(EmbedAdmission {
            background_tasks: Arc::clone(self),
        })
    }

    /// Spawn best-effort background work when shutdown is not in progress.
    ///
    /// Returns `true` when the task was admitted and spawned, `false` when it
    /// was skipped because shutdown had already started.
    pub(crate) fn spawn_best_effort<F>(&self, kind: BackgroundTaskKind, future: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self.tasks.lock();
        #[expect(unused_results, reason = "reap count is informational — callers only care about admission")]
        reap_completed_tasks_locked(&mut tasks);
        if self.is_shutting_down() {
            warn!(task_kind = %kind, "engine is shutting down, skipping background task");
            return false;
        }
        #[expect(unused_results, reason = "AbortHandle intentionally discarded — tasks run to completion")]
        tasks.spawn(future);
        true
    }

    /// Shut down tracked work with a timeout, draining tasks admitted before the shutdown gate closed.
    #[cfg(test)]
    pub(crate) async fn shutdown(&self, timeout: std::time::Duration) {
        self.shutdown_with_cleanup(timeout, || async {}).await;
    }

    /// Shut down tracked work, running cleanup before timed-out tasks are dropped.
    #[expect(clippy::integer_division_remainder_used, reason = "false positive from tokio::select! macro expansion")]
    pub(crate) async fn shutdown_with_cleanup<F, Fut>(&self, timeout: std::time::Duration, cleanup: F)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ()>,
    {
        let mut cleanup = Some(cleanup);
        self.shutting_down.store(true, Ordering::Release);
        let mut deadline = self.clock.sleep(timeout);
        let admissions_drained = self.wait_for_embed_admissions_until(&mut deadline).await;
        if !admissions_drained {
            self.admitted_spawns_closed.store(true, Ordering::Release);
            self.reset_after_admission_timeout.store(true, Ordering::Release);
            warn!(
                remaining_admissions = self.embed_admissions.load(Ordering::Acquire),
                "shutdown timed out while waiting for embed admissions; rejecting late embed spawns"
            );
        }

        let mut tasks = std::mem::take(&mut *self.tasks.lock());
        if !admissions_drained {
            warn!("shutdown timed out with {} remaining tasks, dropping them", tasks.len());
            if let Some(cleanup) = cleanup.take() {
                cleanup().await;
            }
            return;
        }

        loop {
            tokio::select! {
                result = tasks.join_next() => {
                    if result.is_none() {
                        break;
                    }
                }
                () = deadline.as_mut() => {
                    warn!("shutdown timed out with {} remaining tasks, dropping them", tasks.len());
                    if let Some(cleanup) = cleanup.take() {
                        cleanup().await;
                    }
                    break;
                }
            }
        }

        self.admitted_spawns_closed.store(false, Ordering::Release);
        self.shutting_down.store(false, Ordering::Release);
    }

    /// Wait until all in-flight embed admissions have been dropped.
    #[expect(clippy::integer_division_remainder_used, reason = "false positive from tokio::select! macro expansion")]
    async fn wait_for_embed_admissions_until(&self, deadline: &mut Sleep) -> bool {
        let notified = self.embed_admissions_notify.notified();
        tokio::pin!(notified);

        loop {
            // Register the waiter before checking the counter so a final drop
            // cannot race with `notified().await` and lose the wake-up.
            let already_notified = notified.as_mut().enable();
            if self.embed_admissions.load(Ordering::Acquire) == 0 {
                return true;
            }
            if !already_notified {
                tokio::select! {
                    () = notified.as_mut() => {}
                    () = deadline.as_mut() => return false,
                }
            }
            notified.set(self.embed_admissions_notify.notified());
        }
    }

    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub(crate) fn tracked_task_count(&self) -> usize {
        self.tasks.lock().len()
    }

    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub(crate) fn reap_completed_tasks_for_test(&self) -> usize {
        let mut tasks = self.tasks.lock();
        reap_completed_tasks_locked(&mut tasks)
    }

    #[cfg(test)]
    pub(crate) fn spawn_for_test<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let _spawned = self.spawn_admitted(BackgroundTaskKind::Test, future);
    }

    fn spawn_admitted<F>(&self, kind: BackgroundTaskKind, future: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self.tasks.lock();
        if self.admitted_spawns_closed.load(Ordering::Acquire) {
            warn!(task_kind = %kind, "shutdown timed out before admitted background task could be queued");
            return false;
        }
        #[expect(unused_results, reason = "reap count is informational — callers only care about task admission")]
        reap_completed_tasks_locked(&mut tasks);
        #[expect(unused_results, reason = "AbortHandle intentionally discarded — tasks run to completion")]
        tasks.spawn(future);
        true
    }
}

impl EmbedAdmission {
    /// Spawn embed-related work that was admitted before shutdown closed the gate.
    pub(crate) fn spawn<F>(&self, kind: BackgroundTaskKind, future: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.background_tasks.spawn_admitted(kind, future)
    }
}

impl Drop for EmbedAdmission {
    fn drop(&mut self) {
        let previous = self.background_tasks.embed_admissions.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "embed admission count must stay positive");
        if previous == 1 {
            if self.background_tasks.reset_after_admission_timeout.swap(false, Ordering::AcqRel) {
                self.background_tasks.admitted_spawns_closed.store(false, Ordering::Release);
                self.background_tasks.shutting_down.store(false, Ordering::Release);
            }
            self.background_tasks.embed_admissions_notify.notify_waiters();
        }
    }
}

/// Drain completed tasks from the `JoinSet`, logging any join errors.
#[expect(clippy::arithmetic_side_effects, reason = "counter bounded by JoinSet task count — cannot overflow")]
pub(crate) fn reap_completed_tasks_locked(tasks: &mut JoinSet<()>) -> usize {
    let mut completed = 0;
    while let Some(joined) = tasks.try_join_next() {
        completed += 1;
        if let Err(e) = joined {
            warn!("background task join error: {e}");
        }
    }
    completed
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use super::*;
    use crate::clock::MockClock;

    #[tokio::test]
    async fn admitted_embed_task_is_drained_during_shutdown() {
        let background_tasks = BackgroundTasks::new();
        let admission = background_tasks.begin_embed_admission().unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let spawned = admission.spawn(BackgroundTaskKind::Embed, async move {
            #[expect(clippy::let_underscore_must_use, reason = "test task only waits for completion")]
            let _ = rx.await;
        });
        assert!(spawned);

        let shutdown_tasks = Arc::clone(&background_tasks);
        let shutdown = tokio::spawn(async move {
            shutdown_tasks.shutdown(Duration::from_secs(1)).await;
        });

        while !background_tasks.is_shutting_down() {
            tokio::task::yield_now().await;
        }
        tx.send(()).unwrap();
        drop(admission);

        shutdown.await.unwrap();
        assert_eq!(background_tasks.tracked_task_count(), 0);
    }

    #[tokio::test]
    async fn shutdown_finishes_after_last_admission_drops_without_embed_tasks() {
        let background_tasks = BackgroundTasks::new();
        let admission = background_tasks.begin_embed_admission().unwrap();

        let shutdown_tasks = Arc::clone(&background_tasks);
        let shutdown = tokio::spawn(async move {
            shutdown_tasks.shutdown(Duration::from_secs(1)).await;
        });

        tokio::task::yield_now().await;
        drop(admission);

        shutdown.await.unwrap();
        assert_eq!(background_tasks.tracked_task_count(), 0);
    }

    #[tokio::test]
    async fn shutdown_timeout_includes_waiting_for_embed_admissions() {
        let clock = Arc::new(MockClock::new());
        let background_tasks = BackgroundTasks::new_with_clock(Arc::<MockClock>::clone(&clock));
        let admission = background_tasks.begin_embed_admission().unwrap();

        let shutdown = background_tasks.shutdown(Duration::from_millis(10));
        tokio::pin!(shutdown);
        assert!(futures::poll!(shutdown.as_mut()).is_pending());
        clock.advance(chrono::TimeDelta::milliseconds(10));
        shutdown.await;

        let spawned = admission.spawn(BackgroundTaskKind::Test, async {});
        assert!(!spawned, "timed-out admitted spawns should be surfaced to callers");
        assert_eq!(background_tasks.tracked_task_count(), 0, "timed-out admitted spawns should not be queued");
        drop(admission);

        assert!(
            background_tasks.begin_embed_admission().is_ok(),
            "background tasks should recover once timed-out admissions drain"
        );
    }
}
