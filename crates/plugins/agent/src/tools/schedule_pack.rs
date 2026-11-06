use serde_json::{json, Map, Value};

use super::qq_read;
use super::{ToolHub, ToolPass};
use crate::schedule::{self, ScheduleGate};

pub(crate) const PACK: &str = "schedule";
pub(crate) const PACK_DESC: &str = "定时发文字。打开之后可以查看、新建、修改和取消自己的任务。私聊只发给创建者本人。到点按创建者重新检查群成员。";

pub(crate) struct JobTool {
    pub name: &'static str,
    pub wire: &'static str,
    pub description: &'static str,
}

const TOOLS: &[JobTool] = &[
    JobTool {
        name: "schedule.list",
        wire: "schedule_list",
        description: "列出自己的定时任务。不需要参数。还没有时是空数组。看不到别人的任务。",
    },
    JobTool {
        name: "schedule.create",
        wire: "schedule_create",
        description: "给自己新建一条定时发文字。kind 是 group 或 private。group 要数字 group_id，并且现在得是该群成员。private 只发给自己，写了别人的 user_id 会失败。时间用字符串 cron 或数字 at，只能填一个。cron 是本地时间的五段。字符串 payload 是正文。可选 action，只能是 send_text。",
    },
    JobTool {
        name: "schedule.update",
        wire: "schedule_update",
        description: "修改自己的一条定时任务。数字 id。可以改 payload、enabled、cron、at、kind、group_id、user_id。改不了别人的任务，也不能把私聊改成别人。",
    },
    JobTool {
        name: "schedule.cancel",
        wire: "schedule_cancel",
        description: "取消自己的一条定时任务。数字 id。别人的任务取消不了。",
    },
];

pub(crate) fn tools() -> &'static [JobTool] {
    TOOLS
}

pub(crate) fn is_job_tool(wire: &str) -> bool {
    TOOLS.iter().any(|tool| tool.wire == wire)
}

pub(crate) fn parameters(wire: &str) -> Value {
    let keys: &[&str] = match wire {
        "schedule_list" => &[],
        "schedule_create" => &[
            "kind", "group_id", "user_id", "cron", "at", "payload", "action",
        ],
        "schedule_update" => &[
            "id", "payload", "enabled", "cron", "at", "kind", "group_id", "user_id", "action",
        ],
        "schedule_cancel" => &["id"],
        _ => &[],
    };
    let mut properties = Map::new();
    for key in keys {
        let ty = match *key {
            "group_id" | "user_id" | "at" | "id" => "integer",
            "enabled" => "boolean",
            _ => "string",
        };
        properties.insert((*key).to_string(), json!({"type": ty}));
    }
    json!({"type": "object", "properties": properties})
}

pub(crate) struct LiveGate<'a, H: ?Sized> {
    pub hub: &'a H,
}

#[async_trait::async_trait]
impl<H> ScheduleGate for LiveGate<'_, H>
where
    H: ToolHub + ?Sized,
{
    async fn group_ok(&self, speaker: i64, group_id: i64) -> anyhow::Result<bool> {
        qq_read::membership(self.hub, speaker, group_id).await
    }

    async fn deliver(&self, kind: &str, target: i64, text: &str) -> anyhow::Result<()> {
        let message = json!([{ "type": "text", "data": { "text": text } }]);
        match kind {
            "group" => {
                self.hub
                    .onebot(
                        "send_group_msg",
                        json!({"group_id": target, "message": message}),
                    )
                    .await?;
            }
            "private" => {
                self.hub
                    .onebot(
                        "send_private_msg",
                        json!({"user_id": target, "message": message}),
                    )
                    .await?;
            }
            _ => anyhow::bail!("bad schedule target"),
        }
        Ok(())
    }
}

pub(crate) async fn handle<H: ToolHub + ?Sized>(
    pass: &ToolPass<'_>,
    hub: &H,
    wire: &str,
    args: &Value,
) -> Result<Value, schedule::Fail> {
    let Some(pool) = pass.memory else {
        return Err(schedule::Fail::Down(anyhow::anyhow!(
            "schedule db is not open"
        )));
    };
    let Some(scope) = pass.scope else {
        return Err(schedule::Fail::Denied);
    };
    schedule::tool_call(
        pool,
        scope.speaker(),
        wire,
        args,
        crate::session::unix_now(),
        &LiveGate { hub },
    )
    .await
}

pub(crate) async fn command<H: ToolHub + ?Sized>(
    pool: &sqlx::SqlitePool,
    speaker: i64,
    cmd: &schedule::HumanCmd,
    now: i64,
    hub: &H,
) -> anyhow::Result<String> {
    schedule::apply_human(pool, speaker, cmd, now, &LiveGate { hub }).await
}

pub(crate) async fn tick<H: ToolHub + ?Sized>(
    pool: &sqlx::SqlitePool,
    hub: &H,
    now: i64,
) -> anyhow::Result<()> {
    schedule::tick(pool, &LiveGate { hub }, now).await
}
