//! Named services other plugins can call.
//!
//! Values cross the cdylib boundary as JSON (`serde_json::Value`). Do not use
//! `Any` downcast — TypeId is not shared between a statically-linked `.so` and
//! the host.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

#[async_trait]
pub trait Service: Send + Sync {
    async fn call(&self, method: &str, args: Value) -> anyhow::Result<Value>;
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceInfo {
    pub name: String,
    pub owner: String,
}

struct Slot {
    owner: String,
    service: Arc<dyn Service>,
    gen: u64,
}

#[derive(Clone, Default)]
pub struct ServiceHub {
    inner: Arc<RwLock<HashMap<String, Slot>>>,
    gens: Arc<AtomicU64>,
}

impl ServiceHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(name)
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        v.sort();
        v
    }

    pub fn list(&self) -> Vec<ServiceInfo> {
        let mut v: Vec<ServiceInfo> = self
            .inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(name, slot)| ServiceInfo {
                name: name.clone(),
                owner: slot.owner.clone(),
            })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn owner(&self, name: &str) -> Option<String> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .map(|s| s.owner.clone())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Service>> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .map(|s| s.service.clone())
    }

    /// Same owner may replace. A different owner cannot steal the name.
    /// Returns a generation; `revoke` with a stale gen is a no-op (hot reload).
    pub fn provide(
        &self,
        name: &str,
        owner: &str,
        service: Arc<dyn Service>,
    ) -> anyhow::Result<u64> {
        anyhow::ensure!(valid_service_name(name), "invalid service name `{name}`");
        let gen = self.gens.fetch_add(1, Ordering::Relaxed) + 1;
        let mut g = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = g.get(name) {
            anyhow::ensure!(
                old.owner == owner,
                "service `{name}` already provided by `{}`",
                old.owner
            );
        }
        g.insert(
            name.to_string(),
            Slot {
                owner: owner.to_string(),
                service,
                gen,
            },
        );
        Ok(gen)
    }

    pub fn revoke(&self, name: &str, owner: &str, gen: u64) -> bool {
        let mut g = self.inner.write().unwrap_or_else(|e| e.into_inner());
        match g.get(name) {
            Some(s) if s.owner == owner && s.gen == gen => {
                g.remove(name);
                true
            }
            _ => false,
        }
    }

    pub async fn call(&self, name: &str, method: &str, args: Value) -> anyhow::Result<Value> {
        let svc = self
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("no service `{name}`"))?;
        svc.call(method, args).await
    }
}

pub fn valid_service_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '#')
        && name.len() <= 80
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct ConstSvc(Value);

    #[async_trait]
    impl Service for ConstSvc {
        async fn call(&self, method: &str, _args: Value) -> anyhow::Result<Value> {
            anyhow::ensure!(method == "get", "unknown method {method}");
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn provide_call_revoke() {
        let hub = ServiceHub::new();
        let gen = hub
            .provide("echo", "p", Arc::new(ConstSvc(json!("hi"))))
            .unwrap();
        assert!(hub.contains("echo"));
        assert_eq!(
            hub.call("echo", "get", json!({})).await.unwrap(),
            json!("hi")
        );
        assert!(hub.revoke("echo", "p", gen));
        assert!(!hub.contains("echo"));
        assert!(hub.call("echo", "get", json!({})).await.is_err());
    }

    #[test]
    fn owner_cannot_steal() {
        let hub = ServiceHub::new();
        hub.provide("x", "a", Arc::new(ConstSvc(json!(1)))).unwrap();
        assert!(hub.provide("x", "b", Arc::new(ConstSvc(json!(2)))).is_err());
        hub.provide("x", "a", Arc::new(ConstSvc(json!(3)))).unwrap();
        assert_eq!(hub.owner("x").as_deref(), Some("a"));
    }
}
