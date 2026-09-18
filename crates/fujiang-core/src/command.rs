//! Command matching that respects `bot.command_prefix`.
//!
//! Plugins declare commands with a leading `.` (`.luck`, `.remind`). The host
//! rewrites that dot to the live prefix when routing. Handle bodies must use
//! these helpers instead of comparing against a hardcoded `.`.

use serde_json::Value;

use crate::event::{Event, Source};
use crate::plugin::Plugin;

/// `.luck` + prefix `!` → `!luck`. Declarations that do not start with `.` are kept as-is.
pub fn expand(declared: &str, bot_prefix: &str) -> String {
    if let Some(rest) = declared.strip_prefix('.') {
        format!("{bot_prefix}{rest}")
    } else {
        declared.to_string()
    }
}

pub fn first_token(line: &str) -> &str {
    line.trim().split_whitespace().next().unwrap_or("")
}

/// Exact first-token match (`.luck 10` with prefix `!` matches `!luck`).
pub fn matches_token(line: &str, declared: &str, bot_prefix: &str) -> bool {
    first_token(line) == expand(declared, bot_prefix)
}

/// If the first token matches `declared`, return the rest (maybe empty).
pub fn strip_token<'a>(line: &'a str, declared: &str, bot_prefix: &str) -> Option<&'a str> {
    let line = line.trim();
    let want = expand(declared, bot_prefix);
    if line == want {
        return Some("");
    }
    let tok = first_token(line);
    if tok == want {
        Some(line[tok.len()..].trim_start())
    } else {
        None
    }
}

pub fn strip_any<'a>(line: &'a str, declared: &[&str], bot_prefix: &str) -> Option<&'a str> {
    declared
        .iter()
        .find_map(|d| strip_token(line, d, bot_prefix))
}

/// Prefix match for glued commands (`.remind08:30`, `.添加猫`).
pub fn matches_prefix(line: &str, declared: &str, bot_prefix: &str) -> bool {
    let c = expand(declared, bot_prefix);
    !c.is_empty() && line.trim_start().starts_with(&c)
}

pub fn strip_prefix_cmd<'a>(line: &'a str, declared: &str, bot_prefix: &str) -> Option<&'a str> {
    let c = expand(declared, bot_prefix);
    if c.is_empty() {
        return None;
    }
    line.trim_start().strip_prefix(&c)
}

/// Rewrite `.cmd` in help text to the live prefix. Longest declared form first.
pub fn rewrite_help(help: &str, declared: &[&str], bot_prefix: &str) -> String {
    if bot_prefix == "." {
        return help.to_string();
    }
    let mut cmds: Vec<&str> = declared
        .iter()
        .copied()
        .filter(|c| c.starts_with('.'))
        .collect();
    cmds.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    cmds.dedup();
    let mut out = help.to_string();
    for c in cmds {
        out = out.replace(c, &expand(c, bot_prefix));
    }
    out
}

pub fn plugin_help(plugin: &dyn Plugin, bot_prefix: &str) -> String {
    let mut declared: Vec<&str> = plugin.commands().iter().copied().collect();
    declared.extend(plugin.command_prefixes().iter().copied());
    rewrite_help(plugin.help(), &declared, bot_prefix)
}

/// Host-enforced per-plugin ACL. Missing / empty `groups` = all globally allowed groups.
pub fn plugin_sees(cfg: &Value, ev: &Event) -> bool {
    match ev {
        Event::Message(m) => allows_source(cfg, m.source),
        Event::Notice(n) => match n.group_id {
            Some(id) => allows_group(cfg, id),
            None => allows_private(cfg),
        },
        Event::Request(r) => match r.group_id {
            Some(id) => allows_group(cfg, id),
            None => allows_private(cfg),
        },
        Event::Meta { .. } => true,
    }
}

pub fn plugin_priority(cfg: &Value) -> i64 {
    cfg.get("priority").and_then(|x| x.as_i64()).unwrap_or(0)
}

fn allows_source(cfg: &Value, source: Source) -> bool {
    match source {
        Source::Group { id } => allows_group(cfg, id),
        Source::Friend { .. } => allows_private(cfg),
    }
}

fn allows_group(cfg: &Value, id: i64) -> bool {
    let Some(arr) = cfg.get("groups").and_then(|x| x.as_array()) else {
        return true;
    };
    if arr.is_empty() {
        return true;
    }
    arr.iter().any(|v| v.as_i64() == Some(id))
}

fn allows_private(cfg: &Value) -> bool {
    cfg.get("allow_private")
        .and_then(|x| x.as_bool())
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{MessageEvent, Sender};

    #[test]
    fn expand_rewrites_dot() {
        assert_eq!(expand(".luck", "."), ".luck");
        assert_eq!(expand(".luck", "!"), "!luck");
        assert_eq!(expand(".每日一题", "/"), "/每日一题");
        assert_eq!(expand("raw", "!"), "raw");
    }

    #[test]
    fn token_and_prefix() {
        assert!(matches_token("!luck 10", ".luck", "!"));
        assert!(!matches_token(".luck 10", ".luck", "!"));
        assert_eq!(strip_token("!luck 10", ".luck", "!"), Some("10"));
        assert_eq!(strip_token("!luck", ".luck", "!"), Some(""));
        assert_eq!(strip_token("!lucky", ".luck", "!"), None);
        assert_eq!(
            strip_prefix_cmd("!remind08:30", ".remind", "!"),
            Some("08:30")
        );
        assert_eq!(strip_prefix_cmd("!添加猫", ".添加", "!"), Some("猫"));
        assert_eq!(strip_any("!rk -s", &[".rank", ".rk"], "!"), Some("-s"));
    }

    #[test]
    fn help_rewrites_longest_first() {
        let h = rewrite_help(
            ".remindoff 关；.remindHH:MM 开；.cf 最近一场",
            &[".remindoff", ".remind", ".cf"],
            "!",
        );
        assert_eq!(h, "!remindoff 关；!remindHH:MM 开；!cf 最近一场");
    }

    fn msg(source: Source) -> Event {
        Event::Message(MessageEvent {
            id: 1,
            time: 0,
            self_id: 1,
            sender: Sender {
                user_id: 2,
                nickname: None,
                card: None,
            },
            source,
            segments: vec![],
            raw_text: "hi".into(),
        })
    }

    #[test]
    fn acl_groups_and_private() {
        let none = serde_json::json!({});
        assert!(plugin_sees(&none, &msg(Source::Group { id: 1 })));
        assert!(plugin_sees(&none, &msg(Source::Friend { id: 2 })));

        let g = serde_json::json!({ "groups": [10, 20] });
        assert!(plugin_sees(&g, &msg(Source::Group { id: 10 })));
        assert!(!plugin_sees(&g, &msg(Source::Group { id: 3 })));
        assert!(plugin_sees(&g, &msg(Source::Friend { id: 2 })));

        let priv_off = serde_json::json!({ "allow_private": false });
        assert!(!plugin_sees(&priv_off, &msg(Source::Friend { id: 2 })));
        assert!(plugin_sees(&priv_off, &msg(Source::Group { id: 1 })));

        let empty_groups = serde_json::json!({ "groups": [] });
        assert!(plugin_sees(&empty_groups, &msg(Source::Group { id: 99 })));
    }

    #[test]
    fn priority_default() {
        assert_eq!(plugin_priority(&serde_json::json!({})), 0);
        assert_eq!(plugin_priority(&serde_json::json!({ "priority": 7 })), 7);
    }
}
