use std::collections::HashMap;
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
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DynamicPluginView {
    pub name: String,
    pub version: String,
    pub description: String,
    pub commands: Vec<String>,
    pub path: String,
    pub enabled: bool,
}

impl PluginHub {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            loaded: Mutex::new(HashMap::new()),
        }
    }

    pub async fn list(&self, running: &[String]) -> Vec<DynamicPluginView> {
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
        dispatcher.insert(plugin.clone(), ctx).await?;
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
        self.unload(name, dispatcher).await?;
        self.load(&path, dispatcher, ctx).await
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
