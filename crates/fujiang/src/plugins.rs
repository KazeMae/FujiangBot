use std::sync::Arc;

use fujiang_core::Plugin;
use fujiang_plugin_contest::ContestPlugin;
use fujiang_plugin_fun::FunPlugin;
use fujiang_plugin_luck::LuckPlugin;
use fujiang_plugin_problem::ProblemPlugin;
use fujiang_plugin_rank::RankPlugin;

use crate::config::AppConfig;

pub const NAMES: &[&str] = &["contest", "rank", "problem", "fun", "luck"];

pub fn make(name: &str) -> Option<Arc<dyn Plugin>> {
    Some(match name {
        "contest" => Arc::new(ContestPlugin::default()),
        "rank" => Arc::new(RankPlugin::default()),
        "problem" => Arc::new(ProblemPlugin),
        "fun" => Arc::new(FunPlugin),
        "luck" => Arc::new(LuckPlugin),
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
        _ => false,
    }
}
