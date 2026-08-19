use async_trait::async_trait;
use chrono::{Local, Timelike};
use fujiang_core::{BotContext, Event, Flow, Plugin};
use fujiang_store::DailyProblem;
use rand::seq::SliceRandom;
use rand::{rngs::StdRng, SeedableRng};
use serde::Deserialize;

const TAGS: &str = "binary search\nbitmasks\nbrute force\nchinese remainder theorem\ncombinatorics\nconstructive algorithms\ndata structures\ndfs and similar\ndivide and conquer\ndp\ndsu\nexpression parsing\nfft\nflows\ngames\ngeometry\ngraph matchings\ngraphs\ngreedy\nhashing\nimplementation\ninteractive\nmath\nmatrices\nmeet-in-the-middle\nnumber theory\nprobabilities\nschedules\nshortest paths\nsortings\nstring suffix structures\nstrings\nternary search\ntrees\ntwo pointers\n*special problem\nnot-seen";

const BANDS: &[(&str, &str)] = &[
    ("800-1000", "800 1000 !*special new"),
    ("1000-1200", "1000 1200 !*special new"),
    ("1200-1400", "1200 1400 !*special new"),
    ("1400-1600", "1400 1600 !*special new"),
    ("1600-2000", "1600 2000 !*special new"),
];

pub struct ProblemPlugin;

#[async_trait]
impl Plugin for ProblemPlugin {
    fn name(&self) -> &'static str {
        "problem"
    }

    fn help(&self) -> &'static str {
        "从 Codeforces 题库抽题。\n\
.problem <L> <R> [tags...] [-rt]\n\
        L、R 是 rating 区间，必须是 800～3500 的整百，如 1200 1600\n\
        tags 可写多个，空格用下划线代替（binary_search）\n\
        前加 ! 表示不要这个 tag，如 !dp\n\
        new / !new 限制新/旧场次（contest_id ≥1000）\n\
        默认抽一道；末尾加 -rt 会换一种抽法（可重复试）\n\
.cftag  可用 tag 列表\n\
.每日一题  当天五档：800-1000 / 1000-1200 / 1200-1400 / 1400-1600 / 1600-2000\n\
        过了设定的重置小时（默认 4 点）才换新的一天"
    }

    fn commands(&self) -> &'static [&'static str] {
        &[".problem", ".cftag", ".每日一题"]
    }

    async fn handle(&self, ctx: &BotContext, ev: &Event) -> anyhow::Result<Flow> {
        let Some(msg) = ev.as_message() else {
            return Ok(Flow::Continue);
        };
        let line = msg.command_line();
        if line == ".cftag" {
            ctx.reply_text(msg, TAGS).await?;
            return Ok(Flow::Stop);
        }
        if line == ".每日一题" {
            let text = daily(ctx).await?;
            ctx.reply_text(msg, text).await?;
            return Ok(Flow::Stop);
        }
        if let Some(rest) = line.strip_prefix(".problem") {
            let rest = rest.trim();
            if rest.is_empty() {
                ctx.reply_text(msg, self.help()).await?;
                return Ok(Flow::Stop);
            }
            let text = pick_problem(ctx, rest, true).await?;
            ctx.reply_text(msg, text).await?;
            return Ok(Flow::Stop);
        }
        Ok(Flow::Continue)
    }
}

async fn daily(ctx: &BotContext) -> anyhow::Result<String> {
    let now = Local::now();
    let date = now.date_naive().format("%Y-%m-%d").to_string();
    let reset = ctx.bot_config().await.daily_reset_hour;
    let use_date = if now.hour() < reset {
        (now.date_naive() - chrono::Duration::days(1))
            .format("%Y-%m-%d")
            .to_string()
    } else {
        date.clone()
    };

    let existing = ctx.store.daily_for_date(&use_date).await?;
    if existing.len() >= BANDS.len() {
        return Ok(format_daily(&use_date, &existing));
    }

    let mut rows = Vec::new();
    for (band, spec) in BANDS {
        let url = pick_one(ctx, spec, false)
            .await?
            .unwrap_or_else(|| "没有找到对应的题目哦".into());
        let (contest_id, idx) = parse_url(&url);
        let row = DailyProblem {
            date: use_date.clone(),
            band: (*band).into(),
            contest_id,
            idx,
            url,
        };
        ctx.store.save_daily(&row).await?;
        rows.push(row);
    }
    Ok(format_daily(&use_date, &rows))
}

fn format_daily(date: &str, rows: &[DailyProblem]) -> String {
    let mut s = format!("{date}:");
    for r in rows {
        s.push_str(&format!("\n{}:{}", r.band, r.url));
    }
    s
}

async fn pick_problem(ctx: &BotContext, spec: &str, allow_rt: bool) -> anyhow::Result<String> {
    if spec.is_empty() {
        return Ok("用法：.problem <L> <R> [tags...] [-rt]".into());
    }
    match pick_one(ctx, spec, allow_rt).await? {
        Some(url) => Ok(format!("题目链接：{url}")),
        None => Ok("没有找到对应的题目哦".into()),
    }
}

async fn pick_one(ctx: &BotContext, spec: &str, allow_rt: bool) -> anyhow::Result<Option<String>> {
    let mut args: Vec<String> = spec
        .split_whitespace()
        .map(|s| s.replace('_', " "))
        .collect();
    let mut random_once = true;
    if args.last().map(|s| s.as_str()) == Some("-rt") {
        args.pop();
        random_once = false;
    }
    if !allow_rt {
        random_once = false;
    }
    if args.len() < 2 {
        return Ok(None);
    }
    let l: i64 = args[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("Rating 应该是 800 ~ 3500 的整百数"))?;
    let r: i64 = args[1]
        .parse()
        .map_err(|_| anyhow::anyhow!("Rating 应该是 800 ~ 3500 的整百数"))?;
    if l % 100 != 0 || r % 100 != 0 || !(800..=3500).contains(&l) || !(800..=3500).contains(&r) {
        anyhow::bail!("Rating 应该是 800 ~ 3500 的整百数");
    }
    let tags = &args[2..];
    let mut new_flag = 0i8;
    let mut want: Vec<String> = Vec::new();
    let mut not: Vec<String> = Vec::new();
    for t in tags {
        if t == "new" {
            new_flag = 1;
        } else if t == "!new" {
            new_flag = -1;
        } else if t == "not-seen" {
            continue;
        } else if let Some(rest) = t.strip_prefix('!') {
            not.push(rest.to_string());
        } else {
            want.push(t.clone());
        }
    }

    let mut url = "https://codeforces.com/api/problemset.problems?tags=".to_string();
    for t in &want {
        url.push_str(t);
        url.push(';');
    }
    let body: CfProblems = ctx.http.get(url).send().await?.json().await?;
    let mut hits = Vec::new();
    for p in body.result.problems {
        let Some(rating) = p.rating else { continue };
        let Some(cid) = p.contest_id else { continue };
        if rating < l || rating > r {
            continue;
        }
        if new_flag == 1 && cid < 1000 {
            continue;
        }
        if new_flag == -1 && cid >= 1000 {
            continue;
        }
        if want.iter().any(|t| !p.tags.iter().any(|x| x == t)) {
            continue;
        }
        if not.iter().any(|t| p.tags.iter().any(|x| x == t)) {
            continue;
        }
        hits.push(p);
    }
    if hits.is_empty() {
        return Ok(None);
    }
    let mut rng = if random_once {
        StdRng::from_entropy()
    } else {
        let seed = Local::now().date_naive().format("%Y%m%d").to_string();
        let seed: u64 = seed.parse().unwrap_or(1);
        StdRng::seed_from_u64(seed)
    };
    let p = hits.choose(&mut rng).unwrap();
    Ok(Some(format!(
        "https://codeforces.com/problemset/problem/{}/{}",
        p.contest_id.unwrap_or(0),
        p.index
    )))
}

fn parse_url(url: &str) -> (i64, String) {
    // .../problem/1234/A
    let parts: Vec<&str> = url.rsplit('/').take(2).collect();
    if parts.len() == 2 {
        let idx = parts[0].to_string();
        let cid = parts[1].parse().unwrap_or(0);
        (cid, idx)
    } else {
        (0, String::new())
    }
}

#[derive(Debug, Deserialize)]
struct CfProblems {
    result: CfResult,
}

#[derive(Debug, Deserialize)]
struct CfResult {
    problems: Vec<CfProblem>,
}

#[derive(Debug, Deserialize, Clone)]
struct CfProblem {
    #[serde(rename = "contestId")]
    contest_id: Option<i64>,
    index: String,
    rating: Option<i64>,
    #[serde(default)]
    tags: Vec<String>,
}
