use serde_json::{json, Map, Value};

pub(crate) const PACK: &str = "contest";
pub(crate) const PACK_DESC: &str = "比赛日程。打开之后可以查看即将开始的比赛，以及今天开始的比赛。";

pub(crate) struct ContestTool {
    pub name: &'static str,
    pub wire: &'static str,
    pub description: &'static str,
}

const TOOLS: &[ContestTool] = &[
    ContestTool {
        name: "contest.upcoming",
        wire: "contest_upcoming",
        description:
            "列出即将开始的比赛。可选字符串参数 oj，例如 Codeforces 或 Atcoder。不传则返回全部。",
    },
    ContestTool {
        name: "contest.today",
        wire: "contest_today",
        description: "列出今天开始的比赛。不需要参数。",
    },
];

pub(crate) fn tools() -> &'static [ContestTool] {
    TOOLS
}

pub(crate) fn is_contest_tool(wire: &str) -> bool {
    TOOLS.iter().any(|tool| tool.wire == wire)
}

pub(crate) fn parameters(wire: &str) -> Value {
    let keys: &[&str] = match wire {
        "contest_upcoming" => &["oj"],
        _ => &[],
    };
    let mut properties = Map::new();
    for key in keys {
        properties.insert((*key).to_string(), json!({"type": "string"}));
    }
    json!({"type": "object", "properties": properties})
}
