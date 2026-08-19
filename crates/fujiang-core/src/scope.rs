use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Per-plugin lifetime. Tasks spawned here are cancelled and aborted on `dispose`.
pub struct PluginScope {
    stop: CancellationToken,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Default for PluginScope {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginScope {
    pub fn new() -> Self {
        Self {
            stop: CancellationToken::new(),
            tasks: Mutex::new(Vec::new()),
        }
    }

    pub fn stop_token(&self) -> CancellationToken {
        self.stop.clone()
    }

    pub fn is_cancelled(&self) -> bool {
        self.stop.is_cancelled()
    }

    /// Run `fut` until it finishes or the scope is disposed.
    pub fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if self.stop.is_cancelled() {
            return;
        }
        let stop = self.stop.clone();
        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = stop.cancelled() => {}
                _ = fut => {}
            }
        });
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(handle);
    }

    pub async fn sleep(&self, dur: Duration) {
        tokio::select! {
            _ = self.stop.cancelled() => {}
            _ = tokio::time::sleep(dur) => {}
        }
    }

    pub async fn dispose(&self) {
        self.stop.cancel();
        let tasks: Vec<_> = {
            let mut g = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            g.drain(..).collect()
        };
        for t in tasks {
            t.abort();
        }
    }
}

impl Drop for PluginScope {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Ok(mut g) = self.tasks.lock() {
            for t in g.drain(..) {
                t.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn dispose_cancels_spawned_task() {
        let scope = PluginScope::new();
        let started = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let s = started.clone();
        let f = finished.clone();
        scope.spawn(async move {
            s.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(30)).await;
            f.store(true, Ordering::SeqCst);
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(started.load(Ordering::SeqCst));
        scope.dispose().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!finished.load(Ordering::SeqCst));
        assert!(scope.is_cancelled());
    }

    #[tokio::test]
    async fn sleep_returns_on_dispose() {
        let scope = Arc::new(PluginScope::new());
        let ticks = Arc::new(AtomicU32::new(0));
        let t = ticks.clone();
        let sc = scope.clone();
        let h = tokio::spawn(async move {
            sc.sleep(Duration::from_secs(30)).await;
            t.fetch_add(1, Ordering::SeqCst);
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        scope.dispose().await;
        h.await.unwrap();
        assert_eq!(ticks.load(Ordering::SeqCst), 1);
    }
}
