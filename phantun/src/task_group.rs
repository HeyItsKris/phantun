use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
struct TaskGroupInner {
    in_flight: AtomicUsize,
    drained: Notify,
}

#[derive(Clone, Debug)]
pub struct TaskGroup {
    cancel: CancellationToken,
    inner: Arc<TaskGroupInner>,
}

impl Default for TaskGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskGroup {
    pub fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            inner: Arc::new(TaskGroupInner::default()),
        }
    }

    pub fn token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.inner.in_flight.fetch_add(1, Ordering::AcqRel);
        let guard = InFlightGuard::new(self.inner.clone());

        tokio::spawn(async move {
            let _guard = guard;
            future.await;
        });
    }

    pub async fn wait(&self) {
        loop {
            if self.inner.in_flight.load(Ordering::Acquire) == 0 {
                return;
            }

            let notified = self.inner.drained.notified();
            if self.inner.in_flight.load(Ordering::Acquire) == 0 {
                return;
            }

            notified.await;
        }
    }

    pub async fn wait_timeout(&self, wait_timeout: Duration) -> bool {
        timeout(wait_timeout, self.wait()).await.is_ok()
    }
}

#[derive(Debug)]
struct InFlightGuard {
    inner: Arc<TaskGroupInner>,
}

impl InFlightGuard {
    fn new(inner: Arc<TaskGroupInner>) -> Self {
        Self { inner }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if self.inner.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.inner.drained.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TaskGroup;
    use std::time::Duration;

    #[tokio::test]
    async fn wait_does_not_block_on_panicking_task() {
        let group = TaskGroup::new();
        group.spawn(async {
            panic!("boom");
        });

        assert!(group.wait_timeout(Duration::from_millis(200)).await);
    }
}
