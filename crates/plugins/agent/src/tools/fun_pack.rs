use serde_json::{json, Map, Value};

pub(crate) const PACK: &str = "fun";
pub(crate) const PACK_DESC: &str = "图库、收藏和学话。打开之后可以查看 tag、收藏夹和当前会话的学话，也可以按原文查找。不能改图库。";

pub(crate) struct FunTool {
    pub name: &'static str,
    pub wire: &'static str,
    pub description: &'static str,
}

const TOOLS: &[FunTool] = &[
    FunTool {
        name: "fun.tags",
        wire: "fun_tags",
        description: "列出图库的 tag，含条数和别名。旧专辑也在这份清单里。不需要参数。还没有 tag 时是空数组。",
    },
    FunTool {
        name: "fun.stars",
        wire: "fun_stars",
        description: "列出收藏夹的名称和链接。不需要参数。还没有收藏时是空数组。",
    },
    FunTool {
        name: "fun.learn",
        wire: "fun_learn",
        description: "列出当前会话的学话。可选字符串 trigger，只返回这个触发词。群里看本群，私聊看全局。还没有时是空数组。",
    },
    FunTool {
        name: "fun.search",
        wire: "fun_search",
        description: "在 tag、收藏和当前会话的学话里按原文查找。字符串参数 q。q 为空时这次调用失败。",
    },
];

pub(crate) fn tools() -> &'static [FunTool] {
    TOOLS
}

pub(crate) fn is_fun_tool(wire: &str) -> bool {
    TOOLS.iter().any(|tool| tool.wire == wire)
}

pub(crate) fn parameters(wire: &str) -> Value {
    let keys: &[&str] = match wire {
        "fun_learn" => &["trigger"],
        "fun_search" => &["q"],
        _ => &[],
    };
    let mut properties = Map::new();
    for key in keys {
        properties.insert((*key).to_string(), json!({"type": "string"}));
    }
    json!({"type": "object", "properties": properties})
}
