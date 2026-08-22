#[cfg(not(target_family = "wasm"))]
pub use native::{cancel, spawn_detached, spawn_task, SpawnedTask};
#[cfg(target_family = "wasm")]
pub use wasm::{cancel, spawn_detached, spawn_task, SpawnedTask};

#[cfg(not(target_family = "wasm"))]
mod native {
    use tokio::task::JoinHandle;

    pub type SpawnedTask = JoinHandle<()>;

    pub fn spawn_task<F>(future: F) -> SpawnedTask
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(future)
    }

    pub fn spawn_detached<F>(future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(future);
    }

    /// Stops a task, taking it by value.
    ///
    /// By value because the two targets disagree about the receiver —
    /// `JoinHandle::abort` takes `&self` and the wasm handle needs `&mut` — and
    /// a caller holding an `Option<SpawnedTask>` should not have to know which
    /// it is compiling for.
    pub fn cancel(task: SpawnedTask) {
        task.abort();
    }
}

#[cfg(target_family = "wasm")]
mod wasm {
    #[derive(Debug)]
    pub struct SpawnedTask {
        cancel: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl SpawnedTask {
        pub fn abort(&mut self) {
            if let Some(sender) = self.cancel.take() {
                let _ = sender.send(());
            }
        }
    }

    impl Drop for SpawnedTask {
        fn drop(&mut self) {
            self.abort();
        }
    }

    pub fn spawn_task<F>(future: F) -> SpawnedTask
    where
        F: std::future::Future<Output = ()> + 'static,
    {
        let (cancel, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        wasm_bindgen_futures::spawn_local(async move {
            tokio::select! {
                _ = future => {}
                _ = cancel_rx => {}
            }
        });
        SpawnedTask {
            cancel: Some(cancel),
        }
    }

    pub fn spawn_detached<F>(future: F)
    where
        F: std::future::Future<Output = ()> + 'static,
    {
        wasm_bindgen_futures::spawn_local(future);
    }

    /// Stops a task, taking it by value. Dropping it is what cancels it here.
    pub fn cancel(task: SpawnedTask) {
        drop(task);
    }
}
