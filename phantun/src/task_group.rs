use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;
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
        let inner = self.inner.clone();

        tokio::spawn(async move {
            future.await;
            if inner.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
                inner.drained.notify_waiters();
            }
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
}
