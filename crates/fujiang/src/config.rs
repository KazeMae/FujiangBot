use std::path::Path;

use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub adapter: AdapterSection,
    pub napcat: NapcatSection,
    pub bot: BotSection,
    pub store: StoreSection,
    pub clist: ClistSection,
    pub plugins: PluginsSection,
    #[serde(default)]
    pub admin: AdminSection,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AdapterBackend {
    #[default]
    NapcatWs,
    LlonebotWs,
    LlonebotHttp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterSection {
    #[serde(default)]
    pub backend: AdapterBackend,
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub ws_url: String,
    #[serde(default = "default_http_api")]
    pub http_api: String,
    #[serde(default = "default_event_listen")]
    pub event_listen: String,
}

impl Default for AdapterSection {
    fn default() -> Self {
        Self {
            backend: AdapterBackend::NapcatWs,
            access_token: String::new(),
            ws_url: String::new(),
            http_api: default_http_api(),
            event_listen: default_event_listen(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NapcatSection {
    pub ws_url: String,
    #[serde(default)]
    pub access_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotSection {
    #[serde(default = "default_prefix")]
    pub command_prefix: String,
    #[serde(default)]
    pub groups: Vec<i64>,
    #[serde(default = "default_true")]
    pub allow_private: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreSection {
    pub db: String,
    pub image_root: String,
    #[serde(default = "default_archive_dir")]
    pub archive_dir: String,
    /// 早于这么多天的消息搬进 archive_dir；0 表示不归档
    #[serde(default = "default_archive_days")]
    pub archive_after_days: u64,
    #[serde(default = "default_archive_hours")]
    pub archive_every_hours: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClistSection {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminSection {
    /// 管理页监听地址，与 LLOneBot 事件上报端口分开
    #[serde(default = "default_admin_listen")]
    pub listen: String,
    /// 非空时，改配置接口要求请求头 `X-Admin-Token`
    #[serde(default)]
    pub token: String,
}

impl Default for AdminSection {
    fn default() -> Self {
        Self {
            listen: default_admin_listen(),
            token: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginsSection {
    /// 动态 .so / .dylib 目录；放入即可热加载
    #[serde(default = "default_plugin_dir")]
    pub dir: String,
    /// 监视目录，文件增删改会自动加载/卸载
    #[serde(default = "default_true")]
    pub watch: bool,
    #[serde(default)]
    pub contest: PluginToggle,
    #[serde(default)]
    pub rank: PluginToggle,
    #[serde(default)]
    pub problem: PluginToggle,
    #[serde(default)]
    pub fun: FunToggle,
    #[serde(default)]
    pub luck: PluginToggle,
}

impl Default for PluginsSection {
    fn default() -> Self {
        Self {
            dir: default_plugin_dir(),
            watch: true,
            contest: PluginToggle::default(),
            rank: PluginToggle::default(),
            problem: PluginToggle::default(),
            fun: FunToggle::default(),
            luck: PluginToggle::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginToggle {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_60")]
    pub update_minutes: u64,
    #[serde(default = "default_4")]
    pub daily_reset_hour: u32,
}

impl Default for PluginToggle {
    fn default() -> Self {
        Self {
            enabled: true,
            update_minutes: 60,
            daily_reset_hour: 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunToggle {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub allow_mutate: bool,
    #[serde(default)]
    pub admins: Vec<i64>,
    #[serde(default)]
    pub delete_files_on_album_del: bool,
}

impl Default for FunToggle {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_mutate: true,
            admins: vec![],
            delete_files_on_album_del: false,
        }
    }
}

fn default_prefix() -> String {
    ".".into()
}
fn default_true() -> bool {
    true
}
fn default_limit() -> u32 {
    10
}
fn default_60() -> u64 {
    60
}
fn default_4() -> u32 {
    4
}
fn default_archive_dir() -> String {
    "data/archive".into()
}
fn default_archive_days() -> u64 {
    30
}
fn default_archive_hours() -> u64 {
    24
}
fn default_http_api() -> String {
    "http://127.0.0.1:3000".into()
}
fn default_event_listen() -> String {
    "127.0.0.1:5700".into()
}
fn default_admin_listen() -> String {
    "127.0.0.1:8787".into()
}
fn default_plugin_dir() -> String {
    "plugins".into()
}

pub const SECRET_MASK: &str = "********";
pub const SECRET_CLEAR: &str = "__clear__";

fn is_keep_secret(v: &str) -> bool {
    v.is_empty() || v == SECRET_MASK
}

fn resolve_secret(new: &str, old: String) -> String {
    if is_keep_secret(new) {
        old
    } else if new == SECRET_CLEAR {
        String::new()
    } else {
        new.to_string()
    }
}

fn mask_secret(v: &str) -> String {
    if v.is_empty() {
        String::new()
    } else {
        SECRET_MASK.into()
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let fig = Figment::from(Serialized::defaults(AppConfig::example()))
            .merge(Toml::file(path))
            .merge(Env::prefixed("FUJIANG_").split("__"));
        let mut cfg: AppConfig = fig.extract()?;
        if let Ok(tok) = std::env::var("NAPCAT_ACCESS_TOKEN") {
            if !tok.is_empty() {
                cfg.napcat.access_token = tok.clone();
                if cfg.adapter.access_token.is_empty() {
                    cfg.adapter.access_token = tok;
                }
            }
        }
        if let Ok(key) = std::env::var("FUJIANG_CLIST_KEY") {
            if !key.is_empty() {
                cfg.clist.api_key = key;
            }
        }
        Ok(cfg)
    }

    pub fn ws_url(&self) -> &str {
        if !self.adapter.ws_url.is_empty() {
            &self.adapter.ws_url
        } else {
            &self.napcat.ws_url
        }
    }

    pub fn access_token(&self) -> &str {
        if !self.adapter.access_token.is_empty() {
            &self.adapter.access_token
        } else {
            &self.napcat.access_token
        }
    }

    fn example() -> Self {
        Self {
            adapter: AdapterSection::default(),
            napcat: NapcatSection {
                ws_url: "ws://127.0.0.1:3001".into(),
                access_token: String::new(),
            },
            bot: BotSection {
                command_prefix: ".".into(),
                groups: vec![],
                allow_private: true,
            },
            store: StoreSection {
                db: "data/fujiang.db".into(),
                image_root: "data/images".into(),
                archive_dir: default_archive_dir(),
                archive_after_days: default_archive_days(),
                archive_every_hours: default_archive_hours(),
            },
            clist: ClistSection {
                username: String::new(),
                api_key: String::new(),
                limit: 10,
            },
            plugins: PluginsSection::default(),
            admin: AdminSection::default(),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let text = toml::to_string_pretty(self)?;
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Secrets become `********` (or empty if unset) so the page can show “don't change”.
    pub fn redacted(&self) -> Self {
        let mut c = self.clone();
        c.adapter.access_token = mask_secret(&c.adapter.access_token);
        c.napcat.access_token = mask_secret(&c.napcat.access_token);
        c.clist.api_key = mask_secret(&c.clist.api_key);
        c.admin.token = mask_secret(&c.admin.token);
        c
    }

    /// Empty / `********` keeps the old secret; `__clear__` wipes it.
    pub fn merge_secrets(old: &Self, mut new: Self) -> Self {
        new.adapter.access_token =
            resolve_secret(&new.adapter.access_token, old.adapter.access_token.clone());
        new.napcat.access_token =
            resolve_secret(&new.napcat.access_token, old.napcat.access_token.clone());
        new.clist.api_key = resolve_secret(&new.clist.api_key, old.clist.api_key.clone());
        new.admin.token = resolve_secret(&new.admin.token, old.admin.token.clone());
        if new.adapter.ws_url.is_empty() {
            new.adapter.ws_url = old.adapter.ws_url.clone();
        }
        if new.napcat.ws_url.is_empty() {
            new.napcat.ws_url = new.ws_url().to_string();
        }
        if new.adapter.access_token.is_empty() && !new.napcat.access_token.is_empty() {
            new.adapter.access_token = new.napcat.access_token.clone();
        } else if !new.adapter.access_token.is_empty() {
            new.napcat.access_token = new.adapter.access_token.clone();
        }
        new
    }

    pub fn to_bot_config(&self) -> fujiang_core::BotConfig {
        fujiang_core::BotConfig {
            command_prefix: self.bot.command_prefix.clone(),
            groups: self.bot.groups.clone(),
            allow_private: self.bot.allow_private,
            clist_username: self.clist.username.clone(),
            clist_api_key: self.clist.api_key.clone(),
            clist_limit: self.clist.limit,
            contest_update_minutes: self.plugins.contest.update_minutes,
            rank_update_minutes: self.plugins.rank.update_minutes,
            daily_reset_hour: self.plugins.problem.daily_reset_hour,
            fun_allow_mutate: self.plugins.fun.allow_mutate,
            fun_admins: self.plugins.fun.admins.clone(),
            fun_delete_files: self.plugins.fun.delete_files_on_album_del,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_empty_keeps_old() {
        let old = AppConfig::example();
        let mut old = old;
        old.adapter.access_token = "real-token".into();
        old.clist.api_key = "clist-key".into();
        let mut incoming = old.redacted();
        incoming.bot.command_prefix = "!".into();
        incoming.adapter.access_token.clear();
        let merged = AppConfig::merge_secrets(&old, incoming);
        assert_eq!(merged.adapter.access_token, "real-token");
        assert_eq!(merged.clist.api_key, "clist-key");
        assert_eq!(merged.bot.command_prefix, "!");
    }

    #[test]
    fn secret_clear_wipes() {
        let mut old = AppConfig::example();
        old.admin.token = "admin".into();
        let mut incoming = old.redacted();
        incoming.admin.token = SECRET_CLEAR.into();
        let merged = AppConfig::merge_secrets(&old, incoming);
        assert!(merged.admin.token.is_empty());
    }

    #[test]
    fn roundtrip_toml() {
        let mut cfg = AppConfig::example();
        cfg.bot.groups = vec![1, 2];
        cfg.adapter.backend = AdapterBackend::LlonebotHttp;
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.bot.groups, vec![1, 2]);
        assert_eq!(back.adapter.backend, AdapterBackend::LlonebotHttp);
        assert_eq!(back.admin.listen, "127.0.0.1:8787");
    }
}
