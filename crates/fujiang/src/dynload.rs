use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Context;
use fujiang_core::{BotContext, Dispatcher, Plugin, PluginSnapshot, PLUGIN_ABI};
use libloading::{Library, Symbol};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::plugins;

type AbiFn = unsafe extern "C" fn() -> u32;
type CreateFn = unsafe extern "C" fn() -> *mut Box<dyn Plugin>;

pub struct LoadedSo {
    pub path: PathBuf,
    pub mtime: Option<SystemTime>,
    pub snapshot: PluginSnapshot,
    plugin: Arc<dyn Plugin>,
    /// Must outlive `plugin`.
    _lib: Library,
}

pub struct PluginHub {
    dir: PathBuf,
    loaded: Mutex<HashMap<String, LoadedSo>>,
    disabled: Mutex<HashSet<String>>,
    last_error: Mutex<HashMap<String, String>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DynamicPluginView {
    pub name: String,
    pub version: String,
    pub description: String,
    pub commands: Vec<String>,
    pub path: String,
    pub enabled: bool,
    pub config: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl PluginHub {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            loaded: Mutex::new(HashMap::new()),
            disabled: Mutex::new(HashSet::new()),
            last_error: Mutex::new(HashMap::new()),
        }
    }

    async fn set_error(&self, name: &str, err: Option<String>) {
        let mut g = self.last_error.lock().await;
        if let Some(e) = err {
            g.insert(name.to_string(), e);
        } else {
            g.remove(name);
        }
    }

    pub async fn set_disabled(&self, names: impl IntoIterator<Item = String>) {
        *self.disabled.lock().await = names.into_iter().collect();
    }

    pub async fn is_disabled(&self, name: &str) -> bool {
        self.disabled.lock().await.contains(name)
    }

    pub async fn plugin(&self, name: &str) -> Option<Arc<dyn Plugin>> {
        self.loaded.lock().await.get(name).map(|s| s.plugin.clone())
    }

    pub async fn list(
        &self,
        running: &[String],
        configs: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Vec<DynamicPluginView> {
        let errors = self.last_error.lock().await.clone();
        self.loaded
            .lock()
            .await
            .values()
            .map(|s| DynamicPluginView {
                name: s.snapshot.name.clone(),
                version: s.snapshot.version.clone(),
                description: s.snapshot.description.clone(),
                commands: s.snapshot.commands.clone(),
                path: s.path.display().to_string(),
                enabled: running.iter().any(|n| n == &s.snapshot.name),
                config: configs
                    .get(&s.snapshot.name)
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({})),
                last_error: errors.get(&s.snapshot.name).cloned(),
            })
            .collect()
    }

    pub async fn load(
        &self,
        path: &Path,
        dispatcher: &Dispatcher,
        ctx: &BotContext,
    ) -> anyhow::Result<String> {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let (lib, plugin) = unsafe { open_plugin(&path)? };
        let name = plugin.name().to_string();
        if plugins::NAMES.contains(&name.as_str()) {
            anyhow::bail!("「{name}」是内置插件，不能用 .so 覆盖");
        }
        {
            let guard = self.loaded.lock().await;
            if guard.contains_key(&name) {
                anyhow::bail!("动态插件「{name}」已加载");
            }
        }
        let snapshot = PluginSnapshot::from_plugin(plugin.as_ref());
        if !self.is_disabled(&name).await {
            dispatcher.insert(plugin.clone(), ctx).await?;
        }
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        self.loaded.lock().await.insert(
            name.clone(),
            LoadedSo {
                path,
                mtime,
                snapshot,
                plugin,
                _lib: lib,
            },
        );
        info!(plugin = %name, "loaded dynamic plugin");
        Ok(name)
    }

    pub async fn unload(&self, name: &str, dispatcher: &Dispatcher) -> anyhow::Result<bool> {
        if plugins::NAMES.contains(&name) {
            anyhow::bail!("内置插件请用配置开关，不要 unload");
        }
        let Some(slot) = self.loaded.lock().await.remove(name) else {
            return Ok(false);
        };
        dispatcher.remove(name).await?;
        wait_unique(&slot.plugin).await;
        drop(slot);
        info!(plugin = name, "unloaded dynamic plugin");
        Ok(true)
    }

    pub async fn reload(
        &self,
        name: &str,
        dispatcher: &Dispatcher,
        ctx: &BotContext,
    ) -> anyhow::Result<String> {
        let path = {
            let guard = self.loaded.lock().await;
            guard
                .get(name)
                .map(|s| s.path.clone())
                .ok_or_else(|| anyhow::anyhow!("动态插件「{name}」未加载"))?
        };
        let (lib, plugin) = match unsafe { open_plugin(&path) } {
            Ok(v) => v,
            Err(e) => {
                self.set_error(name, Some(format!("{e:#}"))).await;
                return Err(e);
            }
        };
        let new_name = plugin.name().to_string();
        if new_name != name {
            self.set_error(
                name,
                Some(format!("新库名字是「{new_name}」，不是「{name}」")),
            )
            .await;
            anyhow::bail!("新库名字是「{new_name}」，不是「{name}」");
        }
        if plugins::NAMES.contains(&new_name.as_str()) {
            anyhow::bail!("「{new_name}」是内置插件，不能用 .so 覆盖");
        }

        let running = dispatcher.names().await.iter().any(|n| n == name);
        let start = running || !self.is_disabled(name).await;
        if start {
            if let Err(e) = dispatcher.replace(plugin.clone(), ctx).await {
                self.set_error(name, Some(format!("{e:#}"))).await;
                drop(plugin);
                drop(lib);
                return Err(e);
            }
        }

        let snapshot = PluginSnapshot::from_plugin(plugin.as_ref());
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let old = self.loaded.lock().await.insert(
            name.to_string(),
            LoadedSo {
                path,
                mtime,
                snapshot,
                plugin,
                _lib: lib,
            },
        );
        if let Some(old) = old {
            wait_unique(&old.plugin).await;
            drop(old);
        }
        self.set_error(name, None).await;
        info!(plugin = name, "reloaded dynamic plugin");
        Ok(name.to_string())
    }

    /// Load new files, reload changed, unload missing. Returns names that changed.
    pub async fn scan(
        &self,
        dispatcher: &Dispatcher,
        ctx: &BotContext,
    ) -> anyhow::Result<Vec<String>> {
        tokio::fs::create_dir_all(&self.dir).await.ok();
        let mut changed = Vec::new();
        let on_disk = list_plugin_files(&self.dir)?;

        let loaded_paths: HashMap<PathBuf, (String, Option<SystemTime>)> = {
            let guard = self.loaded.lock().await;
            guard
                .values()
                .map(|s| (s.path.clone(), (s.snapshot.name.clone(), s.mtime)))
                .collect()
        };

        for (path, name) in &loaded_paths {
            if !on_disk.iter().any(|(p, _)| p == path) {
                if self.unload(name.0.as_str(), dispatcher).await? {
                    changed.push(format!("unload {name}", name = name.0));
                }
            }
        }

        for (path, mtime) in &on_disk {
            match loaded_paths.get(path) {
                None => match self.load(path, dispatcher, ctx).await {
                    Ok(n) => changed.push(format!("load {n}")),
                    Err(e) => warn!(path = %path.display(), error = %e, "skip plugin so"),
                },
                Some((name, old_mtime)) if old_mtime != &Some(*mtime) && old_mtime.is_some() => {
                    match self.reload(name, dispatcher, ctx).await {
                        Ok(n) => changed.push(format!("reload {n}")),
                        Err(e) => warn!(plugin = %name, error = %e, "reload plugin so"),
                    }
                }
                _ => {}
            }
        }
        changed.extend(self.reconcile(dispatcher, ctx).await?);
        Ok(changed)
    }

    /// Start / stop dynamic plugins to match the disabled set. Libraries stay loaded.
    pub async fn reconcile(
        &self,
        dispatcher: &Dispatcher,
        ctx: &BotContext,
    ) -> anyhow::Result<Vec<String>> {
        let mut changed = Vec::new();
        let disabled = self.disabled.lock().await.clone();
        let running = dispatcher.names().await;
        let names: Vec<String> = self.loaded.lock().await.keys().cloned().collect();
        for name in names {
            let want = !disabled.contains(&name);
            let is_on = running.iter().any(|n| n == &name);
            if is_on && !want {
                dispatcher.remove(&name).await?;
                changed.push(format!("disable {name}"));
            } else if want && !is_on {
                if let Some(p) = self.plugin(&name).await {
                    dispatcher.insert(p, ctx).await?;
                    changed.push(format!("enable {name}"));
                }
            }
        }
        Ok(changed)
    }

    pub async fn watch_loop(
        self: Arc<Self>,
        dispatcher: Arc<Dispatcher>,
        ctx: BotContext,
        stop: CancellationToken,
    ) {
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
            }
            if stop.is_cancelled() {
                break;
            }
            if let Err(e) = self.scan(&dispatcher, &ctx).await {
                warn!(error = %e, "plugin dir scan");
            }
        }
    }
}

async fn wait_unique(plugin: &Arc<dyn Plugin>) {
    for _ in 0..50 {
        if Arc::strong_count(plugin) <= 1 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    warn!(
        plugin = plugin.name(),
        refs = Arc::strong_count(plugin),
        "plugin still in use while unloading"
    );
}

unsafe fn open_plugin(path: &Path) -> anyhow::Result<(Library, Arc<dyn Plugin>)> {
    let lib = unsafe { Library::new(path) }.with_context(|| path.display().to_string())?;
    let abi: Symbol<AbiFn> =
        unsafe { lib.get(b"fujiang_plugin_abi") }.context("missing fujiang_plugin_abi")?;
    let abi_n = unsafe { abi() };
    anyhow::ensure!(
        abi_n == PLUGIN_ABI,
        "plugin ABI {abi_n} != host {PLUGIN_ABI}（请用同一份仓库重新编译 .so）"
    );
    let create: Symbol<CreateFn> =
        unsafe { lib.get(b"fujiang_create_plugin") }.context("missing fujiang_create_plugin")?;
    let raw = unsafe { create() };
    anyhow::ensure!(!raw.is_null(), "fujiang_create_plugin returned null");
    let plugin: Box<dyn Plugin> = unsafe { *Box::from_raw(raw) };
    Ok((lib, Arc::from(plugin)))
}

fn list_plugin_files(dir: &Path) -> anyhow::Result<Vec<(PathBuf, SystemTime)>> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for ent in std::fs::read_dir(dir)? {
        let ent = ent?;
        let path = ent.path();
        if !is_plugin_lib(&path) {
            continue;
        }
        let mtime = ent.metadata()?.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        out.push((path, mtime));
    }
    Ok(out)
}

pub fn is_plugin_lib(path: &Path) -> bool {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "so" | "dylib" | "dll" => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_lib_ext() {
        assert!(is_plugin_lib(Path::new("libfujiang_plugin_echo.dylib")));
        assert!(is_plugin_lib(Path::new("foo.so")));
        assert!(!is_plugin_lib(Path::new("readme.md")));
    }

    fn echo_cdylib() -> Option<PathBuf> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
        [
            "libfujiang_plugin_echo.dylib",
            "libfujiang_plugin_echo.so",
            "fujiang_plugin_echo.dll",
        ]
        .into_iter()
        .map(|n| root.join(n))
        .find(|p| p.exists())
    }

    #[test]
    fn load_echo_cdylib_if_built() {
        let Some(path) = echo_cdylib() else {
            return;
        };
        let (lib, plugin) = unsafe { open_plugin(&path).expect("load echo") };
        assert_eq!(plugin.name(), "echo");
        assert_eq!(plugin.meta().description, "示例热插插件：.ping → pong");
        drop(plugin);
        drop(lib);
    }
}
