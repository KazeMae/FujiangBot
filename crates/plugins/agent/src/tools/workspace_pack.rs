use serde_json::{json, Value};

use super::ToolSpec;

const BASH_DESC: &str = "在宿主已经绑定的当前工作区里执行脚本。绑定只由这一轮真实会话决定，调用参数不能改 owner、路径、QQ 或群。必填字符串 script。可选字符串 lang 只能是 bash 或 sh，省略时是 bash。其它字段拒绝。";
const CODE_DESC: &str = "在宿主已经绑定的当前工作区里执行 Python。绑定只由这一轮真实会话决定，调用参数不能改 owner、路径、QQ 或群。必填字符串 script。必填字符串 lang 只能是 python。其它字段拒绝。";

pub(crate) fn specs() -> [ToolSpec; 2] {
    [
        ToolSpec::parse("workspace.bash", BASH_DESC, bash_schema()).expect("workspace.bash"),
        ToolSpec::parse("workspace.code", CODE_DESC, code_schema()).expect("workspace.code"),
    ]
}

pub(crate) fn reserved(spec: &ToolSpec) -> bool {
    crate::workspace::is_exec_wire(spec.wire_name()) || spec.service() == "workspace"
}

pub(crate) fn production_execute(
    owner: crate::workspace::WorkspaceOwner,
    payload: crate::workspace_runner::Payload,
) -> anyhow::Result<crate::workspace_runner::RunResult> {
    crate::workspace_backend::execute(owner, payload)
}

pub(crate) async fn dispatch<E>(
    pool: Option<&sqlx::SqlitePool>,
    turn: Option<crate::workspace::HostTurn>,
    wire: &str,
    args: &Value,
    execute: &mut E,
) -> Result<Value, ()>
where
    E: FnMut(
        crate::workspace::WorkspaceOwner,
        crate::workspace_runner::Payload,
    ) -> anyhow::Result<crate::workspace_runner::RunResult>,
{
    crate::workspace::handle_tool_with(pool, turn, wire, args, |owner, payload| {
        execute(owner, payload)
    })
    .await
}

fn bash_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["script"],
        "properties": {
            "script": { "type": "string" },
            "lang": { "type": "string", "enum": ["bash", "sh"] }
        }
    })
}

fn code_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["script", "lang"],
        "properties": {
            "script": { "type": "string" },
            "lang": { "type": "string", "enum": ["python"] }
        }
    })
}
