use std::collections::HashMap;
use std::sync::Arc;

use fujiang_core::Plugin;
use fujiang_plugin_contest::ContestPlugin;
use fujiang_plugin_fun::FunPlugin;
use fujiang_plugin_luck::LuckPlugin;
use fujiang_plugin_problem::ProblemPlugin;
use fujiang_plugin_rank::RankPlugin;
use serde_json::{json, Value};
use tracing::warn;

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

pub fn instance_disabled(cfg: &AppConfig, id: &str) -> bool {
    cfg.plugins.disabled.iter().any(|n| n == id)
        || cfg
            .plugins
            .instances
            .iter()
            .any(|i| i.id == id && i.disabled)
}

pub fn initial(cfg: &AppConfig) -> Vec<(String, Arc<dyn Plugin>)> {
    let mut out = Vec::new();
    let mut used = std::collections::HashSet::new();
    for name in NAMES {
        if enabled(cfg, name) && !instance_disabled(cfg, name) {
            if let Some(p) = make(name) {
                used.insert((*name).to_string());
                out.push(((*name).to_string(), p));
            }
        }
    }
    for inst in &cfg.plugins.instances {
        if inst.disabled || instance_disabled(cfg, &inst.id) {
            continue;
        }
        if used.contains(&inst.id) {
            warn!(id = %inst.id, "skip instance, id already used");
            continue;
        }
        if !NAMES.contains(&inst.plugin.as_str()) {
            continue;
        }
        let Ok(_) = fujiang_store::sanitize_plugin_name(&inst.id) else {
            warn!(id = %inst.id, "skip instance, invalid id");
            continue;
        };
        if let Some(p) = make(&inst.plugin) {
            used.insert(inst.id.clone());
            out.push((inst.id.clone(), p));
        }
    }
    out
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

fn merge_overlay(typed: Value, overlay: Option<&Value>) -> Value {
    let mut out = match overlay {
        Some(Value::Object(m)) => Value::Object(m.clone()),
        _ => json!({}),
    };
    if let (Value::Object(typed_map), Value::Object(out_map)) = (typed, &mut out) {
        for (k, v) in typed_map {
            out_map.insert(k, v);
        }
    }
    out
}

pub fn config_view(cfg: &AppConfig, name: &str) -> Value {
    let overlay = cfg.plugins.configs.get(name);
    match name {
        "contest" => merge_overlay(
            json!({
                "enabled": cfg.plugins.contest.enabled,
                "update_minutes": cfg.plugins.contest.update_minutes,
            }),
            overlay,
        ),
        "rank" => merge_overlay(
            json!({
                "enabled": cfg.plugins.rank.enabled,
                "update_minutes": cfg.plugins.rank.update_minutes,
            }),
            overlay,
        ),
        "problem" => merge_overlay(
            json!({
                "enabled": cfg.plugins.problem.enabled,
                "daily_reset_hour": cfg.plugins.problem.daily_reset_hour,
            }),
            overlay,
        ),
        "fun" => merge_overlay(
            json!({
                "enabled": cfg.plugins.fun.enabled,
                "allow_mutate": cfg.plugins.fun.allow_mutate,
                "admins": cfg.plugins.fun.admins,
                "delete_files_on_album_del": cfg.plugins.fun.delete_files_on_album_del,
            }),
            overlay,
        ),
        "luck" => merge_overlay(
            json!({
                "enabled": cfg.plugins.luck.enabled,
            }),
            overlay,
        ),
        _other => overlay.cloned().unwrap_or_else(|| json!({})),
    }
}

pub fn all_configs(cfg: &AppConfig) -> HashMap<String, Value> {
    let mut m = cfg.plugins.configs.clone();
    for name in NAMES {
        m.insert((*name).into(), config_view(cfg, name));
    }
    m
}

fn stash_overlay(cfg: &mut AppConfig, name: &str, v: &Value, typed: &[&str]) {
    let Some(obj) = v.as_object() else {
        return;
    };
    let mut extra = match cfg.plugins.configs.get(name) {
        Some(Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    };
    for t in typed {
        extra.remove(*t);
    }
    for (k, val) in obj {
        if typed.contains(&k.as_str()) {
            continue;
        }
        extra.insert(k.clone(), val.clone());
    }
    if extra.is_empty() {
        cfg.plugins.configs.remove(name);
    } else {
        cfg.plugins
            .configs
            .insert(name.to_string(), Value::Object(extra));
    }
}

fn validate_host_keys(v: &Value) -> anyhow::Result<()> {
    if let Some(arr) = v.get("groups") {
        anyhow::ensure!(arr.is_array(), "groups 必须是整数数组");
        if let Some(items) = arr.as_array() {
            for item in items {
                anyhow::ensure!(item.as_i64().is_some(), "groups 必须是整数数组");
            }
        }
    }
    if let Some(p) = v.get("allow_private") {
        anyhow::ensure!(p.is_boolean(), "allow_private 必须是布尔值");
    }
    if let Some(p) = v.get("priority") {
        anyhow::ensure!(p.as_i64().is_some(), "priority 必须是整数");
    }
    Ok(())
}

pub fn apply_config(cfg: &mut AppConfig, name: &str, v: Value) -> anyhow::Result<()> {
    validate_host_keys(&v)?;
    match name {
        "contest" => {
            if let Some(e) = v.get("enabled").and_then(|x| x.as_bool()) {
                cfg.plugins.contest.enabled = e;
            }
            if let Some(m) = v.get("update_minutes").and_then(|x| x.as_u64()) {
                anyhow::ensure!(m >= 5, "contest.update_minutes 至少为 5");
                cfg.plugins.contest.update_minutes = m;
            }
            stash_overlay(cfg, name, &v, &["enabled", "update_minutes"]);
        }
        "rank" => {
            if let Some(e) = v.get("enabled").and_then(|x| x.as_bool()) {
                cfg.plugins.rank.enabled = e;
            }
            if let Some(m) = v.get("update_minutes").and_then(|x| x.as_u64()) {
                anyhow::ensure!(m >= 10, "rank.update_minutes 至少为 10");
                cfg.plugins.rank.update_minutes = m;
            }
            stash_overlay(cfg, name, &v, &["enabled", "update_minutes"]);
        }
        "problem" => {
            if let Some(e) = v.get("enabled").and_then(|x| x.as_bool()) {
                cfg.plugins.problem.enabled = e;
            }
            if let Some(h) = v.get("daily_reset_hour").and_then(|x| x.as_u64()) {
                anyhow::ensure!(h <= 23, "daily_reset_hour 必须是 0..=23");
                cfg.plugins.problem.daily_reset_hour = h as u32;
            }
            stash_overlay(cfg, name, &v, &["enabled", "daily_reset_hour"]);
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
            stash_overlay(
                cfg,
                name,
                &v,
                &[
                    "enabled",
                    "allow_mutate",
                    "delete_files_on_album_del",
                    "admins",
                ],
            );
        }
        "luck" => {
            if let Some(e) = v.get("enabled").and_then(|x| x.as_bool()) {
                cfg.plugins.luck.enabled = e;
            }
            stash_overlay(cfg, name, &v, &["enabled"]);
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
        apply_config(
            &mut cfg,
            "luck",
            json!({ "enabled": true, "groups": [1, 2], "priority": 3 }),
        )
        .unwrap();
        assert_eq!(cfg.plugins.luck.enabled, true);
        assert_eq!(config_view(&cfg, "luck")["groups"], json!([1, 2]));
        assert_eq!(config_view(&cfg, "luck")["priority"], 3);
        assert_eq!(config_view(&cfg, "luck")["enabled"], true);
    }
}
