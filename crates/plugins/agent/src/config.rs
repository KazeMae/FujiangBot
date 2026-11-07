use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::tools::ToolSpec;

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const DEFAULT_MODEL: &str = "deepseek-flash";
const DEFAULT_API_KEY_ENV: &str = "FUJIANG_LLM_KEY";
const DEFAULT_TEMPERATURE: f64 = 0.7;
const DEFAULT_MAX_HISTORY_TURNS: u32 = 20;
const DEFAULT_SYSTEM_PROMPT: &str = "你是福酱，西南科技大学 ACM 群助手。";
const DEFAULT_TIMEOUT_SECS: u64 = 60;
const DEFAULT_MAX_TOOL_ROUNDS: u32 = 3;
const DEFAULT_USER_RATE: u32 = 30;
const DEFAULT_GROUP_RATE: u32 = 120;
const DEFAULT_MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
const DEFAULT_CONTEXT_BUDGET: u64 = crate::context_budget::DEFAULT_CONTEXT_BUDGET;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimit {
    pub per_user_per_hour: u32,
    pub per_group_per_hour: u32,
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
    pub temperature: f64,
    pub max_history_turns: u32,
    pub context_budget: u64,
    pub model_context_limit: Option<u64>,
    pub image_token_upper_bound: Option<u64>,
    pub memory_enabled: bool,
    /// Body of the `default` persona.
    pub system_prompt: String,
    /// Named personas other than `default`. The default body is `system_prompt`.
    personas: BTreeMap<String, String>,
    pub wake_words: Vec<String>,
    pub require_at: bool,
    pub rate_limit: RateLimit,
    /// Seconds for one request. Missing or 0 means 60.
    pub timeout_secs: u64,
    /// Tool executions allowed in one user turn. `0` runs none.
    pub max_tool_rounds: u32,
    pub enable_rank_tools: bool,
    pub enable_contest_tools: bool,
    pub enable_problem_tools: bool,
    pub enable_memo_tools: bool,
    pub enable_memo_write_tools: bool,
    pub enable_fun_tools: bool,
    pub enable_memory_tools: bool,
    pub enable_memory_write_tools: bool,
    pub memory_max_unused_days: u32,
    pub memory_max_entries_per_scope: u32,
    pub memory_consolidate_on_start: bool,
    pub inject_tool_list: bool,
    pub tools: Vec<ToolSpec>,
    pub vision_enabled: bool,
    pub max_image_bytes: u64,
    blacklist: Vec<String>,
    admins: BTreeSet<i64>,
}

impl AgentConfig {
    /// Lenient parse. Unknown host fields (`priority`, `groups`, `allow_private`, `enabled`) and a leftover `api` key are ignored.
    pub fn from_value(v: &Value) -> Self {
        let (system_prompt, personas) = personas_from(v);
        Self {
            base_url: base_url_from(v),
            model: string_field(v, "model", DEFAULT_MODEL),
            api_key_env: string_field(v, "api_key_env", DEFAULT_API_KEY_ENV),
            temperature: temperature_from(v),
            max_history_turns: u32_field(v, "max_history_turns", DEFAULT_MAX_HISTORY_TURNS),
            context_budget: context_budget_from(v),
            model_context_limit: model_context_limit_from(v),
            image_token_upper_bound: positive_limit(v, "image_token_upper_bound"),
            memory_enabled: memory_enabled_from(v),
            system_prompt,
            personas,
            wake_words: wake_words_from(v),
            require_at: bool_field(v, "require_at", false),
            rate_limit: rate_limit_from(v),
            timeout_secs: timeout_secs_from(v),
            max_tool_rounds: u32_field(v, "max_tool_rounds", DEFAULT_MAX_TOOL_ROUNDS),
            enable_rank_tools: bool_field(v, "enable_rank_tools", true),
            enable_contest_tools: bool_field(v, "enable_contest_tools", true),
            enable_problem_tools: bool_field(v, "enable_problem_tools", true),
            enable_memo_tools: bool_field(v, "enable_memo_tools", true),
            enable_memo_write_tools: bool_field(v, "enable_memo_write_tools", false),
            enable_fun_tools: bool_field(v, "enable_fun_tools", true),
            enable_memory_tools: bool_field(v, "enable_memory_tools", true),
            enable_memory_write_tools: bool_field(v, "enable_memory_write_tools", true),
            memory_max_unused_days: u32_field(
                v,
                "memory_max_unused_days",
                crate::memory_consolidate::DEFAULT_UNUSED_DAYS,
            ),
            memory_max_entries_per_scope: u32_field(
                v,
                "memory_max_entries_per_scope",
                crate::memory_consolidate::DEFAULT_MAX_ENTRIES,
            ),
            memory_consolidate_on_start: bool_field(v, "memory_consolidate_on_start", false),
            inject_tool_list: bool_field(v, "inject_tool_list", false),
            tools: tools_from(v),
            vision_enabled: bool_field(v, "vision_enabled", false),
            max_image_bytes: max_image_bytes_from(v),
            blacklist: blacklist_from(v),
            admins: admins_from(v),
        }
    }

    pub(crate) fn keyword_hit(&self, user_text: &str) -> bool {
        self.blacklist
            .iter()
            .any(|needle| user_text.contains(needle))
    }

    pub(crate) fn is_admin(&self, user_id: i64) -> bool {
        self.admins.contains(&user_id)
    }

    pub fn prompt_for(&self, name: &str) -> Option<&str> {
        if name == "default" {
            Some(self.system_prompt.as_str())
        } else {
            self.personas.get(name).map(String::as_str)
        }
    }

    pub fn persona_names(&self) -> Vec<&str> {
        let mut names = Vec::with_capacity(self.personas.len() + 1);
        names.push("default");
        names.extend(self.personas.keys().map(String::as_str));
        names
    }

    /// Stored name, or the default persona when the name is missing from config.
    pub fn resolve_persona(&self, stored: Option<&str>) -> (&str, &str) {
        if let Some(name) = stored {
            if name == "default" {
                return ("default", self.system_prompt.as_str());
            }
            if let Some((key, body)) = self.personas.get_key_value(name) {
                return (key.as_str(), body.as_str());
            }
        }
        ("default", self.system_prompt.as_str())
    }
}

fn string_field(v: &Value, key: &str, default: &str) -> String {
    match v.get(key).and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => text.trim().to_string(),
        _ => default.to_string(),
    }
}

fn base_url_from(v: &Value) -> String {
    let text = string_field(v, "base_url", DEFAULT_BASE_URL);
    let trimmed = text.trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        DEFAULT_BASE_URL.to_string()
    } else {
        trimmed
    }
}

fn temperature_from(v: &Value) -> f64 {
    v.get("temperature")
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
        .unwrap_or(DEFAULT_TEMPERATURE)
}

fn context_budget_from(v: &Value) -> u64 {
    match v.get("context_budget").and_then(Value::as_u64) {
        Some(0) | None => DEFAULT_CONTEXT_BUDGET,
        Some(n) => n,
    }
}

fn model_context_limit_from(v: &Value) -> Option<u64> {
    positive_limit(v, "model_context_limit")
}

fn positive_limit(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64).filter(|n| *n > 0)
}

fn u32_field(v: &Value, key: &str, default: u32) -> u32 {
    v.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(default)
}

fn timeout_secs_from(v: &Value) -> u64 {
    match v.get("timeout_secs").and_then(Value::as_u64) {
        Some(0) | None => DEFAULT_TIMEOUT_SECS,
        Some(secs) => secs,
    }
}

fn max_image_bytes_from(v: &Value) -> u64 {
    match v.get("max_image_bytes").and_then(Value::as_u64) {
        Some(0) | None => DEFAULT_MAX_IMAGE_BYTES,
        Some(n) => n,
    }
}

fn bool_field(v: &Value, key: &str, default: bool) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn memory_enabled_from(v: &Value) -> bool {
    v.get("memory")
        .and_then(|memory| memory.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

fn wake_words_from(v: &Value) -> Vec<String> {
    match v.get("wake_words") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|word| !word.is_empty())
            .map(str::to_string)
            .collect(),
        _ => vec!["福酱".to_string()],
    }
}

fn blacklist_from(v: &Value) -> Vec<String> {
    match v.get("blacklist") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|word| !word.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn admins_from(v: &Value) -> BTreeSet<i64> {
    let Some(items) = v.get("admins").and_then(Value::as_array) else {
        return BTreeSet::new();
    };
    items.iter().filter_map(admin_id).collect()
}

fn admin_id(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.trim().parse().ok())
}

fn tools_from(v: &Value) -> Vec<ToolSpec> {
    let Some(items) = v.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };
    items.iter().filter_map(tool_spec_from).collect()
}

fn tool_spec_from(v: &Value) -> Option<ToolSpec> {
    let name = v.get("name")?.as_str()?;
    let description = v.get("description").and_then(Value::as_str).unwrap_or("");
    let parameters = v.get("parameters").cloned().unwrap_or(Value::Null);
    ToolSpec::parse(name, description, parameters)
}

fn rate_limit_from(v: &Value) -> RateLimit {
    let rates = v.get("rate_limit").unwrap_or(&Value::Null);
    RateLimit {
        per_user_per_hour: u32_field(rates, "per_user_per_hour", DEFAULT_USER_RATE),
        per_group_per_hour: u32_field(rates, "per_group_per_hour", DEFAULT_GROUP_RATE),
    }
}

/// `personas.default.system_prompt` wins over the top-level `system_prompt`.
/// The default body stays in `system_prompt`. Other names go in the map.
fn personas_from(v: &Value) -> (String, BTreeMap<String, String>) {
    let mut default_prompt = string_field(v, "system_prompt", DEFAULT_SYSTEM_PROMPT);
    let mut extras = BTreeMap::new();
    let Some(obj) = v.get("personas").and_then(Value::as_object) else {
        return (default_prompt, extras);
    };
    for (name, body) in obj {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let Some(prompt) = body.get("system_prompt").and_then(Value::as_str) else {
            continue;
        };
        let prompt = prompt.trim();
        if prompt.is_empty() {
            continue;
        }
        if name == "default" {
            default_prompt = prompt.to_string();
        } else {
            extras.insert(name.to_string(), prompt.to_string());
        }
    }
    (default_prompt, extras)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use serde_json::json;

    fn assert_defaults(cfg: &AgentConfig) {
        assert_eq!(cfg.base_url, "https://api.deepseek.com");
        assert_eq!(cfg.model, "deepseek-flash");
        assert_eq!(cfg.api_key_env, "FUJIANG_LLM_KEY");
        assert_eq!(cfg.temperature, 0.7);
        assert_eq!(cfg.max_history_turns, 20);
        assert_eq!(cfg.context_budget, 500_000);
        assert_eq!(cfg.model_context_limit, None);
        assert_eq!(cfg.image_token_upper_bound, None);
        assert!(cfg.memory_enabled);
        assert_eq!(cfg.system_prompt, "你是福酱，西南科技大学 ACM 群助手。");
        assert!(cfg.personas.is_empty());
        assert_eq!(cfg.persona_names(), vec!["default"]);
        assert_eq!(
            cfg.prompt_for("default"),
            Some("你是福酱，西南科技大学 ACM 群助手。")
        );
        assert_eq!(cfg.prompt_for("strict"), None);
        assert_eq!(cfg.wake_words, vec!["福酱".to_string()]);
        assert!(!cfg.require_at);
        assert_eq!(cfg.rate_limit.per_user_per_hour, 30);
        assert_eq!(cfg.rate_limit.per_group_per_hour, 120);
        assert_eq!(cfg.timeout_secs, 60);
        assert_eq!(cfg.max_tool_rounds, 3);
        assert!(cfg.enable_rank_tools);
        assert!(cfg.enable_contest_tools);
        assert!(cfg.enable_problem_tools);
        assert!(cfg.enable_memo_tools);
        assert!(!cfg.enable_memo_write_tools);
        assert!(cfg.enable_fun_tools);
        assert!(cfg.enable_memory_tools);
        assert!(cfg.enable_memory_write_tools);
        assert_eq!(cfg.memory_max_unused_days, 90);
        assert_eq!(cfg.memory_max_entries_per_scope, 200);
        assert!(!cfg.memory_consolidate_on_start);
        assert!(!cfg.inject_tool_list);
        assert!(cfg.tools.is_empty());
        assert!(!cfg.vision_enabled);
        assert_eq!(cfg.max_image_bytes, 5 * 1024 * 1024);
        assert!(cfg.blacklist.is_empty());
        assert!(cfg.admins.is_empty());
        assert!(!cfg.keyword_hit("代写"));
        assert!(!cfg.is_admin(1));
    }

    #[test]
    fn defaults_when_empty_or_null() {
        assert_defaults(&AgentConfig::from_value(&json!({})));
        assert_defaults(&AgentConfig::from_value(&Value::Null));
    }

    #[test]
    fn overrides_and_ignores_host_fields() {
        let cfg = AgentConfig::from_value(&json!({
            "base_url": " https://example.test/v1/ ",
            "model": "deepseek-chat",
            "api_key_env": "MY_KEY",
            "temperature": 0.2,
            "max_history_turns": 4,
            "system_prompt": "be brief",
            "wake_words": ["福", " bot "],
            "require_at": true,
            "rate_limit": { "per_user_per_hour": 2, "per_group_per_hour": 9 },
            "timeout_secs": 5,
            "priority": 10,
            "groups": [1],
            "allow_private": false,
            "enabled": true,
            "api": "chat",
            "inject_tool_list": true
        }));
        assert_eq!(cfg.base_url, "https://example.test/v1");
        assert_eq!(cfg.model, "deepseek-chat");
        assert_eq!(cfg.api_key_env, "MY_KEY");
        assert_eq!(cfg.temperature, 0.2);
        assert_eq!(cfg.max_history_turns, 4);
        assert_eq!(cfg.system_prompt, "be brief");
        assert_eq!(cfg.prompt_for("default"), Some("be brief"));
        assert!(cfg.personas.is_empty());
        assert_eq!(cfg.wake_words, vec!["福".to_string(), "bot".to_string()]);
        assert!(cfg.require_at);
        assert_eq!(cfg.rate_limit.per_user_per_hour, 2);
        assert_eq!(cfg.rate_limit.per_group_per_hour, 9);
        assert_eq!(cfg.timeout_secs, 5);
        assert!(cfg.inject_tool_list);
    }

    #[test]
    fn bad_types_fall_back_field_by_field() {
        let cfg = AgentConfig::from_value(&json!({
            "model": "",
            "temperature": "hot",
            "max_history_turns": -1,
            "wake_words": "福酱",
            "require_at": "yes",
            "timeout_secs": 0,
            "rate_limit": { "per_user_per_hour": "nope" },
            "personas": "nope",
            "max_tool_rounds": "nope",
            "enable_rank_tools": "no",
            "enable_contest_tools": 0,
            "enable_problem_tools": "no",
            "enable_memo_tools": "no",
            "enable_memo_write_tools": "yes",
            "enable_fun_tools": "no",
            "enable_memory_tools": "no",
            "enable_memory_write_tools": "no",
            "memory_max_unused_days": "no",
            "memory_max_entries_per_scope": -1,
            "memory_consolidate_on_start": "yes",
            "inject_tool_list": "yes",
            "tools": "nope",
            "vision_enabled": "yes",
            "max_image_bytes": "big",
            "blacklist": "nope",
            "admins": 1,
            "memory": { "enabled": "no" }
        }));
        assert_defaults(&cfg);
    }

    #[test]
    fn memory_stays_on_unless_the_flag_is_false() {
        let off = AgentConfig::from_value(&json!({ "memory": { "enabled": false } }));
        assert!(!off.memory_enabled);
        let on = AgentConfig::from_value(&json!({ "memory": { "enabled": true } }));
        assert!(on.memory_enabled);
        let blank = AgentConfig::from_value(&json!({ "memory": {} }));
        assert!(blank.memory_enabled);
    }

    #[test]
    fn explicit_empty_wake_words_stay_empty() {
        let cfg = AgentConfig::from_value(&json!({ "wake_words": [] }));
        assert!(cfg.wake_words.is_empty());
    }

    #[test]
    fn named_personas_overlay_the_default_prompt() {
        let cfg = AgentConfig::from_value(&json!({
            "system_prompt": "top",
            "personas": {
                "default": { "system_prompt": " from default " },
                "strict": { "system_prompt": " be strict " },
                "empty": { "system_prompt": "  " },
                "bad": 1,
                " ": { "system_prompt": "nope" }
            }
        }));
        assert_eq!(cfg.system_prompt, "from default");
        assert_eq!(cfg.prompt_for("default"), Some("from default"));
        assert_eq!(cfg.prompt_for("strict"), Some("be strict"));
        assert_eq!(cfg.prompt_for("missing"), None);
        assert_eq!(cfg.persona_names(), vec!["default", "strict"]);
        assert_eq!(cfg.resolve_persona(None), ("default", "from default"));
        assert_eq!(cfg.resolve_persona(Some("strict")), ("strict", "be strict"));
        assert_eq!(
            cfg.resolve_persona(Some("missing")),
            ("default", "from default")
        );

        let only_named = AgentConfig::from_value(&json!({
            "personas": { "strict": { "system_prompt": "be strict" } }
        }));
        assert_eq!(
            only_named.system_prompt,
            "你是福酱，西南科技大学 ACM 群助手。"
        );
        assert_eq!(only_named.prompt_for("strict"), Some("be strict"));
    }

    #[test]
    fn tool_specs_and_round_cap_are_lenient() {
        let cfg = AgentConfig::from_value(&json!({
            "max_tool_rounds": 0,
            "tools": [
                {
                    "name": " rank.list ",
                    "description": " 榜 ",
                    "parameters": {"type": "object", "properties": {}}
                },
                {"name": "nolist", "description": "no"},
                {"name": " ", "description": "blank"},
                "nope",
                {"description": "missing name"}
            ],
            "future_tool_key": true
        }));
        assert_eq!(cfg.max_tool_rounds, 0);
        assert_eq!(cfg.tools.len(), 1);
        assert_eq!(cfg.tools[0].name(), "rank.list");
        assert_eq!(cfg.tools[0].description(), "榜");
        assert_eq!(cfg.tools[0].service(), "rank");
        assert_eq!(cfg.tools[0].method(), "list");
        assert_eq!(cfg.tools[0].wire_name(), "rank_list");

        let fallback = AgentConfig::from_value(&json!({
            "max_tool_rounds": -1,
            "tools": {"name": "rank.list"}
        }));
        assert_eq!(fallback.max_tool_rounds, 3);
        assert!(fallback.tools.is_empty());
    }

    #[test]
    fn builtin_tool_flags_default_on_and_can_turn_off() {
        let off = AgentConfig::from_value(&json!({
            "enable_rank_tools": false,
            "enable_contest_tools": false,
            "enable_problem_tools": false,
            "enable_memo_tools": false,
            "enable_fun_tools": false,
            "enable_memory_tools": false,
            "enable_memory_write_tools": false
        }));
        assert!(!off.enable_rank_tools);
        assert!(!off.enable_contest_tools);
        assert!(!off.enable_problem_tools);
        assert!(!off.enable_memo_tools);
        assert!(!off.enable_memo_write_tools);
        assert!(!off.enable_fun_tools);
        assert!(!off.enable_memory_tools);
        assert!(!off.enable_memory_write_tools);

        let rank_only = AgentConfig::from_value(&json!({
            "enable_contest_tools": false,
            "enable_problem_tools": false,
            "enable_memo_tools": false
        }));
        assert!(rank_only.enable_rank_tools);
        assert!(!rank_only.enable_contest_tools);
        assert!(!rank_only.enable_problem_tools);
        assert!(!rank_only.enable_memo_tools);
        assert!(rank_only.enable_fun_tools);
        assert!(rank_only.enable_memory_tools);
        assert!(rank_only.enable_memory_write_tools);

        let write = AgentConfig::from_value(&json!({
            "enable_memo_write_tools": true
        }));
        assert!(write.enable_memo_tools);
        assert!(write.enable_memo_write_tools);

        let read = AgentConfig::from_value(&json!({
            "enable_memory_write_tools": false
        }));
        assert!(read.enable_memory_tools);
        assert!(!read.enable_memory_write_tools);

        let limits = AgentConfig::from_value(&json!({
            "memory_max_unused_days": 7,
            "memory_max_entries_per_scope": 3,
            "memory_consolidate_on_start": true
        }));
        assert_eq!(limits.memory_max_unused_days, 7);
        assert_eq!(limits.memory_max_entries_per_scope, 3);
        assert!(limits.memory_consolidate_on_start);
        let zero = AgentConfig::from_value(&json!({
            "memory_max_unused_days": 0,
            "memory_max_entries_per_scope": 0
        }));
        assert_eq!(zero.memory_max_unused_days, 0);
        assert_eq!(zero.memory_max_entries_per_scope, 0);
        assert!(!zero.memory_consolidate_on_start);
    }

    #[test]
    fn vision_defaults_off_and_zero_cap_uses_five_mib() {
        let on = AgentConfig::from_value(&json!({
            "vision_enabled": true,
            "max_image_bytes": 1024
        }));
        assert!(on.vision_enabled);
        assert_eq!(on.max_image_bytes, 1024);

        let zero = AgentConfig::from_value(&json!({ "max_image_bytes": 0 }));
        assert!(!zero.vision_enabled);
        assert_eq!(zero.max_image_bytes, 5 * 1024 * 1024);
    }

    #[test]
    fn blacklist_keeps_trimmed_needles_and_admins_parse_ids() {
        let cfg = AgentConfig::from_value(&json!({
            "blacklist": [" 代写 ", "", "代写", 1],
            "admins": [10001, "10002", " 10003 ", "x", 1.5, true]
        }));
        assert_eq!(cfg.blacklist, vec!["代写".to_string(), "代写".to_string()]);
        assert_eq!(cfg.admins, BTreeSet::from([10001, 10002, 10003]));
        assert!(cfg.keyword_hit("请代写"));
        assert!(!cfg.keyword_hit("请写"));
        assert!(cfg.is_admin(10002));
        assert!(!cfg.is_admin(7));
    }
}
