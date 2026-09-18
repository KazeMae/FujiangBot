use async_trait::async_trait;
use fujiang_core::{command, BotContext, Event, Flow, Interest, Plugin, PluginScope};
use fujiang_store::{AttachResult, DetachResult};
use tracing::warn;

const HELP: &str = "学话、收藏夹、图库。改学习/图库默认要在 fun.admins 里（名单空=谁都能改）。\n\
【学话】原话触发。本群有条目只用本群的，没有则用导入/私聊里的全局词库。\n\
.learn add <触发词> <回复>   写入当前群或私聊\n\
.learn list [触发词]         列出本群/私聊里的句子\n\
.learn del <触发词> [n]      删全部；带序号只删第 n 条\n\
【收藏】\n\
.star                        列出名称和链接\n\
.star add <名> <url>         新增（重名拒绝）\n\
.star set <名> <url>         新增或覆盖\n\
.star del <名>\n\
【图库】图片和视频共用 tag；摘掉最后一个 tag 也不删文件。\n\
.tag list                    所有 tag、条数、别名\n\
.tag add <tag>               只建空 tag\n\
.tag alias <tag> <别名>      来只别名 也算这个 tag\n\
.tag merge <from> <to>       把 from 上的条目也挂到 to，from 还在\n\
.tag retire <tag>            去掉这个 tag（文件还在）\n\
来只<tag>                    随机一条带该 tag 的图或视频\n\
回复图/视频 + .添加<tag>     给这条挂 tag\n\
回复图/视频 + .删除<tag>     只摘这一个 tag\n\
回复图/视频 + .标签          列出这条的全部 tag";

const FUN_CMDS: &[&str] = &[
    ".learn", ".star", ".tag", ".idea", ".album", ".添加", ".删除", ".标签", ".想法",
];

#[derive(Default)]
pub struct FunPlugin;

#[async_trait]
impl Plugin for FunPlugin {
    fn meta(&self) -> fujiang_core::PluginMeta {
        fujiang_core::PluginMeta::new("fun", "学话、收藏夹、图库", self.commands())
    }

    fn name(&self) -> &'static str {
        "fun"
    }

    fn help(&self) -> &'static str {
        HELP
    }

    fn commands(&self) -> &'static [&'static str] {
        &[
            ".learn", ".star", ".tag", ".idea", ".album", ".添加", ".删除", ".标签", ".想法",
        ]
    }

    fn command_prefixes(&self) -> &'static [&'static str] {
        &[".添加", ".删除"]
    }

    fn interest(&self) -> Interest {
        Interest::Messages
    }

    async fn handle(
        &self,
        ctx: &BotContext,
        ev: &Event,
        _scope: &PluginScope,
    ) -> anyhow::Result<Flow> {
        let Some(msg) = ev.as_message() else {
            return Ok(Flow::Continue);
        };
        let prefix = ctx.bot_config().await.command_prefix;
        let line = msg.command_line();

        if let Some(rest) = command::strip_token(&line, ".learn", &prefix) {
            handle_learn(ctx, msg, rest, &prefix).await?;
            return Ok(Flow::Stop);
        }
        if let Some(rest) = command::strip_token(&line, ".star", &prefix) {
            handle_star(ctx, msg, rest, &prefix).await?;
            return Ok(Flow::Stop);
        }
        if let Some(rest) = command::strip_any(&line, &[".tag", ".idea", ".album"], &prefix) {
            handle_tag(ctx, msg, rest, &prefix).await?;
            return Ok(Flow::Stop);
        }

        if command::matches_token(&line, ".标签", &prefix)
            || command::matches_token(&line, ".想法", &prefix)
        {
            handle_show_tags(ctx, msg).await?;
            return Ok(Flow::Stop);
        }

        if let Some(name) = command::strip_prefix_cmd(&line, ".添加", &prefix) {
            handle_media_cmd(ctx, msg, true, name, &prefix).await?;
            return Ok(Flow::Stop);
        }
        if let Some(name) = command::strip_prefix_cmd(&line, ".删除", &prefix) {
            handle_media_cmd(ctx, msg, false, name, &prefix).await?;
            return Ok(Flow::Stop);
        }

        if let Some(name) = line.strip_prefix("来只") {
            if !name.is_empty() {
                match ctx.store.random_by_tag(name).await? {
                    Some(path) if path.exists() => {
                        let p = path.to_string_lossy();
                        if fujiang_store::is_video_path(&path) {
                            ctx.send_video(msg.source, p).await?;
                        } else {
                            ctx.send_image(msg.source, p).await?;
                        }
                    }
                    _ => {}
                }
                return Ok(Flow::Stop);
            }
        }

        if let Some(reply) = learn_reply(ctx, msg.source.group_id(), &line).await? {
            ctx.reply_text(msg, reply).await?;
            return Ok(Flow::Continue);
        }
        Ok(Flow::Continue)
    }
}

async fn learn_reply(
    ctx: &BotContext,
    group_id: Option<i64>,
    line: &str,
) -> anyhow::Result<Option<String>> {
    if let Some(reply) = ctx.store.learn_random(group_id, line).await? {
        return Ok(Some(reply));
    }
    // 旧 bot 按第一个空格分词匹配，例如「活着？ 啊」仍能中。
    let first = line.split_whitespace().next().unwrap_or("");
    if first.is_empty() || first == line {
        return Ok(None);
    }
    ctx.store.learn_random(group_id, first).await
}

async fn can_mutate(ctx: &BotContext, qq: i64) -> bool {
    let cfg = ctx.bot_config().await;
    cfg.fun_allow_mutate && cfg.is_fun_admin(qq)
}

async fn handle_learn(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
    rest: &str,
    prefix: &str,
) -> anyhow::Result<()> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.is_empty() {
        ctx.reply_text(msg, command::rewrite_help(HELP, FUN_CMDS, prefix))
            .await?;
        return Ok(());
    }
    match parts[0] {
        "add" => {
            if !can_mutate(ctx, msg.user_id()).await {
                ctx.reply_text(msg, "没有权限").await?;
                return Ok(());
            }
            let mut sp = rest.splitn(3, ' ');
            let _ = sp.next();
            let trigger = sp.next();
            let reply = sp.next();
            let (Some(trigger), Some(reply)) = (trigger, reply) else {
                ctx.reply_text(msg, format!("格式：{prefix}learn add <触发词> <回复>"))
                    .await?;
                return Ok(());
            };
            ctx.store
                .learn_add(trigger, reply, msg.user_id(), msg.source.group_id())
                .await?;
            ctx.reply_text(msg, "learned").await?;
        }
        "list" => {
            let trigger = parts.get(1).copied();
            let rows = ctx.store.learn_list(msg.source.group_id(), trigger).await?;
            if rows.is_empty() {
                ctx.reply_text(msg, "空").await?;
                return Ok(());
            }
            let mut s = String::new();
            for (i, r) in rows.iter().enumerate() {
                s.push_str(&format!("{}. [{}] {}\n", i + 1, r.trigger, r.reply));
            }
            ctx.reply_text(msg, s.trim_end()).await?;
        }
        "del" => {
            if !can_mutate(ctx, msg.user_id()).await {
                ctx.reply_text(msg, "没有权限").await?;
                return Ok(());
            }
            if parts.len() < 2 {
                ctx.reply_text(msg, format!("格式：{prefix}learn del <触发词> [n]"))
                    .await?;
                return Ok(());
            }
            let n = parts.get(2).and_then(|x| x.parse().ok());
            let c = ctx
                .store
                .learn_del(msg.source.group_id(), parts[1], n)
                .await?;
            ctx.reply_text(msg, format!("已删除 {c} 条")).await?;
        }
        _ => {
            ctx.reply_text(msg, command::rewrite_help(HELP, FUN_CMDS, prefix))
                .await?;
        }
    }
    Ok(())
}

async fn handle_star(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
    rest: &str,
    prefix: &str,
) -> anyhow::Result<()> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.is_empty() {
        let rows = ctx.store.star_list().await?;
        let mut s = format!("收藏夹：{prefix}star add|set|del");
        for r in rows {
            s.push_str(&format!("\n{} : {}", r.name, r.url));
        }
        ctx.reply_text(msg, s).await?;
        return Ok(());
    }
    match parts[0] {
        "add" if parts.len() == 3 => {
            if !can_mutate(ctx, msg.user_id()).await {
                ctx.reply_text(msg, "没有权限").await?;
                return Ok(());
            }
            if ctx
                .store
                .star_add(parts[1], parts[2], msg.user_id())
                .await?
            {
                ctx.reply_text(msg, "stared").await?;
            } else {
                ctx.reply_text(msg, format!("名称「{}」已存在", parts[1]))
                    .await?;
            }
        }
        "set" if parts.len() == 3 => {
            if !can_mutate(ctx, msg.user_id()).await {
                ctx.reply_text(msg, "没有权限").await?;
                return Ok(());
            }
            ctx.store
                .star_set(parts[1], parts[2], msg.user_id())
                .await?;
            ctx.reply_text(msg, "stared").await?;
        }
        "del" if parts.len() == 2 => {
            if !can_mutate(ctx, msg.user_id()).await {
                ctx.reply_text(msg, "没有权限").await?;
                return Ok(());
            }
            if ctx.store.star_del(parts[1]).await? {
                ctx.reply_text(msg, format!("名称「{}」已删除", parts[1]))
                    .await?;
            } else {
                ctx.reply_text(msg, format!("名称「{}」不存在", parts[1]))
                    .await?;
            }
        }
        _ => {
            ctx.reply_text(msg, format!("格式：{prefix}star add|set|del"))
                .await?;
        }
    }
    Ok(())
}

async fn handle_tag(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
    rest: &str,
    prefix: &str,
) -> anyhow::Result<()> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.is_empty() || parts[0] == "list" {
        let list = ctx.store.tag_list().await?;
        if list.is_empty() {
            ctx.reply_text(msg, "还没有 tag").await?;
            return Ok(());
        }
        let mut s = String::from("tag：");
        for item in list {
            s.push_str(&format!(
                "\n{}  {}条  别名:{:?}",
                item.tag.name, item.image_count, item.aliases
            ));
        }
        ctx.reply_text(msg, s).await?;
        return Ok(());
    }
    if !can_mutate(ctx, msg.user_id()).await {
        ctx.reply_text(msg, "没有权限").await?;
        return Ok(());
    }
    match parts[0] {
        "add" if parts.len() == 2 => {
            ctx.store.tag_ensure(parts[1]).await?;
            ctx.reply_text(msg, "ok").await?;
        }
        "alias" if parts.len() == 3 => {
            let t = ctx.store.tag_alias(parts[1], parts[2]).await?;
            ctx.reply_text(msg, t).await?;
        }
        "merge" if parts.len() == 3 => {
            let t = ctx.store.tag_merge(parts[1], parts[2]).await?;
            ctx.reply_text(msg, t).await?;
        }
        "retire" | "del" if parts.len() == 2 => {
            let ok = ctx.store.tag_retire(parts[1]).await?;
            ctx.reply_text(
                msg,
                if ok {
                    "已去掉这个 tag（文件还在）"
                } else {
                    "不存在"
                },
            )
            .await?;
        }
        _ => {
            ctx.reply_text(
                msg,
                format!("格式：{prefix}tag list|add|alias|merge|retire"),
            )
            .await?;
        }
    }
    Ok(())
}

async fn handle_media_cmd(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
    add: bool,
    name: &str,
    prefix: &str,
) -> anyhow::Result<()> {
    if !can_mutate(ctx, msg.user_id()).await {
        ctx.reply_text(msg, "没有权限").await?;
        return Ok(());
    }
    let name = name.trim();
    if name.is_empty() {
        ctx.reply_text(
            msg,
            format!("格式：回复图片或视频后发 {prefix}添加名 / {prefix}删除名"),
        )
        .await?;
        return Ok(());
    }
    let Some(got) = reply_visual(ctx, msg).await? else {
        return Ok(());
    };
    let kind = if got.video { "视频" } else { "图" };
    if add {
        let img = ctx
            .store
            .image_upsert(&got.bytes, &got.ext, msg.user_id())
            .await?;
        let text = match ctx
            .store
            .image_attach(&img.md5, &name, msg.user_id())
            .await?
        {
            AttachResult::Attached => format!("已挂上 tag「{name}」🤫"),
            AttachResult::Already => format!("这条{kind}已有 tag「{name}」"),
        };
        ctx.reply_text(msg, text).await?;
    } else {
        let md5 = fujiang_store::md5_hex(&got.bytes);
        let text = match ctx.store.image_detach(&md5, &name).await? {
            DetachResult::Detached => format!("已从这条{kind}去掉 tag「{name}」"),
            DetachResult::NoSuchTag => format!("这条{kind}没有 tag「{name}」"),
            DetachResult::UnknownImage => format!("图库里没有这条{kind}"),
        };
        ctx.reply_text(msg, text).await?;
    }
    Ok(())
}

async fn handle_show_tags(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
) -> anyhow::Result<()> {
    let Some(got) = reply_visual(ctx, msg).await? else {
        return Ok(());
    };
    let md5 = fujiang_store::md5_hex(&got.bytes);
    let tags = ctx.store.image_tags(&md5).await?;
    let kind = if got.video { "视频" } else { "图" };
    if tags.is_empty() {
        ctx.reply_text(msg, format!("这条{kind}还没有 tag")).await?;
    } else {
        ctx.reply_text(msg, format!("这条{kind}的 tag：{}", tags.join("、")))
            .await?;
    }
    Ok(())
}

struct QuotedVisual {
    bytes: Vec<u8>,
    ext: String,
    video: bool,
}

async fn reply_visual(
    ctx: &BotContext,
    msg: &fujiang_core::MessageEvent,
) -> anyhow::Result<Option<QuotedVisual>> {
    let Some(rid) = msg.reply_id() else {
        ctx.reply_text(msg, "请先回复一张图片或视频").await?;
        return Ok(None);
    };
    let quoted = ctx.messenger().await.get_message(rid).await?;
    let Some((media, video)) = quoted.first_visual() else {
        ctx.reply_text(msg, "回复的不是图片或视频").await?;
        return Ok(None);
    };
    match ctx.fetch_media_bytes(media).await {
        Ok(bytes) => {
            let hint = if video { Some("mp4") } else { Some("jpg") };
            let ext = fujiang_store::sniff_media_ext(&bytes, hint);
            Ok(Some(QuotedVisual { bytes, ext, video }))
        }
        Err(e) => {
            warn!(error = %e, "fetch media");
            ctx.reply_text(msg, "ERROR😪").await?;
            Ok(None)
        }
    }
}
