use serde_json::Value;

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const DEFAULT_MODEL: &str = "deepseek-flash";
const DEFAULT_API_KEY_ENV: &str = "FUJIANG_LLM_KEY";
const DEFAULT_TEMPERATURE: f64 = 0.7;
const DEFAULT_MAX_HISTORY_TURNS: u32 = 20;
const DEFAULT_SYSTEM_PROMPT: &str = "你是福酱，西南科技大学 ACM 群助手。";
const DEFAULT_TIMEOUT_SECS: u64 = 60;
const DEFAULT_USER_RATE: u32 = 30;
const DEFAULT_GROUP_RATE: u32 = 120;

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
    pub system_prompt: String,
    pub wake_words: Vec<String>,
    pub require_at: bool,
    pub rate_limit: RateLimit,
    /// Seconds for one request. Missing or 0 means 60.
    pub timeout_secs: u64,
}

impl AgentConfig {
    /// Lenient parse. Unknown host fields (`priority`, `groups`, `allow_private`, `enabled`) and a leftover `api` key are ignored.
    pub fn from_value(v: &Value) -> Self {
        Self {
            base_url: base_url_from(v),
            model: string_field(v, "model", DEFAULT_MODEL),
            api_key_env: string_field(v, "api_key_env", DEFAULT_API_KEY_ENV),
            temperature: temperature_from(v),
            max_history_turns: u32_field(v, "max_history_turns", DEFAULT_MAX_HISTORY_TURNS),
            system_prompt: string_field(v, "system_prompt", DEFAULT_SYSTEM_PROMPT),
            wake_words: wake_words_from(v),
            require_at: bool_field(v, "require_at", false),
            rate_limit: rate_limit_from(v),
            timeout_secs: timeout_secs_from(v),
        }
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

fn bool_field(v: &Value, key: &str, default: bool) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(default)
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

fn rate_limit_from(v: &Value) -> RateLimit {
    let rates = v.get("rate_limit").unwrap_or(&Value::Null);
    RateLimit {
        per_user_per_hour: u32_field(rates, "per_user_per_hour", DEFAULT_USER_RATE),
        per_group_per_hour: u32_field(rates, "per_group_per_hour", DEFAULT_GROUP_RATE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assert_defaults(cfg: &AgentConfig) {
        assert_eq!(cfg.base_url, "https://api.deepseek.com");
        assert_eq!(cfg.model, "deepseek-flash");
        assert_eq!(cfg.api_key_env, "FUJIANG_LLM_KEY");
        assert_eq!(cfg.temperature, 0.7);
        assert_eq!(cfg.max_history_turns, 20);
        assert_eq!(cfg.system_prompt, "你是福酱，西南科技大学 ACM 群助手。");
        assert_eq!(cfg.wake_words, vec!["福酱".to_string()]);
        assert!(!cfg.require_at);
        assert_eq!(cfg.rate_limit.per_user_per_hour, 30);
        assert_eq!(cfg.rate_limit.per_group_per_hour, 120);
        assert_eq!(cfg.timeout_secs, 60);
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
            "enabled": true
        }));
        assert_eq!(cfg.base_url, "https://example.test/v1");
        assert_eq!(cfg.model, "deepseek-chat");
        assert_eq!(cfg.api_key_env, "MY_KEY");
        assert_eq!(cfg.temperature, 0.2);
        assert_eq!(cfg.max_history_turns, 4);
        assert_eq!(cfg.system_prompt, "be brief");
        assert_eq!(cfg.wake_words, vec!["福".to_string(), "bot".to_string()]);
        assert!(cfg.require_at);
        assert_eq!(cfg.rate_limit.per_user_per_hour, 2);
        assert_eq!(cfg.rate_limit.per_group_per_hour, 9);
        assert_eq!(cfg.timeout_secs, 5);
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
            "rate_limit": { "per_user_per_hour": "nope" }
        }));
        assert_defaults(&cfg);
    }

    #[test]
    fn explicit_empty_wake_words_stay_empty() {
        let cfg = AgentConfig::from_value(&json!({ "wake_words": [] }));
        assert!(cfg.wake_words.is_empty());
    }
}
