use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::command::{self, plugin_help, plugin_priority, plugin_sees};
use crate::event::Event;
use crate::plugin::{BotContext, Flow, Interest, Plugin, PluginMeta};
use crate::scope::PluginScope;

#[derive(Debug, Clone, serde::Serialize)]
pub struct PluginSnapshot {
    pub name: String,
    pub version: String,
    pub description: String,
    pub commands: Vec<String>,
}

impl PluginSnapshot {
    pub fn from_plugin(p: &dyn Plugin) -> Self {
        let PluginMeta {
            name,
            version,
            description,
            commands,
        } = p.meta();
        Self {
            name: name.into(),
            version: version.into(),
            description: description.into(),
            commands: commands.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

struct PluginSlot {
    id: String,
    plugin: Arc<dyn Plugin>,
    scope: Arc<PluginScope>,
}

#[derive(Clone)]
struct PendingPlugin {
    id: String,
    plugin: Arc<dyn Plugin>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InstanceSnapshot {
    pub id: String,
    pub plugin: String,
    pub version: String,
    pub description: String,
    pub commands: Vec<String>,
}

pub struct Dispatcher {
    plugins: RwLock<Vec<PluginSlot>>,
    pending: RwLock<Vec<PendingPlugin>>,
    errors: RwLock<HashMap<String, String>>,
}

impl Dispatcher {
    pub fn new(plugins: Vec<Arc<dyn Plugin>>) -> Self {
        Self::with_instances(
            plugins
                .into_iter()
                .map(|p| (p.name().to_string(), p))
                .collect(),
        )
    }

    pub fn with_instances(plugins: Vec<(String, Arc<dyn Plugin>)>) -> Self {
        Self {
            plugins: RwLock::new(Vec::new()),
            pending: RwLock::new(
                plugins
                    .into_iter()
                    .map(|(id, plugin)| PendingPlugin { id, plugin })
                    .collect(),
            ),
            errors: RwLock::new(HashMap::new()),
        }
    }

    pub async fn last_errors(&self) -> HashMap<String, String> {
        self.errors.read().await.clone()
    }

    async fn set_error(&self, name: &str, err: Option<String>) {
        let mut g = self.errors.write().await;
        if let Some(e) = err {
            g.insert(name.to_string(), e);
        } else {
            g.remove(name);
        }
    }

    pub async fn names(&self) -> Vec<String> {
        self.plugins
            .read()
            .await
            .iter()
            .map(|s| s.id.clone())
            .collect()
    }

    pub async fn pending_names(&self) -> Vec<String> {
        self.pending
            .read()
            .await
            .iter()
            .map(|p| p.id.clone())
            .collect()
    }

    pub async fn instance_list(&self) -> Vec<InstanceSnapshot> {
        self.plugins
            .read()
            .await
            .iter()
            .map(|s| {
                let snap = PluginSnapshot::from_plugin(s.plugin.as_ref());
                InstanceSnapshot {
                    id: s.id.clone(),
                    plugin: s.plugin.name().to_string(),
                    version: snap.version,
                    description: snap.description,
                    commands: snap.commands,
                }
            })
            .collect()
    }

    pub async fn info_list(&self) -> Vec<PluginSnapshot> {
        self.plugins
            .read()
            .await
            .iter()
            .map(|s| PluginSnapshot::from_plugin(s.plugin.as_ref()))
            .collect()
    }

    pub async fn help_text(&self, prefix: &str) -> String {
        let mut s = format!("菜单（前缀 `{prefix}`，指令区分大小写）\n\n");
        for slot in self.plugins.read().await.iter() {
            let title = instance_title(&slot.id, slot.plugin.name());
            s.push_str(&format!(
                "【{}】\n{}\n\n",
                title,
                plugin_help(slot.plugin.as_ref(), prefix).trim()
            ));
        }
        s.push_str(&format!(
            "发 `{prefix}help 插件名` 只看一项；各插件入口命令也能看对应说明。"
        ));
        s
    }

    pub async fn help_one(&self, prefix: &str, name: &str) -> Option<String> {
        let guard = self.plugins.read().await;
        let slot = guard.iter().find(|s| s.id == name).or_else(|| {
            let hits: Vec<_> = guard.iter().filter(|s| s.plugin.name() == name).collect();
            if hits.len() == 1 {
                Some(hits[0])
            } else {
                None
            }
        })?;
        Some(format!(
            "【{}】\n{}",
            instance_title(&slot.id, slot.plugin.name()),
            plugin_help(slot.plugin.as_ref(), prefix).trim()
        ))
    }

    /// Start every plugin. A failure is skipped so one bad plugin cannot take the process down.
    /// Plugins whose `inject` is not yet provided stay in `pending`.
    pub async fn start_all(&self, ctx: &BotContext) -> Vec<(String, String)> {
        self.try_start_pending(ctx).await
    }

    pub async fn stop_all(&self, ctx: &BotContext) {
        self.pending.write().await.clear();
        let slots: Vec<_> = self.plugins.write().await.drain(..).collect();
        for slot in slots {
            self.teardown_slot(slot, ctx).await;
        }
    }

    pub async fn insert(&self, plugin: Arc<dyn Plugin>, ctx: &BotContext) -> anyhow::Result<()> {
        self.insert_instance(plugin.name().to_string(), plugin, ctx)
            .await
    }

    pub async fn insert_instance(
        &self,
        id: String,
        plugin: Arc<dyn Plugin>,
        ctx: &BotContext,
    ) -> anyhow::Result<()> {
        if self.plugins.read().await.iter().any(|s| s.id == id) {
            return Ok(());
        }
        if self.pending.read().await.iter().any(|p| p.id == id) {
            let failed = self.try_start_pending(ctx).await;
            if let Some((_, e)) = failed.into_iter().find(|(n, _)| n == &id) {
                anyhow::bail!(e);
            }
            return Ok(());
        }
        info!(instance = %id, plugin = plugin.name(), "hot-enable plugin");
        self.pending.write().await.push(PendingPlugin {
            id: id.clone(),
            plugin,
        });
        let failed = self.try_start_pending(ctx).await;
        if let Some((_, e)) = failed.into_iter().find(|(n, _)| n == &id) {
            anyhow::bail!(e);
        }
        Ok(())
    }

    pub async fn remove(&self, name: &str, ctx: &BotContext) -> anyhow::Result<bool> {
        {
            let mut pending = self.pending.write().await;
            if let Some(i) = pending.iter().position(|p| p.id == name) {
                pending.remove(i);
                self.set_error(name, None).await;
                return Ok(true);
            }
        }
        let Some(slot) = self.take_running(name).await else {
            return Ok(false);
        };
        info!(
            instance = name,
            plugin = slot.plugin.name(),
            "hot-disable plugin"
        );
        self.teardown_slot(slot, ctx).await;
        self.reconcile_inject(ctx).await;
        Ok(true)
    }

    pub async fn replace(&self, plugin: Arc<dyn Plugin>, ctx: &BotContext) -> anyhow::Result<()> {
        self.replace_instance(plugin.name().to_string(), plugin, ctx)
            .await
    }

    /// Start `plugin` first; only then stop the previous instance with the same id.
    /// If `on_start` fails the old plugin (if any) keeps running.
    pub async fn replace_instance(
        &self,
        id: String,
        plugin: Arc<dyn Plugin>,
        ctx: &BotContext,
    ) -> anyhow::Result<()> {
        let kind = plugin.name().to_string();
        let cfg = slot_config(ctx, &id, &kind).await;
        let missing = missing_inject(plugin.as_ref(), &cfg, ctx);
        if !missing.is_empty() {
            anyhow::bail!("waiting for services: {}", missing.join(", "));
        }
        let scope = Arc::new(PluginScope::for_instance(&id, &kind));
        if let Err(e) = plugin.on_start(ctx, &scope).await {
            let msg = format!("{e:#}");
            self.set_error(&id, Some(msg)).await;
            scope.dispose().await;
            return Err(e);
        }
        let prefix = ctx.bot_config().await.command_prefix;
        self.pending.write().await.retain(|p| p.id != id);
        let old = {
            let mut guard = self.plugins.write().await;
            warn_collisions(
                guard
                    .iter()
                    .filter(|s| s.id != id)
                    .map(|s| s.plugin.as_ref()),
                plugin.as_ref(),
                &prefix,
            );
            let old = guard
                .iter()
                .position(|s| s.id == id)
                .map(|i| guard.remove(i));
            guard.push(PluginSlot {
                id: id.clone(),
                plugin,
                scope,
            });
            old
        };
        if let Some(old) = old {
            info!(instance = %id, plugin = old.plugin.name(), "replaced plugin");
            old.scope.dispose().await;
            if let Err(e) = old.plugin.on_stop().await {
                error!(instance = %id, error = %e, "plugin stop");
            }
        } else {
            info!(instance = %id, "hot-enable plugin");
            ctx.emit(
                "plugin.started",
                serde_json::json!({ "name": id, "plugin": kind }),
            );
        }
        self.set_error(&id, None).await;
        self.try_start_pending(ctx).await;
        Ok(())
    }

    async fn take_running(&self, name: &str) -> Option<PluginSlot> {
        let mut guard = self.plugins.write().await;
        guard
            .iter()
            .position(|s| s.id == name)
            .map(|i| guard.remove(i))
    }

    async fn teardown_slot(&self, slot: PluginSlot, ctx: &BotContext) {
        let id = slot.id.clone();
        let kind = slot.plugin.name().to_string();
        slot.scope.dispose().await;
        if let Err(e) = slot.plugin.on_stop().await {
            error!(instance = %id, error = %e, "plugin stop");
        }
        ctx.emit(
            "plugin.stopped",
            serde_json::json!({ "name": id, "plugin": kind }),
        );
    }

    /// Stop running plugins whose inject is no longer satisfied; start pending ones that are.
    pub async fn reconcile_inject(&self, ctx: &BotContext) -> Vec<(String, String)> {
        let running: Vec<(String, String, Arc<dyn Plugin>)> = self
            .plugins
            .read()
            .await
            .iter()
            .map(|s| (s.id.clone(), s.plugin.name().to_string(), s.plugin.clone()))
            .collect();
        for (id, kind, plugin) in running {
            let cfg = slot_config(ctx, &id, &kind).await;
            let missing = missing_inject(plugin.as_ref(), &cfg, ctx);
            if missing.is_empty() {
                continue;
            }
            if let Some(slot) = self.take_running(&id).await {
                info!(instance = %id, missing = ?missing, "demote, inject lost");
                self.teardown_slot(slot, ctx).await;
                self.pending.write().await.push(PendingPlugin {
                    id: id.clone(),
                    plugin,
                });
                self.set_error(
                    &id,
                    Some(format!("waiting for services: {}", missing.join(", "))),
                )
                .await;
            }
        }
        self.try_start_pending(ctx).await
    }

    async fn try_start_pending(&self, ctx: &BotContext) -> Vec<(String, String)> {
        let mut failed = Vec::new();
        loop {
            let pending = self.pending.read().await.clone();
            if pending.is_empty() {
                break;
            }
            let mut cfgs: HashMap<String, serde_json::Value> = HashMap::new();
            for p in &pending {
                cfgs.insert(p.id.clone(), slot_config(ctx, &p.id, p.plugin.name()).await);
            }
            let cyclic = cycle_plugins(&pending, &cfgs);
            if !cyclic.is_empty() {
                let mut leftover = self.pending.write().await;
                leftover.retain(|p| {
                    if cyclic.iter().any(|n| n == &p.id) {
                        let msg = format!("inject cycle: {}", cyclic.join(" ↔ "));
                        failed.push((p.id.clone(), msg.clone()));
                        false
                    } else {
                        true
                    }
                });
                drop(leftover);
                for (n, msg) in failed.iter().cloned() {
                    error!(instance = %n, error = %msg, "plugin start");
                    self.set_error(&n, Some(msg)).await;
                }
                continue;
            }

            let mut ready = Vec::new();
            for p in &pending {
                let cfg = cfgs.get(&p.id).cloned().unwrap_or_default();
                if missing_inject(p.plugin.as_ref(), &cfg, ctx).is_empty() {
                    ready.push(p.clone());
                }
            }
            if ready.is_empty() {
                for p in &pending {
                    let cfg = cfgs.get(&p.id).cloned().unwrap_or_default();
                    let miss = missing_inject(p.plugin.as_ref(), &cfg, ctx);
                    if !miss.is_empty() {
                        let msg = format!("waiting for services: {}", miss.join(", "));
                        info!(instance = %p.id, missing = ?miss, "plugin pending inject");
                        self.set_error(&p.id, Some(msg)).await;
                    }
                }
                break;
            }

            {
                let mut g = self.pending.write().await;
                g.retain(|p| !ready.iter().any(|r| r.id == p.id));
            }

            for item in ready {
                match self.activate(item, ctx).await {
                    Ok(()) => {}
                    Err((name, msg)) => failed.push((name, msg)),
                }
            }
        }
        failed
    }

    async fn activate(
        &self,
        item: PendingPlugin,
        ctx: &BotContext,
    ) -> Result<(), (String, String)> {
        let id = item.id;
        let plugin = item.plugin;
        let kind = plugin.name().to_string();
        if self.plugins.read().await.iter().any(|s| s.id == id) {
            return Ok(());
        }
        info!(instance = %id, plugin = %kind, "starting plugin");
        let scope = Arc::new(PluginScope::for_instance(&id, &kind));
        if let Err(e) = plugin.on_start(ctx, &scope).await {
            let msg = format!("{e:#}");
            error!(instance = %id, error = %msg, "plugin start");
            scope.dispose().await;
            self.set_error(&id, Some(msg.clone())).await;
            return Err((id, msg));
        }
        let prefix = ctx.bot_config().await.command_prefix;
        let mut guard = self.plugins.write().await;
        if guard.iter().any(|s| s.id == id) {
            drop(guard);
            scope.dispose().await;
            let _ = plugin.on_stop().await;
            return Ok(());
        }
        warn_collisions(
            guard.iter().map(|s| s.plugin.as_ref()),
            plugin.as_ref(),
            &prefix,
        );
        guard.push(PluginSlot {
            id: id.clone(),
            plugin: plugin.clone(),
            scope,
        });
        drop(guard);
        self.set_error(&id, None).await;
        ctx.emit(
            "plugin.started",
            serde_json::json!({ "name": id, "plugin": kind }),
        );
        Ok(())
    }

    pub async fn handle(&self, ctx: &BotContext, ev: &Event) {
        let mut prefix = String::new();
        if let Some(msg) = ev.as_message() {
            let cfg = ctx.bot_config().await;
            if !cfg.allowed(msg.source) {
                return;
            }
            if let Err(e) = persist_message(ctx, msg).await {
                error!(error = %e, message_id = msg.id, "persist message");
            }
            prefix = cfg.command_prefix;
            let line = msg.command_line();
            let help = format!("{prefix}help");
            if line == help {
                if let Err(e) = ctx.reply_text(msg, self.help_text(&prefix).await).await {
                    error!(error = %e, "send help");
                }
                return;
            }
            if let Some(rest) = line.strip_prefix(&format!("{help} ")) {
                let name = rest.trim();
                let body = if name.is_empty() {
                    self.help_text(&prefix).await
                } else {
                    self.help_one(&prefix, name).await.unwrap_or_else(|| {
                        format!("没有叫「{name}」的运行中插件。发 `{prefix}help` 看全部。")
                    })
                };
                if let Err(e) = ctx.reply_text(msg, body).await {
                    error!(error = %e, "send help");
                }
                return;
            }
        }

        let slots: Vec<_> = self
            .plugins
            .read()
            .await
            .iter()
            .map(|s| (s.id.clone(), s.plugin.clone(), s.scope.clone()))
            .collect();

        let mut prepared: Vec<(i64, usize, Arc<dyn Plugin>, Arc<PluginScope>)> = Vec::new();
        for (idx, (id, p, scope)) in slots.into_iter().enumerate() {
            let cfg = slot_config(ctx, &id, p.name()).await;
            if !plugin_sees(&cfg, ev) {
                continue;
            }
            prepared.push((plugin_priority(&cfg), idx, p, scope));
        }
        prepared.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

        let listed: Vec<&dyn Plugin> = prepared.iter().map(|(_, _, p, _)| p.as_ref()).collect();
        let targets = route_targets(listed, ev, &prefix);
        for i in targets {
            let (_, _, p, scope) = &prepared[i];
            match p.handle(ctx, ev, scope).await {
                Ok(Flow::Stop) => return,
                Ok(Flow::Continue) => {}
                Err(e) => {
                    let msg = format!("{e:#}");
                    error!(plugin = p.name(), error = %msg, "plugin error");
                    self.set_error(p.name(), Some(msg)).await;
                    if let Some(m) = ev.as_message() {
                        if let Err(send_e) =
                            ctx.reply_text(m, format!("插件 {} 出错了", p.name())).await
                        {
                            error!(error = %send_e, "send plugin error");
                        }
                    }
                }
            }
        }
    }
}

fn instance_title(id: &str, kind: &str) -> String {
    if id == kind {
        id.to_string()
    } else {
        format!("{id} · {kind}")
    }
}

async fn slot_config(ctx: &BotContext, id: &str, kind: &str) -> serde_json::Value {
    let kind_cfg = ctx.plugin_config(kind).await;
    if id == kind {
        return kind_cfg;
    }
    let over = ctx.plugin_config(id).await;
    match (kind_cfg, over) {
        (serde_json::Value::Object(mut a), serde_json::Value::Object(b)) => {
            for (k, v) in b {
                a.insert(k, v);
            }
            serde_json::Value::Object(a)
        }
        (_, over) if over != serde_json::Value::Null => over,
        (base, _) => base,
    }
}

fn advertised_provides(id: &str, plugin: &dyn Plugin, cfg: &serde_json::Value) -> Vec<String> {
    let raw = provides_list(plugin, cfg);
    if id == plugin.name() {
        raw
    } else {
        raw.into_iter().map(|s| format!("{s}#{id}")).collect()
    }
}

fn string_list(declared: &[&str], extra: Option<&serde_json::Value>) -> Vec<String> {
    let mut v: Vec<String> = declared.iter().map(|s| (*s).to_string()).collect();
    if let Some(arr) = extra.and_then(|x| x.as_array()) {
        for x in arr {
            if let Some(s) = x.as_str() {
                if !s.is_empty() {
                    v.push(s.to_string());
                }
            }
        }
    }
    v.sort();
    v.dedup();
    v
}

fn inject_list(plugin: &dyn Plugin, cfg: &serde_json::Value) -> Vec<String> {
    string_list(plugin.inject(), cfg.get("inject"))
}

fn provides_list(plugin: &dyn Plugin, cfg: &serde_json::Value) -> Vec<String> {
    string_list(plugin.provides(), cfg.get("provides"))
}

fn missing_inject(plugin: &dyn Plugin, cfg: &serde_json::Value, ctx: &BotContext) -> Vec<String> {
    inject_list(plugin, cfg)
        .into_iter()
        .filter(|s| !ctx.services.contains(s))
        .collect()
}

/// Plugin names that sit on a cycle in the pending inject/provides graph.
fn cycle_plugins(
    pending: &[PendingPlugin],
    cfgs: &HashMap<String, serde_json::Value>,
) -> Vec<String> {
    let mut provided_by: HashMap<String, String> = HashMap::new();
    for p in pending {
        let cfg = cfgs.get(&p.id).cloned().unwrap_or_default();
        for s in advertised_provides(&p.id, p.plugin.as_ref(), &cfg) {
            provided_by.insert(s, p.id.clone());
        }
    }
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    let mut indeg: HashMap<String, usize> = HashMap::new();
    for p in pending {
        indeg.entry(p.id.clone()).or_insert(0);
        adj.entry(p.id.clone()).or_default();
        let cfg = cfgs.get(&p.id).cloned().unwrap_or_default();
        for s in inject_list(p.plugin.as_ref(), &cfg) {
            let Some(prov) = provided_by.get(&s) else {
                continue;
            };
            if prov == &p.id {
                continue;
            }
            adj.entry(prov.clone()).or_default().push(p.id.clone());
            *indeg.entry(p.id.clone()).or_insert(0) += 1;
        }
    }
    let mut q: Vec<String> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| n.clone())
        .collect();
    let mut seen = 0usize;
    while let Some(n) = q.pop() {
        seen += 1;
        for nxt in adj.get(&n).cloned().unwrap_or_default() {
            if let Some(e) = indeg.get_mut(&nxt) {
                *e = e.saturating_sub(1);
                if *e == 0 {
                    q.push(nxt);
                }
            }
        }
    }
    if seen == indeg.len() {
        return Vec::new();
    }
    let mut cyclic: Vec<String> = indeg
        .into_iter()
        .filter(|(_, d)| *d > 0)
        .map(|(n, _)| n)
        .collect();
    cyclic.sort();
    cyclic
}

fn warn_collisions<'a>(
    existing: impl IntoIterator<Item = &'a dyn Plugin>,
    newp: &dyn Plugin,
    prefix: &str,
) {
    let new_cmds: Vec<String> = newp
        .commands()
        .iter()
        .map(|c| command::expand(c, prefix))
        .collect();
    for p in existing {
        for c in p.commands() {
            let e = command::expand(c, prefix);
            if new_cmds.iter().any(|n| n == &e) {
                warn!(
                    plugin = newp.name(),
                    other = p.name(),
                    cmd = %e,
                    "command collision; higher priority wins"
                );
            }
        }
    }
}

/// Exact first-token match, else longest declared prefix. Plugin order breaks ties
/// (caller should already have sorted by priority).
pub fn find_command_owner<'a, P: Plugin + ?Sized + 'a>(
    plugins: impl IntoIterator<Item = &'a P>,
    line: &str,
    bot_prefix: &str,
) -> Option<&'a str> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let token = command::first_token(line);
    let mut prefix_best: Option<(&'a str, usize)> = None;
    for p in plugins {
        for cmd in p.commands() {
            if token == command::expand(cmd, bot_prefix) {
                return Some(p.name());
            }
        }
        for pre in p.command_prefixes() {
            let c = command::expand(pre, bot_prefix);
            if !c.is_empty() && line.starts_with(&c) {
                let keep = prefix_best.map(|(_, n)| c.len() > n).unwrap_or(true);
                if keep {
                    prefix_best = Some((p.name(), c.len()));
                }
            }
        }
    }
    prefix_best.map(|(name, _)| name)
}

fn route_targets<'a, I>(plugins: I, ev: &Event, bot_prefix: &str) -> Vec<usize>
where
    I: IntoIterator<Item = &'a dyn Plugin>,
{
    let listed: Vec<&dyn Plugin> = plugins.into_iter().collect();
    if let Some(msg) = ev.as_message() {
        let line = msg.command_line();
        let owner = find_command_owner(listed.iter().copied(), &line, bot_prefix);
        let mut out = Vec::new();
        if let Some(name) = owner {
            if let Some(i) = listed.iter().position(|p| p.name() == name) {
                out.push(i);
            }
        }
        for (i, p) in listed.iter().enumerate() {
            if owner == Some(p.name()) {
                continue;
            }
            match p.interest() {
                Interest::Commands => {}
                Interest::Messages | Interest::All => out.push(i),
            }
        }
        out
    } else {
        listed
            .iter()
            .enumerate()
            .filter(|(_, p)| p.interest() == Interest::All)
            .map(|(i, _)| i)
            .collect()
    }
}

async fn persist_message(ctx: &BotContext, msg: &crate::event::MessageEvent) -> anyhow::Result<()> {
    let segments_json = serde_json::to_string(&msg.segments).unwrap_or_else(|_| "[]".into());
    ctx.store
        .insert_message(&fujiang_store::MessageLog {
            message_id: msg.id,
            time: msg.time,
            self_id: msg.self_id,
            user_id: msg.user_id(),
            group_id: msg.source.group_id(),
            nickname: msg.sender.nickname.clone(),
            card: msg.sender.card.clone(),
            raw_text: msg.raw_text.clone(),
            segments_json,
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::PluginScope;
    use async_trait::async_trait;

    struct Stub {
        name: &'static str,
        cmds: &'static [&'static str],
        prefixes: &'static [&'static str],
        interest: Interest,
    }

    #[async_trait]
    impl Plugin for Stub {
        fn name(&self) -> &'static str {
            self.name
        }
        fn help(&self) -> &'static str {
            ""
        }
        fn commands(&self) -> &'static [&'static str] {
            self.cmds
        }
        fn command_prefixes(&self) -> &'static [&'static str] {
            self.prefixes
        }
        fn interest(&self) -> Interest {
            self.interest
        }
        async fn handle(
            &self,
            _ctx: &BotContext,
            _ev: &Event,
            _scope: &PluginScope,
        ) -> anyhow::Result<Flow> {
            Ok(Flow::Continue)
        }
    }

    fn stubs() -> Vec<Stub> {
        vec![
            Stub {
                name: "contest",
                cmds: &[".cf", ".contest", ".remind"],
                prefixes: &[".remind"],
                interest: Interest::Commands,
            },
            Stub {
                name: "echo",
                cmds: &[".ping"],
                prefixes: &[],
                interest: Interest::Commands,
            },
            Stub {
                name: "fun",
                cmds: &[".learn", ".star", ".tag"],
                prefixes: &[".添加", ".删除"],
                interest: Interest::Messages,
            },
        ]
    }

    #[test]
    fn exact_token_picks_owner() {
        let p = stubs();
        assert_eq!(find_command_owner(&p, ".cf", "."), Some("contest"));
        assert_eq!(find_command_owner(&p, ".ping extra", "."), Some("echo"));
        assert_eq!(find_command_owner(&p, ".learn add a b", "."), Some("fun"));
    }

    #[test]
    fn prefix_matches_remind_and_image_cmds() {
        let p = stubs();
        assert_eq!(find_command_owner(&p, ".remind08:30", "."), Some("contest"));
        assert_eq!(find_command_owner(&p, ".remindoff", "."), Some("contest"));
        assert_eq!(find_command_owner(&p, ".添加猫", "."), Some("fun"));
        assert_eq!(find_command_owner(&p, "来只猫", "."), None);
        assert_eq!(find_command_owner(&p, "活着？", "."), None);
    }

    #[test]
    fn respects_custom_bot_prefix() {
        let p = stubs();
        assert_eq!(find_command_owner(&p, "!ping", "!"), Some("echo"));
        assert_eq!(find_command_owner(&p, ".ping", "!"), None);
    }

    #[test]
    fn message_route_skips_command_only() {
        use crate::event::{MessageEvent, Sender, Source};
        let p = stubs();
        let listed: Vec<&dyn Plugin> = p.iter().map(|s| s as &dyn Plugin).collect();
        let ev = Event::Message(MessageEvent {
            id: 1,
            time: 0,
            self_id: 1,
            sender: Sender {
                user_id: 2,
                nickname: None,
                card: None,
            },
            source: Source::Group { id: 3 },
            segments: vec![],
            raw_text: "来只猫".into(),
        });
        let idx = route_targets(listed, &ev, ".");
        let names: Vec<_> = idx.into_iter().map(|i| p[i].name).collect();
        assert_eq!(names, vec!["fun"]);
    }

    struct NoopMessenger;
    #[async_trait]
    impl crate::plugin::Messenger for NoopMessenger {
        async fn send(
            &self,
            _target: crate::event::Source,
            _segs: &[crate::event::Segment],
        ) -> anyhow::Result<i64> {
            Ok(0)
        }
        async fn get_message(&self, _id: i64) -> anyhow::Result<crate::event::MessageEvent> {
            anyhow::bail!("no")
        }
        async fn get_image(&self, _file: &str) -> anyhow::Result<std::path::PathBuf> {
            anyhow::bail!("no")
        }
        async fn get_file(&self, _file_id: &str) -> anyhow::Result<std::path::PathBuf> {
            anyhow::bail!("no")
        }
    }

    async fn test_ctx() -> BotContext {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "fujiang-core-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = fujiang_store::Store::open(dir.join("t.db").to_str().unwrap(), dir.join("img"))
            .await
            .unwrap();
        BotContext {
            messenger: Arc::new(tokio::sync::RwLock::new(
                Arc::new(NoopMessenger) as Arc<dyn crate::plugin::Messenger>
            )),
            store,
            http: reqwest::Client::new(),
            config: Arc::new(tokio::sync::RwLock::new(crate::BotConfig {
                command_prefix: ".".into(),
                groups: vec![],
                allow_private: true,
                clist_username: String::new(),
                clist_api_key: String::new(),
                clist_limit: 10,
                contest_update_minutes: 60,
                rank_update_minutes: 60,
                daily_reset_hour: 4,
                fun_allow_mutate: true,
                fun_admins: vec![],
                fun_delete_files: false,
            })),
            plugin_configs: Arc::new(tokio::sync::RwLock::new(Default::default())),
            services: crate::ServiceHub::new(),
            events: crate::EventBus::new(),
        }
    }

    struct Named {
        name: &'static str,
        fail_start: bool,
        started: std::sync::atomic::AtomicU32,
    }

    #[async_trait]
    impl Plugin for Named {
        fn name(&self) -> &'static str {
            self.name
        }
        fn help(&self) -> &'static str {
            ""
        }
        async fn on_start(&self, _ctx: &BotContext, _scope: &PluginScope) -> anyhow::Result<()> {
            if self.fail_start {
                anyhow::bail!("boom");
            }
            self.started
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        async fn handle(
            &self,
            _ctx: &BotContext,
            _ev: &Event,
            _scope: &PluginScope,
        ) -> anyhow::Result<Flow> {
            Ok(Flow::Continue)
        }
    }

    #[tokio::test]
    async fn replace_rolls_back_when_start_fails() {
        let ctx = test_ctx().await;
        let old = Arc::new(Named {
            name: "x",
            fail_start: false,
            started: std::sync::atomic::AtomicU32::new(0),
        });
        let d = Dispatcher::new(vec![old.clone()]);
        assert!(d.start_all(&ctx).await.is_empty());
        assert_eq!(old.started.load(std::sync::atomic::Ordering::SeqCst), 1);

        let newp = Arc::new(Named {
            name: "x",
            fail_start: true,
            started: std::sync::atomic::AtomicU32::new(0),
        });
        assert!(d.replace(newp, &ctx).await.is_err());
        assert_eq!(d.names().await, vec!["x".to_string()]);
        assert_eq!(old.started.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn replace_swaps_when_start_ok() {
        let ctx = test_ctx().await;
        let old = Arc::new(Named {
            name: "x",
            fail_start: false,
            started: std::sync::atomic::AtomicU32::new(0),
        });
        let d = Dispatcher::new(vec![old.clone()]);
        assert!(d.start_all(&ctx).await.is_empty());
        let newp = Arc::new(Named {
            name: "x",
            fail_start: false,
            started: std::sync::atomic::AtomicU32::new(0),
        });
        d.replace(newp.clone(), &ctx).await.unwrap();
        assert_eq!(newp.started.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(d.names().await, vec!["x".to_string()]);
    }

    #[tokio::test]
    async fn start_all_skips_failed_plugin() {
        let ctx = test_ctx().await;
        let bad = Arc::new(Named {
            name: "bad",
            fail_start: true,
            started: std::sync::atomic::AtomicU32::new(0),
        });
        let good = Arc::new(Named {
            name: "good",
            fail_start: false,
            started: std::sync::atomic::AtomicU32::new(0),
        });
        let d = Dispatcher::new(vec![bad.clone(), good.clone()]);
        let failed = d.start_all(&ctx).await;
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, "bad");
        assert_eq!(d.names().await, vec!["good".to_string()]);
        assert_eq!(good.started.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(d.last_errors().await.contains_key("bad"));
    }

    #[tokio::test]
    async fn plugin_groups_skip_handle() {
        use crate::event::{MessageEvent, Sender, Source};
        use std::sync::atomic::{AtomicU32, Ordering};

        struct Count {
            n: AtomicU32,
        }
        #[async_trait]
        impl Plugin for Count {
            fn name(&self) -> &'static str {
                "count"
            }
            fn help(&self) -> &'static str {
                ""
            }
            fn commands(&self) -> &'static [&'static str] {
                &[".ping"]
            }
            async fn handle(
                &self,
                _ctx: &BotContext,
                _ev: &Event,
                _scope: &PluginScope,
            ) -> anyhow::Result<Flow> {
                self.n.fetch_add(1, Ordering::SeqCst);
                Ok(Flow::Stop)
            }
        }

        let ctx = test_ctx().await;
        ctx.plugin_configs
            .write()
            .await
            .insert("count".into(), serde_json::json!({ "groups": [99] }));
        let p = Arc::new(Count {
            n: AtomicU32::new(0),
        });
        let d = Dispatcher::new(vec![p.clone()]);
        assert!(d.start_all(&ctx).await.is_empty());
        let ev = Event::Message(MessageEvent {
            id: 1,
            time: 0,
            self_id: 1,
            sender: Sender {
                user_id: 2,
                nickname: None,
                card: None,
            },
            source: Source::Group { id: 3 },
            segments: vec![],
            raw_text: ".ping".into(),
        });
        d.handle(&ctx, &ev).await;
        assert_eq!(p.n.load(Ordering::SeqCst), 0);

        let ev2 = Event::Message(MessageEvent {
            id: 2,
            time: 0,
            self_id: 1,
            sender: Sender {
                user_id: 2,
                nickname: None,
                card: None,
            },
            source: Source::Group { id: 99 },
            segments: vec![],
            raw_text: ".ping".into(),
        });
        d.handle(&ctx, &ev2).await;
        assert_eq!(p.n.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn help_one_names_plugin() {
        let ctx = test_ctx().await;
        struct H;
        #[async_trait]
        impl Plugin for H {
            fn name(&self) -> &'static str {
                "echo"
            }
            fn help(&self) -> &'static str {
                ".ping → pong"
            }
            fn commands(&self) -> &'static [&'static str] {
                &[".ping"]
            }
            async fn handle(
                &self,
                _ctx: &BotContext,
                _ev: &Event,
                _scope: &PluginScope,
            ) -> anyhow::Result<Flow> {
                Ok(Flow::Continue)
            }
        }
        let d = Dispatcher::new(vec![Arc::new(H)]);
        assert!(d.start_all(&ctx).await.is_empty());
        let one = d.help_one("!", "echo").await.unwrap();
        assert!(one.contains("!ping"));
        assert!(d.help_one("!", "nope").await.is_none());
    }

    struct ConstSvc(serde_json::Value);
    #[async_trait]
    impl crate::Service for ConstSvc {
        async fn call(
            &self,
            method: &str,
            _args: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            anyhow::ensure!(method == "get", "no");
            Ok(self.0.clone())
        }
    }

    struct Provider;
    #[async_trait]
    impl Plugin for Provider {
        fn name(&self) -> &'static str {
            "prov"
        }
        fn help(&self) -> &'static str {
            ""
        }
        fn provides(&self) -> &'static [&'static str] {
            &["dep"]
        }
        async fn on_start(&self, ctx: &BotContext, scope: &PluginScope) -> anyhow::Result<()> {
            ctx.provide(scope, "dep", Arc::new(ConstSvc(serde_json::json!(1))))?;
            Ok(())
        }
        async fn handle(
            &self,
            _ctx: &BotContext,
            _ev: &Event,
            _scope: &PluginScope,
        ) -> anyhow::Result<Flow> {
            Ok(Flow::Continue)
        }
    }

    struct Waiter {
        started: std::sync::atomic::AtomicU32,
    }
    #[async_trait]
    impl Plugin for Waiter {
        fn name(&self) -> &'static str {
            "wait"
        }
        fn help(&self) -> &'static str {
            ""
        }
        fn inject(&self) -> &'static [&'static str] {
            &["dep"]
        }
        async fn on_start(&self, _ctx: &BotContext, _scope: &PluginScope) -> anyhow::Result<()> {
            self.started
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        async fn handle(
            &self,
            _ctx: &BotContext,
            _ev: &Event,
            _scope: &PluginScope,
        ) -> anyhow::Result<Flow> {
            Ok(Flow::Continue)
        }
    }

    #[tokio::test]
    async fn inject_waits_then_starts() {
        let ctx = test_ctx().await;
        let wait = Arc::new(Waiter {
            started: std::sync::atomic::AtomicU32::new(0),
        });
        let d = Dispatcher::new(vec![wait.clone()]);
        assert!(d.start_all(&ctx).await.is_empty());
        assert!(d.names().await.is_empty());
        assert_eq!(d.pending_names().await, vec!["wait".to_string()]);
        assert_eq!(wait.started.load(std::sync::atomic::Ordering::SeqCst), 0);

        d.insert(Arc::new(Provider), &ctx).await.unwrap();
        assert_eq!(wait.started.load(std::sync::atomic::Ordering::SeqCst), 1);
        let mut names = d.names().await;
        names.sort();
        assert_eq!(names, vec!["prov".to_string(), "wait".to_string()]);
        assert_eq!(
            ctx.call("dep", "get", serde_json::json!({})).await.unwrap(),
            serde_json::json!(1)
        );

        d.remove("prov", &ctx).await.unwrap();
        assert!(!d.names().await.iter().any(|n| n == "prov"));
        assert!(!d.names().await.iter().any(|n| n == "wait"));
        assert!(d.pending_names().await.contains(&"wait".to_string()));
        assert!(!ctx.services.contains("dep"));
    }

    struct CycA;
    struct CycB;
    #[async_trait]
    impl Plugin for CycA {
        fn name(&self) -> &'static str {
            "a"
        }
        fn help(&self) -> &'static str {
            ""
        }
        fn inject(&self) -> &'static [&'static str] {
            &["b"]
        }
        fn provides(&self) -> &'static [&'static str] {
            &["a"]
        }
        async fn handle(
            &self,
            _ctx: &BotContext,
            _ev: &Event,
            _scope: &PluginScope,
        ) -> anyhow::Result<Flow> {
            Ok(Flow::Continue)
        }
    }
    #[async_trait]
    impl Plugin for CycB {
        fn name(&self) -> &'static str {
            "b"
        }
        fn help(&self) -> &'static str {
            ""
        }
        fn inject(&self) -> &'static [&'static str] {
            &["a"]
        }
        fn provides(&self) -> &'static [&'static str] {
            &["b"]
        }
        async fn handle(
            &self,
            _ctx: &BotContext,
            _ev: &Event,
            _scope: &PluginScope,
        ) -> anyhow::Result<Flow> {
            Ok(Flow::Continue)
        }
    }

    #[tokio::test]
    async fn inject_cycle_fails() {
        let ctx = test_ctx().await;
        let d = Dispatcher::new(vec![Arc::new(CycA), Arc::new(CycB)]);
        let failed = d.start_all(&ctx).await;
        assert_eq!(failed.len(), 2);
        assert!(d.names().await.is_empty());
        assert!(d.pending_names().await.is_empty());
    }

    #[tokio::test]
    async fn two_instances_same_kind() {
        let ctx = test_ctx().await;
        ctx.plugin_configs
            .write()
            .await
            .insert("echo-b".into(), serde_json::json!({ "groups": [2] }));
        struct Echo;
        #[async_trait]
        impl Plugin for Echo {
            fn name(&self) -> &'static str {
                "echo"
            }
            fn help(&self) -> &'static str {
                ".ping"
            }
            fn commands(&self) -> &'static [&'static str] {
                &[".ping"]
            }
            fn provides(&self) -> &'static [&'static str] {
                &["echo"]
            }
            async fn on_start(&self, ctx: &BotContext, scope: &PluginScope) -> anyhow::Result<()> {
                ctx.provide(
                    scope,
                    "echo",
                    Arc::new(ConstSvc(serde_json::json!(scope.instance_id()))),
                )?;
                Ok(())
            }
            async fn handle(
                &self,
                _ctx: &BotContext,
                _ev: &Event,
                _scope: &PluginScope,
            ) -> anyhow::Result<Flow> {
                Ok(Flow::Continue)
            }
        }
        let d = Dispatcher::with_instances(vec![
            ("echo".into(), Arc::new(Echo) as Arc<dyn Plugin>),
            ("echo-b".into(), Arc::new(Echo)),
        ]);
        assert!(d.start_all(&ctx).await.is_empty());
        let mut names = d.names().await;
        names.sort();
        assert_eq!(names, vec!["echo".to_string(), "echo-b".to_string()]);
        assert_eq!(
            ctx.call("echo", "get", serde_json::json!({}))
                .await
                .unwrap(),
            serde_json::json!("echo")
        );
        assert_eq!(
            ctx.call("echo#echo-b", "get", serde_json::json!({}))
                .await
                .unwrap(),
            serde_json::json!("echo-b")
        );
        let help = d.help_one(".", "echo-b").await.unwrap();
        assert!(help.contains("echo-b"));
    }
}
