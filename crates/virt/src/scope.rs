use std::{
    future::Future,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::FutureExt;
use tokio::{
    sync::oneshot,
    task::{AbortHandle, JoinHandle},
};
use tokio_util::sync::CancellationToken;
use virt_protocol::{Error, Result};

type Cleanup = Box<dyn FnOnce() + Send>;

struct State {
    open: bool,
    cleanup: Vec<Cleanup>,
    tasks: Vec<JoinHandle<()>>,
}

/// Revocable registration and async task scope. Only trusted native plugins use this.
#[derive(Clone)]
pub(crate) struct Scope {
    owner: String,
    state: Arc<Mutex<State>>,
    pub(crate) cancelled: CancellationToken,
    failures: Arc<Mutex<Vec<Error>>>,
}

pub(crate) struct ScopedTask<T> {
    owner: String,
    receiver: oneshot::Receiver<Result<T>>,
    abort: AbortHandle,
}

impl<T> ScopedTask<T> {
    pub(crate) async fn wait(mut self, timeout: Duration) -> Result<T> {
        match tokio::time::timeout(timeout, &mut self.receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(Error::ScopeClosed(self.owner.clone())),
            Err(_) => Err(Error::Timeout(self.owner.clone())),
        }
    }
}

impl<T> Drop for ScopedTask<T> {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

impl Scope {
    pub(crate) fn new(owner: String) -> Self {
        Self {
            owner,
            state: Arc::new(Mutex::new(State {
                open: true,
                cleanup: vec![],
                tasks: vec![],
            })),
            cancelled: CancellationToken::new(),
            failures: Arc::default(),
        }
    }

    pub(crate) fn ensure_open(&self) -> Result<()> {
        if self.state.lock().expect("scope lock").open {
            Ok(())
        } else {
            Err(Error::ScopeClosed(self.owner.clone()))
        }
    }

    // Registration + cleanup enlistment are atomic with respect to close.
    pub(crate) fn register<T>(
        &self,
        operation: impl FnOnce() -> Result<(T, Cleanup)>,
    ) -> Result<T> {
        let mut state = self.state.lock().expect("scope lock");
        if !state.open {
            return Err(Error::ScopeClosed(self.owner.clone()));
        }
        if state.cleanup.len() >= 1024 {
            return Err(Error::ResourceLimit(self.owner.clone()));
        }
        let (value, cleanup) = operation()?;
        state.cleanup.push(cleanup);
        Ok(value)
    }

    pub(crate) fn spawn<T: Send + 'static>(
        &self,
        future: impl Future<Output = Result<T>> + Send + 'static,
    ) -> Result<ScopedTask<T>> {
        let mut state = self.state.lock().expect("scope lock");
        if !state.open {
            return Err(Error::ScopeClosed(self.owner.clone()));
        }
        state.tasks.retain(|task| !task.is_finished());
        if state.tasks.len() >= 128 {
            return Err(Error::ResourceLimit(self.owner.clone()));
        }
        let (sender, receiver) = oneshot::channel();
        let owner = self.owner.clone();
        let task = tokio::spawn(async move {
            let result = AssertUnwindSafe(future)
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err(Error::PluginPanic(owner)));
            let _ = sender.send(result);
        });
        let abort = task.abort_handle();
        state.tasks.push(task);
        Ok(ScopedTask {
            owner: self.owner.clone(),
            receiver,
            abort,
        })
    }

    pub(crate) fn background(
        &self,
        future: impl Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        let mut state = self.state.lock().expect("scope lock");
        if !state.open {
            return Err(Error::ScopeClosed(self.owner.clone()));
        }
        state.tasks.retain(|task| !task.is_finished());
        let failures = self.failures.clone();
        if state.tasks.len() >= 128 {
            return Err(Error::ResourceLimit(self.owner.clone()));
        }
        let owner = self.owner.clone();
        state.tasks.push(tokio::spawn(async move {
            if AssertUnwindSafe(future).catch_unwind().await.is_err() {
                let mut failures = failures.lock().expect("failure lock");
                if failures.len() == 32 {
                    failures.remove(0);
                }
                failures.push(Error::PluginPanic(owner));
            }
        }));
        Ok(())
    }

    pub(crate) fn task_count(&self) -> usize {
        self.state
            .lock()
            .expect("scope lock")
            .tasks
            .iter()
            .filter(|task| !task.is_finished())
            .count()
    }

    pub(crate) fn failures(&self) -> Vec<Error> {
        self.failures.lock().expect("failure lock").clone()
    }

    pub(crate) fn close(&self) {
        let (cleanup, aborts) = {
            let mut state = self.state.lock().expect("scope lock");
            state.open = false;
            self.cancelled.cancel();
            (
                std::mem::take(&mut state.cleanup),
                state
                    .tasks
                    .iter()
                    .map(JoinHandle::abort_handle)
                    .collect::<Vec<_>>(),
            )
        };
        for cleanup in cleanup.into_iter().rev() {
            // Internal cleanup must not prevent remaining resources from being revoked.
            let _ = std::panic::catch_unwind(AssertUnwindSafe(cleanup));
        }
        for abort in aborts {
            abort.abort();
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.close();
        let tasks = std::mem::take(&mut self.state.lock().expect("scope lock").tasks);
        for task in tasks {
            let _ = task.await;
        }
    }
}
