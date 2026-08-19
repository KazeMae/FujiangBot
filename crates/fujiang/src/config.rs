use std::path::Path;

use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub napcat: NapcatSection,
    pub bot: BotSection,
    pub store: StoreSection,
    pub clist: ClistSection,
    pub plugins: PluginsSection,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginsSection {
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

impl AppConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let fig = Figment::from(Serialized::defaults(AppConfig::example()))
            .merge(Toml::file(path))
            .merge(Env::prefixed("FUJIANG_").split("__"));
        let mut cfg: AppConfig = fig.extract()?;
        if let Ok(tok) = std::env::var("NAPCAT_ACCESS_TOKEN") {
            if !tok.is_empty() {
                cfg.napcat.access_token = tok;
            }
        }
        if let Ok(key) = std::env::var("FUJIANG_CLIST_KEY") {
            if !key.is_empty() {
                cfg.clist.api_key = key;
            }
        }
        Ok(cfg)
    }

    fn example() -> Self {
        Self {
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
            },
            clist: ClistSection {
                username: String::new(),
                api_key: String::new(),
                limit: 10,
            },
            plugins: PluginsSection::default(),
        }
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
