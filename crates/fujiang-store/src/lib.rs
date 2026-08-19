mod ideas;
mod importer;
mod messages;
mod models;

pub use importer::migrate_from_python;
pub use messages::{default_archive_dir, shard_table, ArchiveReport};
pub use models::*;

use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
    pub image_root: PathBuf,
    archive_dir: PathBuf,
    archive_after_days: u64,
}

pub struct StoreOpts {
    pub db: String,
    pub image_root: PathBuf,
    pub archive_dir: PathBuf,
    pub archive_after_days: u64,
}

impl Store {
    pub async fn open(db: &str, image_root: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let image_root = image_root.into();
        let archive_dir = messages::default_archive_dir(&image_root);
        Self::open_opts(StoreOpts {
            db: db.into(),
            image_root,
            archive_dir,
            archive_after_days: 30,
        })
        .await
    }

    pub async fn open_opts(opts: StoreOpts) -> anyhow::Result<Self> {
        if let Some(parent) = Path::new(&opts.db).parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::create_dir_all(&opts.image_root).await.ok();
        tokio::fs::create_dir_all(&opts.archive_dir).await.ok();

        let connect = SqliteConnectOptions::new()
            .filename(&opts.db)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(connect)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        let store = Self {
            pool,
            image_root: opts.image_root,
            archive_dir: opts.archive_dir,
            archive_after_days: opts.archive_after_days,
        };
        store.split_legacy_messages().await?;
        store.migrate_legacy_albums().await?;
        Ok(store)
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn get_setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        let row = sqlx::query("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<String, _>(0)))
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO settings(key, value) VALUES(?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // --- contests ---

    pub async fn replace_contests(&self, oj: &str, items: &[ContestRow]) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM contests WHERE oj = ?")
            .bind(oj)
            .execute(&mut *tx)
            .await?;
        for c in items {
            sqlx::query(
                "INSERT INTO contests(oj, title, begin_ts, end_ts, url, source, updated_at)
                 VALUES(?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&c.oj)
            .bind(&c.title)
            .bind(c.begin_ts)
            .bind(c.end_ts)
            .bind(&c.url)
            .bind(&c.source)
            .bind(c.updated_at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn contests_by_oj(&self, oj: &str) -> anyhow::Result<Vec<ContestRow>> {
        let rows = sqlx::query_as::<_, ContestRow>(
            "SELECT id, oj, title, begin_ts, end_ts, url, source, updated_at
             FROM contests WHERE oj = ? ORDER BY begin_ts ASC",
        )
        .bind(oj)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn contests_all(&self) -> anyhow::Result<Vec<ContestRow>> {
        Ok(sqlx::query_as::<_, ContestRow>(
            "SELECT id, oj, title, begin_ts, end_ts, url, source, updated_at
             FROM contests ORDER BY begin_ts ASC",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn contests_today(
        &self,
        start_ts: i64,
        end_ts: i64,
    ) -> anyhow::Result<Vec<ContestRow>> {
        Ok(sqlx::query_as::<_, ContestRow>(
            "SELECT id, oj, title, begin_ts, end_ts, url, source, updated_at
             FROM contests WHERE begin_ts >= ? AND begin_ts < ? ORDER BY begin_ts ASC",
        )
        .bind(start_ts)
        .bind(end_ts)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn remind_groups(&self) -> anyhow::Result<Vec<RemindGroup>> {
        Ok(
            sqlx::query_as::<_, RemindGroup>("SELECT group_id, hour, min FROM remind_groups")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn set_remind(&self, group_id: i64, hour: i64, min: i64) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO remind_groups(group_id, hour, min) VALUES(?, ?, ?)
             ON CONFLICT(group_id) DO UPDATE SET hour = excluded.hour, min = excluded.min",
        )
        .bind(group_id)
        .bind(hour)
        .bind(min)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_remind(&self, group_id: i64) -> anyhow::Result<bool> {
        let r = sqlx::query("DELETE FROM remind_groups WHERE group_id = ?")
            .bind(group_id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    // --- rank ---

    pub async fn cf_users(&self) -> anyhow::Result<Vec<CfUser>> {
        Ok(sqlx::query_as::<_, CfUser>(
            "SELECT id, year, name, handle, last_rating, max_rating, solved, last_month, valid_rating, is_main, updated_at
             FROM cf_users ORDER BY year DESC, name ASC",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn cf_users_by_year(&self, year: i64) -> anyhow::Result<Vec<CfUser>> {
        Ok(sqlx::query_as::<_, CfUser>(
            "SELECT id, year, name, handle, last_rating, max_rating, solved, last_month, valid_rating, is_main, updated_at
             FROM cf_users WHERE year = ? ORDER BY name ASC",
        )
        .bind(year)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn cf_users_by_name(&self, year: i64, name: &str) -> anyhow::Result<Vec<CfUser>> {
        Ok(sqlx::query_as::<_, CfUser>(
            "SELECT id, year, name, handle, last_rating, max_rating, solved, last_month, valid_rating, is_main, updated_at
             FROM cf_users WHERE year = ? AND name = ?",
        )
        .bind(year)
        .bind(name)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn upsert_cf_user(&self, u: &CfUser) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO cf_users(year, name, handle, last_rating, max_rating, solved, last_month, valid_rating, is_main, updated_at)
             VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(handle) DO UPDATE SET
                year=excluded.year, name=excluded.name,
                last_rating=excluded.last_rating, max_rating=excluded.max_rating,
                solved=excluded.solved, last_month=excluded.last_month,
                valid_rating=excluded.valid_rating, is_main=excluded.is_main,
                updated_at=excluded.updated_at",
        )
        .bind(u.year)
        .bind(&u.name)
        .bind(&u.handle)
        .bind(u.last_rating)
        .bind(u.max_rating)
        .bind(u.solved)
        .bind(u.last_month)
        .bind(u.valid_rating)
        .bind(u.is_main)
        .bind(u.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_cf_user(&self, handle: &str) -> anyhow::Result<bool> {
        let r = sqlx::query("DELETE FROM cf_users WHERE handle = ?")
            .bind(handle)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn handle_exists(&self, handle: &str) -> anyhow::Result<bool> {
        let row = sqlx::query("SELECT 1 FROM cf_users WHERE handle = ?")
            .bind(handle)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    pub async fn replace_standings(
        &self,
        contest_id: &str,
        rows: &[StandingRow],
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        for s in rows {
            sqlx::query(
                "INSERT INTO contest_standings(contest_id, handle, label, rank, old_rating, new_rating)
                 VALUES(?, ?, ?, ?, ?, ?)
                 ON CONFLICT(contest_id, handle) DO UPDATE SET
                    label=excluded.label, rank=excluded.rank,
                    old_rating=excluded.old_rating, new_rating=excluded.new_rating",
            )
            .bind(contest_id)
            .bind(&s.handle)
            .bind(&s.label)
            .bind(s.rank)
            .bind(s.old_rating)
            .bind(s.new_rating)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn standings(&self, contest_id: &str) -> anyhow::Result<Vec<StandingRow>> {
        Ok(sqlx::query_as::<_, StandingRow>(
            "SELECT contest_id, handle, label, rank, old_rating, new_rating
             FROM contest_standings WHERE contest_id = ? ORDER BY rank ASC",
        )
        .bind(contest_id)
        .fetch_all(&self.pool)
        .await?)
    }

    // --- daily problems ---

    pub async fn daily_for_date(&self, date: &str) -> anyhow::Result<Vec<DailyProblem>> {
        Ok(sqlx::query_as::<_, DailyProblem>(
            "SELECT date, band, contest_id, idx, url FROM daily_problems WHERE date = ?",
        )
        .bind(date)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn save_daily(&self, p: &DailyProblem) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO daily_problems(date, band, contest_id, idx, url)
             VALUES(?, ?, ?, ?, ?)
             ON CONFLICT(date, band) DO UPDATE SET
                contest_id=excluded.contest_id, idx=excluded.idx, url=excluded.url",
        )
        .bind(&p.date)
        .bind(&p.band)
        .bind(p.contest_id)
        .bind(&p.idx)
        .bind(&p.url)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // --- learn ---

    pub async fn learn_add(
        &self,
        trigger: &str,
        reply: &str,
        created_by: i64,
        group_id: Option<i64>,
    ) -> anyhow::Result<i64> {
        let now = chrono::Utc::now().timestamp();
        let r = sqlx::query(
            "INSERT INTO learn_replies(trigger, reply, created_by, created_at, group_id)
             VALUES(?, ?, ?, ?, ?)",
        )
        .bind(trigger)
        .bind(reply)
        .bind(created_by)
        .bind(now)
        .bind(group_id)
        .execute(&self.pool)
        .await?;
        Ok(r.last_insert_rowid())
    }

    pub async fn learn_list(
        &self,
        group_id: Option<i64>,
        trigger: Option<&str>,
    ) -> anyhow::Result<Vec<LearnReply>> {
        let sql = match (group_id, trigger) {
            (Some(_), Some(_)) => {
                "SELECT id, trigger, reply, created_by, created_at, group_id
                 FROM learn_replies WHERE group_id = ? AND trigger = ? ORDER BY id"
            }
            (None, Some(_)) => {
                "SELECT id, trigger, reply, created_by, created_at, group_id
                 FROM learn_replies WHERE group_id IS NULL AND trigger = ? ORDER BY id"
            }
            (Some(_), None) => {
                "SELECT id, trigger, reply, created_by, created_at, group_id
                 FROM learn_replies WHERE group_id = ? ORDER BY trigger, id"
            }
            (None, None) => {
                "SELECT id, trigger, reply, created_by, created_at, group_id
                 FROM learn_replies WHERE group_id IS NULL ORDER BY trigger, id"
            }
        };
        let mut q = sqlx::query_as::<_, LearnReply>(sql);
        if let Some(gid) = group_id {
            q = q.bind(gid);
        }
        if let Some(t) = trigger {
            q = q.bind(t);
        }
        Ok(q.fetch_all(&self.pool).await?)
    }

    pub async fn learn_del(
        &self,
        group_id: Option<i64>,
        trigger: &str,
        n: Option<i64>,
    ) -> anyhow::Result<u64> {
        if let Some(n) = n {
            let rows = self.learn_list(group_id, Some(trigger)).await?;
            let idx = (n - 1) as usize;
            if let Some(row) = rows.get(idx) {
                let r = sqlx::query("DELETE FROM learn_replies WHERE id = ?")
                    .bind(row.id)
                    .execute(&self.pool)
                    .await?;
                return Ok(r.rows_affected());
            }
            return Ok(0);
        }
        let r = if let Some(gid) = group_id {
            sqlx::query("DELETE FROM learn_replies WHERE group_id = ? AND trigger = ?")
                .bind(gid)
                .bind(trigger)
                .execute(&self.pool)
                .await?
        } else {
            sqlx::query("DELETE FROM learn_replies WHERE group_id IS NULL AND trigger = ?")
                .bind(trigger)
                .execute(&self.pool)
                .await?
        };
        Ok(r.rows_affected())
    }

    pub async fn learn_random(
        &self,
        group_id: Option<i64>,
        trigger: &str,
    ) -> anyhow::Result<Option<String>> {
        let row = if let Some(gid) = group_id {
            sqlx::query(
                "SELECT reply FROM learn_replies
                 WHERE group_id = ? AND trigger = ? ORDER BY RANDOM() LIMIT 1",
            )
            .bind(gid)
            .bind(trigger)
            .fetch_optional(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT reply FROM learn_replies
                 WHERE group_id IS NULL AND trigger = ? ORDER BY RANDOM() LIMIT 1",
            )
            .bind(trigger)
            .fetch_optional(&self.pool)
            .await?
        };
        Ok(row.map(|r| r.get::<String, _>(0)))
    }

    // --- stars ---

    pub async fn star_list(&self) -> anyhow::Result<Vec<Star>> {
        Ok(sqlx::query_as::<_, Star>(
            "SELECT id, name, url, created_by, updated_at FROM stars ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn star_add(&self, name: &str, url: &str, by: i64) -> anyhow::Result<bool> {
        let now = chrono::Utc::now().timestamp();
        let r = sqlx::query(
            "INSERT OR IGNORE INTO stars(name, url, created_by, updated_at) VALUES(?, ?, ?, ?)",
        )
        .bind(name)
        .bind(url)
        .bind(by)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn star_set(&self, name: &str, url: &str, by: i64) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO stars(name, url, created_by, updated_at) VALUES(?, ?, ?, ?)
             ON CONFLICT(name) DO UPDATE SET url=excluded.url, created_by=excluded.created_by, updated_at=excluded.updated_at",
        )
        .bind(name)
        .bind(url)
        .bind(by)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn star_del(&self, name: &str) -> anyhow::Result<bool> {
        let r = sqlx::query("DELETE FROM stars WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }
}

pub fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn open_tmp() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("fujiang-test-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let store = Store::open(db.to_str().unwrap(), dir.join("img"))
            .await
            .unwrap();
        (store, dir)
    }

    #[tokio::test]
    async fn learn_roundtrip() {
        let (store, dir) = open_tmp().await;
        store.learn_add("hi", "hello", 1, None).await.unwrap();
        store.learn_add("hi", "hey", 1, None).await.unwrap();
        let list = store.learn_list(None, Some("hi")).await.unwrap();
        assert_eq!(list.len(), 2);
        let n = store.learn_del(None, "hi", Some(1)).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.learn_list(None, Some("hi")).await.unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn learn_isolated_by_group() {
        let (store, dir) = open_tmp().await;
        store.learn_add("hi", "g1", 1, Some(11)).await.unwrap();
        store.learn_add("hi", "g2", 1, Some(22)).await.unwrap();
        store.learn_add("hi", "pm", 1, None).await.unwrap();

        assert_eq!(
            store.learn_random(Some(11), "hi").await.unwrap().as_deref(),
            Some("g1")
        );
        assert_eq!(
            store.learn_random(Some(22), "hi").await.unwrap().as_deref(),
            Some("g2")
        );
        assert_eq!(
            store.learn_random(None, "hi").await.unwrap().as_deref(),
            Some("pm")
        );
        assert_eq!(store.learn_list(Some(11), None).await.unwrap().len(), 1);
        assert_eq!(store.learn_del(Some(11), "hi", None).await.unwrap(), 1);
        assert!(store.learn_random(Some(11), "hi").await.unwrap().is_none());
        assert_eq!(
            store.learn_random(Some(22), "hi").await.unwrap().as_deref(),
            Some("g2")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn image_multi_idea() {
        let (store, dir) = open_tmp().await;
        let bytes = b"fake-jpeg-bytes";
        let img = store.image_upsert(bytes, "jpg", 1).await.unwrap();
        assert_eq!(
            store.image_attach(&img.md5, "福哥", 1).await.unwrap(),
            AttachResult::Attached
        );
        assert_eq!(
            store.image_attach(&img.md5, "合照", 1).await.unwrap(),
            AttachResult::Attached
        );
        assert_eq!(
            store.image_attach(&img.md5, "福哥", 1).await.unwrap(),
            AttachResult::Already
        );
        assert_eq!(
            store.image_ideas(&img.md5).await.unwrap(),
            vec!["合照", "福哥"]
        );
        assert!(store.random_by_idea("福哥").await.unwrap().is_some());
        assert!(store.random_by_idea("合照").await.unwrap().is_some());

        assert_eq!(
            store.image_detach(&img.md5, "福哥").await.unwrap(),
            DetachResult::Detached
        );
        assert!(store.random_by_idea("福哥").await.unwrap().is_none());
        assert!(store.random_by_idea("合照").await.unwrap().is_some());
        assert!(store.image_root.join(&img.rel_path).exists());

        store.idea_merge("合照", "团建").await.unwrap();
        assert!(store.random_by_idea("合照").await.unwrap().is_some());
        assert!(store.random_by_idea("团建").await.unwrap().is_some());

        store.idea_retire("合照").await.unwrap();
        assert!(store.random_by_idea("合照").await.unwrap().is_none());
        assert!(store.random_by_idea("团建").await.unwrap().is_some());
        assert!(store.image_root.join(&img.rel_path).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn persist_message() {
        let (store, dir) = open_tmp().await;
        store
            .insert_message(&MessageLog {
                message_id: 42,
                time: 1,
                self_id: 9,
                user_id: 2,
                group_id: Some(100),
                nickname: Some("a".into()),
                card: None,
                raw_text: "hello".into(),
                segments_json: r#"[{"Text":{"text":"hello"}}]"#.into(),
            })
            .await
            .unwrap();
        store
            .insert_message(&MessageLog {
                message_id: 42,
                time: 1,
                self_id: 9,
                user_id: 2,
                group_id: Some(100),
                nickname: None,
                card: None,
                raw_text: "dup".into(),
                segments_json: "[]".into(),
            })
            .await
            .unwrap();
        let n = store.count_in_shard("msg_g100").await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.count_in_shard("msg_pm").await.unwrap_or(0), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn archive_old_messages() {
        let dir = std::env::temp_dir().join(format!("fujiang-test-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open_opts(StoreOpts {
            db: dir.join("t.db").to_string_lossy().into(),
            image_root: dir.join("img"),
            archive_dir: dir.join("archive"),
            archive_after_days: 1,
        })
        .await
        .unwrap();
        store
            .insert_message(&MessageLog {
                message_id: 7,
                time: 1,
                self_id: 9,
                user_id: 2,
                group_id: Some(55),
                nickname: None,
                card: None,
                raw_text: "old".into(),
                segments_json: "[]".into(),
            })
            .await
            .unwrap();
        store
            .insert_message(&MessageLog {
                message_id: 8,
                time: chrono::Utc::now().timestamp(),
                self_id: 9,
                user_id: 2,
                group_id: Some(55),
                nickname: None,
                card: None,
                raw_text: "new".into(),
                segments_json: "[]".into(),
            })
            .await
            .unwrap();
        let report = store.archive_due().await.unwrap();
        assert_eq!(report.moved, 1);
        assert_eq!(store.count_in_shard("msg_g55").await.unwrap(), 1);
        assert!(!report.files.is_empty());
        assert!(std::path::Path::new(&report.files[0]).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn uuid_like() -> u128 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        now.wrapping_add(SEQ.fetch_add(1, Ordering::Relaxed) as u128 * 1_000_000_039)
    }
}
