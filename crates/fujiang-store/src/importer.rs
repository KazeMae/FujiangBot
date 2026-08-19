use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json::Value;
use tracing::{info, warn};
use walkdir::WalkDir;

use crate::{AttachResult, CfUser, StandingRow, Store};

/// Import useful JSON / images from the old Python tree. Secrets are skipped.
pub async fn migrate_from_python(store: &Store, from: &Path) -> anyhow::Result<String> {
    let mut report = Vec::new();

    if let Some(n) = import_cf_users(store, &from.join("cf_rank_new.json")).await? {
        report.push(format!("cf_users: {n}"));
    }
    if let Some(n) = import_standings(store, &from.join("contest_rank_new.json")).await? {
        report.push(format!("contest_standings: {n}"));
    }
    if let Some((learn, stars)) = import_learn(store, &from.join("learn.json")).await? {
        report.push(format!("learn_replies: {learn}, stars: {stars}"));
    }
    if let Some(n) = import_reminds(store, &from.join("config.json")).await? {
        report.push(format!("remind_groups: {n}"));
    }
    if let Some(n) = import_gallery(store, from).await? {
        report.push(format!("album images: {n}"));
    }

    if report.is_empty() {
        Ok("没有找到可导入的文件".into())
    } else {
        Ok(report.join("\n"))
    }
}

async fn import_cf_users(store: &Store, path: &Path) -> anyhow::Result<Option<usize>> {
    let Some(v) = read_json(path)? else {
        return Ok(None);
    };
    let arr = v.as_array().cloned().unwrap_or_default();
    let now = chrono::Utc::now().timestamp();
    let mut n = 0usize;
    for u in arr {
        let handle = u
            .get("cf_id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if handle.is_empty() {
            continue;
        }
        let user = CfUser {
            year: as_i64(&u["year"]),
            name: as_str(&u["name"]),
            handle,
            last_rating: as_i64(&u["last_rating"]),
            max_rating: as_i64(&u["max_rating"]),
            solved: as_i64(&u["solved"]),
            last_month: as_i64(&u["last_month"]),
            valid_rating: as_i64(&u["valid_rating"]),
            is_main: as_i64(&u["is_main"]),
            updated_at: now,
            ..CfUser::default()
        };
        store.upsert_cf_user(&user).await?;
        n += 1;
    }
    Ok(Some(n))
}

async fn import_standings(store: &Store, path: &Path) -> anyhow::Result<Option<usize>> {
    let Some(v) = read_json(path)? else {
        return Ok(None);
    };
    let obj = match v.as_object() {
        Some(o) => o,
        None => return Ok(Some(0)),
    };
    let mut n = 0usize;
    for (cid, list) in obj {
        let mut rows = Vec::new();
        if let Some(arr) = list.as_array() {
            for item in arr {
                let name = as_str(&item["name"]);
                let handle = name
                    .rsplit_once(' ')
                    .map(|(_, h)| h.to_string())
                    .unwrap_or_else(|| name.clone());
                rows.push(StandingRow {
                    contest_id: cid.clone(),
                    handle,
                    label: name,
                    rank: as_i64(&item["rank"]),
                    old_rating: as_i64(&item["oldRating"]),
                    new_rating: as_i64(&item["newRating"]),
                });
                n += 1;
            }
        }
        store.replace_standings(cid, &rows).await?;
    }
    Ok(Some(n))
}

async fn import_learn(store: &Store, path: &Path) -> anyhow::Result<Option<(usize, usize)>> {
    let Some(v) = read_json(path)? else {
        return Ok(None);
    };
    let arr = v.as_array().cloned().unwrap_or_default();
    let mut learn_n = 0usize;
    let mut star_n = 0usize;
    if let Some(learn) = arr.first().and_then(|x| x.as_object()) {
        for (k, val) in learn {
            let replies = match val {
                Value::Array(a) => a
                    .iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect(),
                Value::String(s) => vec![s.clone()],
                _ => vec![],
            };
            for r in replies {
                // 旧 JSON 没有群号，记入私聊空间，不会串到群。
                store.learn_add(k, &r, 0, None).await?;
                learn_n += 1;
            }
        }
    }
    if let Some(stars) = arr.last().and_then(|x| x.as_object()) {
        if arr.len() > 1 {
            for (k, val) in stars {
                if let Some(url) = val.as_str() {
                    store.star_set(k, url, 0).await?;
                    star_n += 1;
                }
            }
        }
    }
    Ok(Some((learn_n, star_n)))
}

async fn import_reminds(store: &Store, path: &Path) -> anyhow::Result<Option<usize>> {
    let Some(v) = read_json(path)? else {
        return Ok(None);
    };
    let Some(arr) = v.get("match_catch_remind").and_then(|x| x.as_array()) else {
        return Ok(Some(0));
    };
    let mut n = 0usize;
    for item in arr {
        let gid = as_i64(&item["group"]);
        let hour = as_i64(&item["timeHour"]);
        let min = as_i64(&item["timeMin"]);
        if gid != 0 {
            store.set_remind(gid, hour, min).await?;
            n += 1;
        }
    }
    Ok(Some(n))
}

async fn import_gallery(store: &Store, from: &Path) -> anyhow::Result<Option<usize>> {
    let path_json = from.join("path.json");
    let src = from.join("src");
    if !path_json.exists() && !src.exists() {
        return Ok(None);
    }

    let mut alias_to_rel: HashMap<String, String> = HashMap::new();
    if let Some(v) = read_json(&path_json)? {
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                if let Some(rel) = val.as_str() {
                    alias_to_rel.insert(k.trim_start_matches("来只").to_string(), rel.to_string());
                }
            }
        }
    }

    let mut n = 0usize;
    // Group aliases that share the same relative folder.
    let mut folder_to_names: HashMap<String, Vec<String>> = HashMap::new();
    for (alias, rel) in &alias_to_rel {
        folder_to_names
            .entry(rel.trim_end_matches('/').to_string())
            .or_default()
            .push(alias.clone());
    }

    if folder_to_names.is_empty() && src.exists() {
        for entry in fs::read_dir(&src)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                folder_to_names.insert(name.clone(), vec![name]);
            }
        }
    }

    for (folder, names) in folder_to_names {
        let primary = names.first().cloned().unwrap_or_else(|| folder.clone());
        store.tag_ensure(&primary).await?;
        for alias in &names {
            if alias != &primary {
                store.tag_alias(&primary, alias).await.ok();
            }
        }
        let dir = src.join(&folder);
        if !dir.is_dir() {
            continue;
        }
        for file in WalkDir::new(&dir).max_depth(1).into_iter().flatten() {
            if !file.file_type().is_file() {
                continue;
            }
            let ext = file
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("jpg")
                .to_lowercase();
            if !matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp") {
                continue;
            }
            let bytes = match fs::read(file.path()) {
                Ok(b) => b,
                Err(e) => {
                    warn!(error = %e, path = %file.path().display(), "skip image");
                    continue;
                }
            };
            let img = store.image_upsert(&bytes, &ext, 0).await?;
            match store.image_attach(&img.md5, &primary, 0).await? {
                AttachResult::Attached => n += 1,
                AttachResult::Already => {}
            }
        }
    }
    info!(n, "imported gallery images");
    Ok(Some(n))
}

fn read_json(path: &Path) -> anyhow::Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&text)?))
}

fn as_i64(v: &Value) -> i64 {
    v.as_i64()
        .or_else(|| v.as_u64().map(|x| x as i64))
        .or_else(|| v.as_f64().map(|x| x as i64))
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

fn as_str(v: &Value) -> String {
    v.as_str()
        .map(|s| s.to_string())
        .or_else(|| v.as_i64().map(|n| n.to_string()))
        .unwrap_or_default()
}
