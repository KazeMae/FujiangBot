//! Plugin event bus. Payloads are JSON so they cross the cdylib boundary.
//!
//! Closest Cordis counterparts: `emit` / `parallel` / `serial` / `waterfall`.
//! There is no sync `bail` — everything is async.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use tracing::error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventResult {
    /// Keep going. `waterfall` uses `Continue(Some(v))` as the next payload.
    Continue(Option<Value>),
    /// Stop `serial` / `waterfall`. Ignored by `emit` / `parallel`.
    Stop(Option<Value>),
}

impl Default for EventResult {
    fn default() -> Self {
        Self::Continue(None)
    }
}

#[async_trait]
pub trait EventHandler: Send + Sync {
    async fn handle(&self, event: &str, payload: Value) -> anyhow::Result<EventResult>;
}

/// Wrap an async closure as a handler.
pub fn event_fn<F, Fut>(f: F) -> Arc<dyn EventHandler>
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<EventResult>> + Send + 'static,
{
    struct H<F>(F);
    #[async_trait]
    impl<F, Fut> EventHandler for H<F>
    where
        F: Fn(Value) -> Fut + Send + Sync,
        Fut: Future<Output = anyhow::Result<EventResult>> + Send,
    {
        async fn handle(&self, _event: &str, payload: Value) -> anyhow::Result<EventResult> {
            (self.0)(payload).await
        }
    }
    Arc::new(H(f))
}

#[derive(Clone)]
struct Hook {
    id: u64,
    plugin: String,
    handler: Arc<dyn EventHandler>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventInfo {
    pub name: String,
    pub listeners: usize,
}

struct Inner {
    next_id: AtomicU64,
    hooks: RwLock<HashMap<String, Vec<Hook>>>,
}

#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Inner>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                next_id: AtomicU64::new(1),
                hooks: RwLock::new(HashMap::new()),
            }),
        }
    }

    pub fn on(
        &self,
        event: &str,
        plugin: &str,
        handler: Arc<dyn EventHandler>,
        prepend: bool,
    ) -> u64 {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let hook = Hook {
            id,
            plugin: plugin.to_string(),
            handler,
        };
        let mut g = self.inner.hooks.write().unwrap_or_else(|e| e.into_inner());
        let list = g.entry(event.to_string()).or_default();
        if prepend {
            list.insert(0, hook);
        } else {
            list.push(hook);
        }
        id
    }

    pub fn off(&self, id: u64) -> bool {
        let mut g = self.inner.hooks.write().unwrap_or_else(|e| e.into_inner());
        for list in g.values_mut() {
            if let Some(i) = list.iter().position(|h| h.id == id) {
                list.remove(i);
                return true;
            }
        }
        false
    }

    pub fn list(&self) -> Vec<EventInfo> {
        let g = self.inner.hooks.read().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<EventInfo> = g
            .iter()
            .filter(|(_, hooks)| !hooks.is_empty())
            .map(|(name, hooks)| EventInfo {
                name: name.clone(),
                listeners: hooks.len(),
            })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    fn hooks(&self, event: &str) -> Vec<Hook> {
        self.inner
            .hooks
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(event)
            .cloned()
            .unwrap_or_default()
    }

    /// Fire-and-forget. Errors are logged.
    pub fn emit(&self, event: impl Into<String>, payload: Value) {
        let bus = self.clone();
        let event = event.into();
        tokio::spawn(async move {
            bus.parallel(&event, payload).await;
        });
    }

    pub async fn parallel(&self, event: &str, payload: Value) {
        let hooks = self.hooks(event);
        let mut joins = Vec::with_capacity(hooks.len());
        for h in hooks {
            let event = event.to_string();
            let payload = payload.clone();
            joins.push(tokio::spawn(async move {
                if let Err(e) = h.handler.handle(&event, payload).await {
                    error!(plugin = %h.plugin, event = %event, error = %e, "event handler");
                }
            }));
        }
        for j in joins {
            let _ = j.await;
        }
    }

    /// Sequential. `Stop` ends the chain and is returned.
    pub async fn serial(&self, event: &str, payload: Value) -> Option<Value> {
        for h in self.hooks(event) {
            match h.handler.handle(event, payload.clone()).await {
                Ok(EventResult::Stop(v)) => return v,
                Ok(EventResult::Continue(_)) => {}
                Err(e) => {
                    error!(plugin = %h.plugin, event = %event, error = %e, "event handler");
                }
            }
        }
        None
    }

    /// Sequential transform. `Continue(Some(v))` / `Stop(Some(v))` replace the payload.
    pub async fn waterfall(&self, event: &str, mut payload: Value) -> Value {
        for h in self.hooks(event) {
            match h.handler.handle(event, payload.clone()).await {
                Ok(EventResult::Continue(Some(v))) => payload = v,
                Ok(EventResult::Continue(None)) => {}
                Ok(EventResult::Stop(v)) => return v.unwrap_or(payload),
                Err(e) => {
                    error!(plugin = %h.plugin, event = %event, error = %e, "event handler");
                }
            }
        }
        payload
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn serial_stops() {
        let bus = EventBus::new();
        bus.on(
            "t",
            "a",
            event_fn(|_p| async { Ok(EventResult::Continue(None)) }),
            false,
        );
        bus.on(
            "t",
            "b",
            event_fn(|_p| async { Ok(EventResult::Stop(Some(json!(7)))) }),
            false,
        );
        bus.on(
            "t",
            "c",
            event_fn(|_p| async {
                panic!("should not run");
            }),
            false,
        );
        assert_eq!(bus.serial("t", json!(null)).await, Some(json!(7)));
    }

    #[tokio::test]
    async fn waterfall_transforms() {
        let bus = EventBus::new();
        bus.on(
            "t",
            "a",
            event_fn(|p| async move {
                let n = p.as_i64().unwrap_or(0);
                Ok(EventResult::Continue(Some(json!(n + 1))))
            }),
            false,
        );
        bus.on(
            "t",
            "b",
            event_fn(|p| async move {
                let n = p.as_i64().unwrap_or(0);
                Ok(EventResult::Continue(Some(json!(n * 10))))
            }),
            false,
        );
        assert_eq!(bus.waterfall("t", json!(3)).await, json!(40));
    }

    #[tokio::test]
    async fn off_removes() {
        let bus = EventBus::new();
        let id = bus.on(
            "t",
            "a",
            event_fn(|_p| async { Ok(EventResult::Stop(Some(json!(1)))) }),
            false,
        );
        assert!(bus.off(id));
        assert_eq!(bus.serial("t", json!(null)).await, None);
    }
}
