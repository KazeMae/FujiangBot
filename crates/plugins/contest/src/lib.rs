use std::time::Duration;

use async_trait::async_trait;
use chrono::{Local, TimeZone, Timelike};
use fujiang_core::{BotContext, Event, Flow, Plugin, Source};
use fujiang_store::ContestRow;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

const OJS: &[(&str, &str)] = &[
    (".cf", "Codeforces"),
    (".lg", "LuoGu"),
    (".nc", "NowCoder"),
    (".atc", "Atcoder"),
    (".scpc", "SCPC"),
];

#[derive(Default)]
pub struct ContestPlugin {
    stop: CancellationToken,
}

#[async_trait]
impl Plugin for ContestPlugin {
    fn name(&self) -> &'static str {
        "contest"
    }

    fn help(&self) -> &'static str {
        "比赛日历。后台定时拉 Codeforces / 洛谷 / 牛客 / AtCoder / SCPC，开赛前约 1 小时会在已设提醒的群里预告。\n\
.contest          本说明\n\
.cf / .lg / .nc / .atc / .scpc\n\
                  对应 OJ 最近一场（未开始优先）\n\
.cfall / .lgall / .ncall / .atcall / .scpcall\n\
                  该 OJ 当前缓存的全部场次\n\
.day              今天还有哪些比赛\n\
.bot              比赛数据上次刷新时间\n\
.remindHH:MM      仅群聊。每天在这个点推送各 OJ 最近一场，如 .remind08:30\n\
.remindoff        仅群聊。关掉本群每日提醒"
    }

    fn commands(&self) -> &'static [&'static str] {
        &[
            ".contest", ".cf", ".lg", ".nc", ".atc", ".scpc", ".cfall", ".lgall", ".ncall",
            ".atcall", ".scpcall", ".day", ".bot", ".remind",
        ]
    }

    async fn on_start(&self, ctx: &BotContext) -> anyhow::Result<()> {
        let stop = self.stop.clone();
        let refresh_ctx = ctx.clone();
        tokio::spawn(async move {
            loop {
                if stop.is_cancelled() {
                    break;
                }
                if let Err(e) = refresh(&refresh_ctx).await {
                    warn!(error = %e, "contest refresh failed");
                }
                if let Err(e) = pre_remind(&refresh_ctx, &stop).await {
                    warn!(error = %e, "pre-remind failed");
                }
                let minutes = refresh_ctx.bot_config().await.contest_update_minutes.max(5);
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(minutes * 60)) => {}
                }
            }
        });
        let remind_ctx = ctx.clone();
        let stop2 = self.stop.clone();
        tokio::spawn(async move {
            daily_remind_loop(remind_ctx, stop2).await;
        });
        Ok(())
    }

    async fn on_stop(&self) -> anyhow::Result<()> {
        self.stop.cancel();
        Ok(())
    }

    async fn handle(&self, ctx: &BotContext, ev: &Event) -> anyhow::Result<Flow> {
        let Some(msg) = ev.as_message() else {
            return Ok(Flow::Continue);
        };
        let line = msg.command_line();
        if line == ".contest" {
            ctx.reply_text(msg, self.help()).await?;
            return Ok(Flow::Stop);
        }
        if line == ".bot" {
            let t = ctx
                .store
                .get_setting("contest_updated_at")
                .await?
                .unwrap_or_else(|| "尚未更新".into());
            ctx.reply_text(
                msg,
                format!("MatchCatch Version 2.0.0 (Rust)\n上次比赛信息更新时间:{t}"),
            )
            .await?;
            return Ok(Flow::Stop);
        }
        if line == ".day" {
            ctx.reply_text(msg, day_text(ctx).await?).await?;
            return Ok(Flow::Stop);
        }
        if line == ".remindoff" {
            if !msg.source.is_group() {
                ctx.reply_text(msg, "只能在群里关提醒").await?;
                return Ok(Flow::Stop);
            }
            let ok = ctx.store.delete_remind(msg.source.id()).await?;
            ctx.reply_text(
                msg,
                if ok {
                    "已经关闭提醒啦"
                } else {
                    "该群并没有开启提醒哦"
                },
            )
            .await?;
            return Ok(Flow::Stop);
        }
        if let Some(rest) = line.strip_prefix(".remind") {
            if !msg.source.is_group() {
                ctx.reply_text(msg, "只能在群里设提醒").await?;
                return Ok(Flow::Stop);
            }
            if rest.len() != 5 || rest.as_bytes().get(2) != Some(&b':') {
                ctx.reply_text(msg, "别捉弄福酱啦，您的指令键入有误")
                    .await?;
                return Ok(Flow::Stop);
            }
            let hour: i64 = rest[0..2].parse().unwrap_or(-1);
            let min: i64 = rest[3..5].parse().unwrap_or(-1);
            if !(0..=23).contains(&hour) || !(0..=59).contains(&min) {
                ctx.reply_text(msg, "别捉弄福酱啦，您的指令键入有误")
                    .await?;
                return Ok(Flow::Stop);
            }
            ctx.store.set_remind(msg.source.id(), hour, min).await?;
            ctx.reply_text(msg, "已经开启提醒啦").await?;
            return Ok(Flow::Stop);
        }

        for (cmd, oj) in OJS {
            if line == *cmd {
                ctx.reply_text(msg, build_one(ctx, oj, 0).await?).await?;
                return Ok(Flow::Stop);
            }
            if line == format!("{cmd}all") {
                ctx.reply_text(msg, build_all(ctx, oj).await?).await?;
                return Ok(Flow::Stop);
            }
        }
        Ok(Flow::Continue)
    }
}

async fn refresh(ctx: &BotContext) -> anyhow::Result<()> {
    info!("refreshing contests");
    let now = chrono::Utc::now().timestamp();
    let mut items = fetch_clist(ctx).await.unwrap_or_default();
    items.extend(fetch_alg(ctx).await.unwrap_or_default());
    items.extend(fetch_scpc(ctx).await.unwrap_or_default());
    use std::collections::HashMap;
    let mut by: HashMap<String, Vec<ContestRow>> = HashMap::new();
    for mut c in items {
        c.updated_at = now;
        by.entry(c.oj.clone()).or_default().push(c);
    }
    for (oj, list) in by {
        ctx.store.replace_contests(&oj, &list).await?;
    }
    let stamp = Local::now().format("%Y年%m月%d日%H时%M分%S秒").to_string();
    ctx.store.set_setting("contest_updated_at", &stamp).await?;
    info!("contests updated");
    Ok(())
}

async fn fetch_clist(ctx: &BotContext) -> anyhow::Result<Vec<ContestRow>> {
    let cfg = ctx.bot_config().await;
    if cfg.clist_username.is_empty() || cfg.clist_api_key.is_empty() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for (host, oj) in [("codeforces.com", "Codeforces"), ("atcoder.jp", "Atcoder")] {
        let today = Local::now().format("%Y-%m-%d").to_string();
        let url = format!(
            "https://clist.by/api/v3/contest/?username={}&api_key={}&host={}&start__gt={}T00:00:00&order_by=start&limit={}&format=json",
            cfg.clist_username,
            cfg.clist_api_key,
            host,
            today,
            cfg.clist_limit
        );
        let body: ClistResp = ctx.http.get(url).send().await?.json().await?;
        for o in body.objects {
            if !keep(oj, &o.event) {
                continue;
            }
            out.push(ContestRow {
                id: 0,
                oj: oj.into(),
                title: o.event,
                begin_ts: parse_utc(&o.start),
                end_ts: parse_utc(&o.end),
                url: o.href,
                source: "clist".into(),
                updated_at: 0,
            });
        }
    }
    Ok(out)
}

async fn fetch_alg(ctx: &BotContext) -> anyhow::Result<Vec<ContestRow>> {
    let arr: Vec<AlgItem> = ctx
        .http
        .get("https://algcontest.rainng.com/")
        .send()
        .await?
        .json()
        .await?;
    let mut out = Vec::new();
    for it in arr {
        if it.oj != "LuoGu" && it.oj != "NowCoder" {
            continue;
        }
        if !keep(&it.oj, &it.name) {
            continue;
        }
        out.push(ContestRow {
            id: 0,
            oj: it.oj,
            title: it.name,
            begin_ts: parse_local(&it.start_time),
            end_ts: parse_local(&it.end_time),
            url: it.link,
            source: "algcontest".into(),
            updated_at: 0,
        });
    }
    Ok(out)
}

async fn fetch_scpc(ctx: &BotContext) -> anyhow::Result<Vec<ContestRow>> {
    let body: serde_json::Value = ctx
        .http
        .get("http://scpc.fun/api/get-contest-list?status=-1")
        .send()
        .await?
        .json()
        .await?;
    let mut out = Vec::new();
    if let Some(arr) = body.pointer("/data/records").and_then(|v| v.as_array()) {
        for it in arr {
            let title = it
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let id = it.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            let start = it
                .get("startTime")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .split('.')
                .next()
                .unwrap_or("");
            let end = it
                .get("endTime")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .split('.')
                .next()
                .unwrap_or("");
            out.push(ContestRow {
                id: 0,
                oj: "SCPC".into(),
                title,
                begin_ts: parse_utc(start),
                end_ts: parse_utc(end),
                url: format!("http://scpc.fun/contest/{id}"),
                source: "scpc".into(),
                updated_at: 0,
            });
        }
    }
    Ok(out)
}

fn keep(oj: &str, title: &str) -> bool {
    match oj {
        "Codeforces" => !title.contains("(Div. 1)") && !title.contains("(Rated for Div. 1)"),
        "NowCoder" => title.contains('赛') || title.contains("集训营"),
        "Atcoder" => {
            title.contains("AtCoder Beginner Contest") || title.contains("AtCoder Regular Contest")
        }
        _ => true,
    }
}

fn parse_utc(s: &str) -> i64 {
    // 2026-08-19T12:00:00
    let s = s.replace('T', " ");
    chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .and_then(|n| {
            chrono::Utc
                .from_utc_datetime(&n)
                .checked_add_signed(chrono::Duration::hours(8))
        })
        .map(|t| t.timestamp())
        .unwrap_or_else(|| parse_local(&s))
}

fn parse_local(s: &str) -> i64 {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .and_then(|n| Local.from_local_datetime(&n).single())
        .map(|t| t.timestamp())
        .unwrap_or(0)
}

async fn build_one(ctx: &BotContext, oj: &str, pos: usize) -> anyhow::Result<String> {
    let list = ctx.store.contests_by_oj(oj).await?;
    if let Some(c) = list.get(pos) {
        Ok(format!(
            "近期{oj}的比赛为{}\n比赛开始时间:{}\n比赛结束时间:{}\n比赛链接:{}\n要好好加油打比赛,不准偷懒哦,不然就只能打铁了",
            c.title,
            fmt_ts(c.begin_ts),
            fmt_ts(c.end_ts),
            c.url
        ))
    } else {
        Ok(format!("知道你想打, 但是近期没有 {oj} 的比赛呢"))
    }
}

async fn build_all(ctx: &BotContext, oj: &str) -> anyhow::Result<String> {
    let list = ctx.store.contests_by_oj(oj).await?;
    if list.is_empty() {
        return Ok(format!("知道你想打, 但是近期没有 {oj} 的比赛呢"));
    }
    let mut s = String::new();
    for (i, _) in list.iter().enumerate() {
        s.push_str(&build_one(ctx, oj, i).await?);
        s.push('\n');
    }
    Ok(s)
}

async fn day_text(ctx: &BotContext) -> anyhow::Result<String> {
    let start = Local::now().date_naive().and_hms_opt(0, 0, 0).unwrap();
    let start_ts = Local
        .from_local_datetime(&start)
        .single()
        .unwrap()
        .timestamp();
    let rows = ctx.store.contests_today(start_ts, start_ts + 86400).await?;
    if rows.is_empty() {
        return Ok("知道你想打，但是今天没有比赛哦".into());
    }
    let mut s = String::from("今天的比赛如下:\n");
    let mut cur = "";
    for c in &rows {
        if c.oj != cur {
            s.push_str(&format!("{}比赛:\n", c.oj));
            cur = &c.oj;
        }
        s.push_str(&format!(
            "{}\n比赛开始时间:{}\n比赛结束时间:{}\n比赛链接:{}\n",
            c.title,
            fmt_ts(c.begin_ts),
            fmt_ts(c.end_ts),
            c.url
        ));
    }
    Ok(s)
}

fn fmt_ts(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| ts.to_string())
}

async fn pre_remind(ctx: &BotContext, stop: &CancellationToken) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    let groups = ctx.store.remind_groups().await?;
    if groups.is_empty() {
        return Ok(());
    }
    let all = ctx.store.contests_all().await?;
    for c in all {
        let delta = c.begin_ts - now;
        if (3600..=12 * 3600).contains(&delta) {
            let key = format!("preremind:{}", c.url);
            if ctx.store.get_setting(&key).await?.is_some() {
                continue;
            }
            let msg = format!(
                "比赛 {} 只有一个小时就要开始啦!\n比赛开始时间:{}\n比赛结束时间:{}\n比赛链接:{}\n要好好加油打比赛,不准偷懒哦,不然就只能打铁了",
                c.title,
                fmt_ts(c.begin_ts),
                fmt_ts(c.end_ts),
                c.url
            );
            let wait = (delta - 3600).max(0) as u64;
            let spawn_ctx = ctx.clone();
            let groups = groups.clone();
            let stop = stop.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(wait)) => {}
                }
                for g in groups {
                    let _ = spawn_ctx
                        .send_text(Source::Group { id: g.group_id }, msg.clone())
                        .await;
                }
            });
            ctx.store.set_setting(&key, "1").await?;
        }
    }
    Ok(())
}

async fn daily_remind_loop(ctx: BotContext, stop: CancellationToken) {
    loop {
        if stop.is_cancelled() {
            break;
        }
        let groups = ctx.store.remind_groups().await.unwrap_or_default();
        let now = Local::now();
        for g in groups {
            if now.hour() as i64 == g.hour && now.minute() as i64 == g.min {
                let mut msg = String::new();
                for (_, oj) in OJS {
                    if let Ok(t) = build_one(&ctx, oj, 0).await {
                        msg.push_str(&t);
                        msg.push('\n');
                    }
                }
                if !msg.is_empty() {
                    let _ = ctx.send_text(Source::Group { id: g.group_id }, msg).await;
                }
            }
        }
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
        }
    }
}

#[derive(Deserialize)]
struct ClistResp {
    #[serde(default)]
    objects: Vec<ClistObj>,
}

#[derive(Deserialize)]
struct ClistObj {
    event: String,
    start: String,
    end: String,
    href: String,
}

#[derive(Deserialize)]
struct AlgItem {
    oj: String,
    name: String,
    #[serde(rename = "startTime")]
    start_time: String,
    #[serde(rename = "endTime")]
    end_time: String,
    link: String,
}
