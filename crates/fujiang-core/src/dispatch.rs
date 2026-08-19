use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{error, info};

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
    plugin: Arc<dyn Plugin>,
    scope: Arc<PluginScope>,
}

pub struct Dispatcher {
    plugins: RwLock<Vec<PluginSlot>>,
}

impl Dispatcher {
    pub fn new(plugins: Vec<Arc<dyn Plugin>>) -> Self {
        Self {
            plugins: RwLock::new(
                plugins
                    .into_iter()
                    .map(|plugin| PluginSlot {
                        plugin,
                        scope: Arc::new(PluginScope::new()),
                    })
                    .collect(),
            ),
        }
    }

    pub async fn names(&self) -> Vec<String> {
        self.plugins
            .read()
            .await
            .iter()
            .map(|s| s.plugin.name().to_string())
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
            s.push_str(&format!(
                "【{}】\n{}\n\n",
                slot.plugin.name(),
                slot.plugin.help().trim()
            ));
        }
        s.push_str("发各插件自己的入口命令（如 .contest、.ping）也能看对应说明。");
        s
    }

    pub async fn start_all(&self, ctx: &BotContext) -> anyhow::Result<()> {
        for slot in self.plugins.read().await.iter() {
            info!(plugin = slot.plugin.name(), "starting plugin");
            if let Err(e) = slot.plugin.on_start(ctx, &slot.scope).await {
                slot.scope.dispose().await;
                return Err(e);
            }
        }
        Ok(())
    }

    pub async fn stop_all(&self) {
        let slots: Vec<_> = self.plugins.write().await.drain(..).collect();
        for slot in slots {
            if let Err(e) = slot.plugin.on_stop().await {
                error!(plugin = slot.plugin.name(), error = %e, "plugin stop");
            }
            slot.scope.dispose().await;
        }
    }

    pub async fn insert(&self, plugin: Arc<dyn Plugin>, ctx: &BotContext) -> anyhow::Result<()> {
        let name = plugin.name();
        {
            let guard = self.plugins.read().await;
            if guard.iter().any(|s| s.plugin.name() == name) {
                return Ok(());
            }
        }
        info!(plugin = name, "hot-enable plugin");
        let scope = Arc::new(PluginScope::new());
        if let Err(e) = plugin.on_start(ctx, &scope).await {
            scope.dispose().await;
            return Err(e);
        }
        let mut guard = self.plugins.write().await;
        if guard.iter().any(|s| s.plugin.name() == name) {
            drop(guard);
            if let Err(e) = plugin.on_stop().await {
                error!(plugin = name, error = %e, "plugin stop");
            }
            scope.dispose().await;
            return Ok(());
        }
        guard.push(PluginSlot { plugin, scope });
        Ok(())
    }

    pub async fn remove(&self, name: &str) -> anyhow::Result<bool> {
        let mut guard = self.plugins.write().await;
        if let Some(i) = guard.iter().position(|s| s.plugin.name() == name) {
            let slot = guard.remove(i);
            drop(guard);
            info!(plugin = name, "hot-disable plugin");
            if let Err(e) = slot.plugin.on_stop().await {
                error!(plugin = name, error = %e, "plugin stop");
            }
            slot.scope.dispose().await;
            return Ok(true);
        }
        Ok(false)
    }

    /// Start `plugin` first; only then stop the previous instance with the same name.
    /// If `on_start` fails the old plugin (if any) keeps running.
    pub async fn replace(&self, plugin: Arc<dyn Plugin>, ctx: &BotContext) -> anyhow::Result<()> {
        let name = plugin.name();
        let scope = Arc::new(PluginScope::new());
        if let Err(e) = plugin.on_start(ctx, &scope).await {
            scope.dispose().await;
            return Err(e);
        }
        let old = {
            let mut guard = self.plugins.write().await;
            let old = guard
                .iter()
                .position(|s| s.plugin.name() == name)
                .map(|i| guard.remove(i));
            guard.push(PluginSlot { plugin, scope });
            old
        };
        if let Some(old) = old {
            info!(plugin = name, "replaced plugin");
            if let Err(e) = old.plugin.on_stop().await {
                error!(plugin = name, error = %e, "plugin stop");
            }
            old.scope.dispose().await;
        } else {
            info!(plugin = name, "hot-enable plugin");
        }
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
            let line = msg.command_line();
            if line == format!("{}help", cfg.command_prefix) {
                if let Err(e) = ctx
                    .reply_text(msg, self.help_text(&cfg.command_prefix).await)
                    .await
                {
                    error!(error = %e, "send help");
                }
                return;
            }
            prefix = cfg.command_prefix;
        } else if let Event::Request(req) = ev {
            if req.kind == "friend" {
                // ignore
            }
        }

        let slots: Vec<_> = self
            .plugins
            .read()
            .await
            .iter()
            .map(|s| (s.plugin.clone(), s.scope.clone()))
            .collect();
        let targets = route_targets(slots.iter().map(|(p, _)| p.as_ref()), ev, &prefix);
        for i in targets {
            let (p, scope) = &slots[i];
            match p.handle(ctx, ev, scope).await {
                Ok(Flow::Stop) => return,
                Ok(Flow::Continue) => {}
                Err(e) => {
                    error!(plugin = p.name(), error = %e, "plugin error");
                }
            }
        }
    }
}

fn expand_cmd(declared: &str, bot_prefix: &str) -> String {
    if let Some(rest) = declared.strip_prefix('.') {
        format!("{bot_prefix}{rest}")
    } else {
        declared.to_string()
    }
}

fn first_token(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

/// Exact first-token match, else longest declared prefix. Plugin order breaks ties.
pub fn find_command_owner<'a, P: Plugin + ?Sized + 'a>(
    plugins: impl IntoIterator<Item = &'a P>,
    line: &str,
    bot_prefix: &str,
) -> Option<&'a str> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let token = first_token(line);
    let mut prefix_best: Option<(&'a str, usize)> = None;
    for p in plugins {
        for cmd in p.commands() {
            if token == expand_cmd(cmd, bot_prefix) {
                return Some(p.name());
            }
        }
        for pre in p.command_prefixes() {
            let c = expand_cmd(pre, bot_prefix);
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
        let dir = std::env::temp_dir().join(format!(
            "fujiang-core-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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
        d.start_all(&ctx).await.unwrap();
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
        d.start_all(&ctx).await.unwrap();
        let newp = Arc::new(Named {
            name: "x",
            fail_start: false,
            started: std::sync::atomic::AtomicU32::new(0),
        });
        d.replace(newp.clone(), &ctx).await.unwrap();
        assert_eq!(newp.started.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(d.names().await, vec!["x".to_string()]);
    }
}
