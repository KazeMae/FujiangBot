use std::collections::HashMap;
use std::sync::Arc;

use fujiang_core::Plugin;
use fujiang_plugin_contest::ContestPlugin;
use fujiang_plugin_fun::FunPlugin;
use fujiang_plugin_luck::LuckPlugin;
use fujiang_plugin_problem::ProblemPlugin;
use fujiang_plugin_rank::RankPlugin;
use serde_json::{json, Value};

use crate::config::AppConfig;

pub const NAMES: &[&str] = &["contest", "rank", "problem", "fun", "luck"];

pub fn make(name: &str) -> Option<Arc<dyn Plugin>> {
    Some(match name {
        "contest" => Arc::new(ContestPlugin::default()),
        "rank" => Arc::new(RankPlugin::default()),
        "problem" => Arc::new(ProblemPlugin::default()),
        "fun" => Arc::new(FunPlugin::default()),
        "luck" => Arc::new(LuckPlugin::default()),
        _ => return None,
    })
}

pub fn enabled(cfg: &AppConfig, name: &str) -> bool {
    match name {
        "contest" => cfg.plugins.contest.enabled,
        "rank" => cfg.plugins.rank.enabled,
        "problem" => cfg.plugins.problem.enabled,
        "fun" => cfg.plugins.fun.enabled,
        "luck" => cfg.plugins.luck.enabled,
        _ => false,
    }
}

pub fn initial(cfg: &AppConfig) -> Vec<Arc<dyn Plugin>> {
    NAMES
        .iter()
        .copied()
        .filter(|n| enabled(cfg, n))
        .filter_map(make)
        .collect()
}

pub fn needs_restart(name: &str, old: &AppConfig, new: &AppConfig) -> bool {
    match name {
        "contest" => old.plugins.contest.update_minutes != new.plugins.contest.update_minutes,
        "rank" => old.plugins.rank.update_minutes != new.plugins.rank.update_minutes,
        "problem" => old.plugins.problem.daily_reset_hour != new.plugins.problem.daily_reset_hour,
        "fun" => {
            old.plugins.fun.allow_mutate != new.plugins.fun.allow_mutate
                || old.plugins.fun.admins != new.plugins.fun.admins
                || old.plugins.fun.delete_files_on_album_del
                    != new.plugins.fun.delete_files_on_album_del
        }
        _ => old.plugins.configs.get(name) != new.plugins.configs.get(name),
    }
}

pub fn config_view(cfg: &AppConfig, name: &str) -> Value {
    match name {
        "contest" => json!({
            "enabled": cfg.plugins.contest.enabled,
            "update_minutes": cfg.plugins.contest.update_minutes,
        }),
        "rank" => json!({
            "enabled": cfg.plugins.rank.enabled,
            "update_minutes": cfg.plugins.rank.update_minutes,
        }),
        "problem" => json!({
            "enabled": cfg.plugins.problem.enabled,
            "daily_reset_hour": cfg.plugins.problem.daily_reset_hour,
        }),
        "fun" => json!({
            "enabled": cfg.plugins.fun.enabled,
            "allow_mutate": cfg.plugins.fun.allow_mutate,
            "admins": cfg.plugins.fun.admins,
            "delete_files_on_album_del": cfg.plugins.fun.delete_files_on_album_del,
        }),
        "luck" => json!({
            "enabled": cfg.plugins.luck.enabled,
        }),
        other => cfg
            .plugins
            .configs
            .get(other)
            .cloned()
            .unwrap_or_else(|| json!({})),
    }
}

pub fn all_configs(cfg: &AppConfig) -> HashMap<String, Value> {
    let mut m = cfg.plugins.configs.clone();
    for name in NAMES {
        m.insert((*name).into(), config_view(cfg, name));
    }
    m
}

pub fn apply_config(cfg: &mut AppConfig, name: &str, v: Value) -> anyhow::Result<()> {
    match name {
        "contest" => {
            if let Some(e) = v.get("enabled").and_then(|x| x.as_bool()) {
                cfg.plugins.contest.enabled = e;
            }
            if let Some(m) = v.get("update_minutes").and_then(|x| x.as_u64()) {
                anyhow::ensure!(m >= 5, "contest.update_minutes 至少为 5");
                cfg.plugins.contest.update_minutes = m;
            }
        }
        "rank" => {
            if let Some(e) = v.get("enabled").and_then(|x| x.as_bool()) {
                cfg.plugins.rank.enabled = e;
            }
            if let Some(m) = v.get("update_minutes").and_then(|x| x.as_u64()) {
                anyhow::ensure!(m >= 10, "rank.update_minutes 至少为 10");
                cfg.plugins.rank.update_minutes = m;
            }
        }
        "problem" => {
            if let Some(e) = v.get("enabled").and_then(|x| x.as_bool()) {
                cfg.plugins.problem.enabled = e;
            }
            if let Some(h) = v.get("daily_reset_hour").and_then(|x| x.as_u64()) {
                anyhow::ensure!(h <= 23, "daily_reset_hour 必须是 0..=23");
                cfg.plugins.problem.daily_reset_hour = h as u32;
            }
        }
        "fun" => {
            if let Some(e) = v.get("enabled").and_then(|x| x.as_bool()) {
                cfg.plugins.fun.enabled = e;
            }
            if let Some(e) = v.get("allow_mutate").and_then(|x| x.as_bool()) {
                cfg.plugins.fun.allow_mutate = e;
            }
            if let Some(e) = v.get("delete_files_on_album_del").and_then(|x| x.as_bool()) {
                cfg.plugins.fun.delete_files_on_album_del = e;
            }
            if let Some(arr) = v.get("admins").and_then(|x| x.as_array()) {
                let mut admins = Vec::new();
                for item in arr {
                    let n = item
                        .as_i64()
                        .ok_or_else(|| anyhow::anyhow!("fun.admins 必须是整数"))?;
                    admins.push(n);
                }
                cfg.plugins.fun.admins = admins;
            }
        }
        "luck" => {
            if let Some(e) = v.get("enabled").and_then(|x| x.as_bool()) {
                cfg.plugins.luck.enabled = e;
            }
        }
        other => {
            anyhow::ensure!(v.is_object() || v.is_null(), "插件配置必须是 JSON 对象");
            if v.is_null() {
                cfg.plugins.configs.remove(other);
            } else {
                cfg.plugins.configs.insert(other.to_string(), v);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> AppConfig {
        toml::from_str(
            r#"
            [napcat]
            ws_url = "ws://127.0.0.1:3001"
            [bot]
            [store]
            db = "x.db"
            image_root = "img"
            [clist]
            [plugins]
            "#,
        )
        .unwrap()
    }

    #[test]
    fn apply_typed_and_dynamic_config() {
        let mut cfg = sample();
        apply_config(&mut cfg, "contest", json!({ "update_minutes": 15 })).unwrap();
        assert_eq!(cfg.plugins.contest.update_minutes, 15);
        apply_config(&mut cfg, "echo", json!({ "prefix": "!" })).unwrap();
        assert_eq!(cfg.plugins.configs["echo"]["prefix"], "!");
        assert_eq!(config_view(&cfg, "echo")["prefix"], "!");
    }
}
