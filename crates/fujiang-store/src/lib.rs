mod importer;
mod models;

pub use importer::migrate_from_python;
pub use models::*;

use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
    pub image_root: PathBuf,
}

impl Store {
    pub async fn open(db: &str, image_root: impl Into<PathBuf>) -> anyhow::Result<Self> {
        if let Some(parent) = Path::new(db).parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let image_root = image_root.into();
        tokio::fs::create_dir_all(&image_root).await.ok();

        let opts = SqliteConnectOptions::new()
            .filename(db)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool, image_root })
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

    pub async fn learn_list(&self, trigger: Option<&str>) -> anyhow::Result<Vec<LearnReply>> {
        if let Some(t) = trigger {
            Ok(sqlx::query_as::<_, LearnReply>(
                "SELECT id, trigger, reply, created_by, created_at, group_id
                 FROM learn_replies WHERE trigger = ? ORDER BY id",
            )
            .bind(t)
            .fetch_all(&self.pool)
            .await?)
        } else {
            Ok(sqlx::query_as::<_, LearnReply>(
                "SELECT id, trigger, reply, created_by, created_at, group_id
                 FROM learn_replies ORDER BY trigger, id",
            )
            .fetch_all(&self.pool)
            .await?)
        }
    }

    pub async fn learn_del(&self, trigger: &str, n: Option<i64>) -> anyhow::Result<u64> {
        if let Some(n) = n {
            let rows = self.learn_list(Some(trigger)).await?;
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
        let r = sqlx::query("DELETE FROM learn_replies WHERE trigger = ?")
            .bind(trigger)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    pub async fn learn_random(&self, trigger: &str) -> anyhow::Result<Option<String>> {
        let row = sqlx::query(
            "SELECT reply FROM learn_replies WHERE trigger = ? ORDER BY RANDOM() LIMIT 1",
        )
        .bind(trigger)
        .fetch_optional(&self.pool)
        .await?;
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

    // --- albums ---

    pub async fn album_by_alias(&self, alias: &str) -> anyhow::Result<Option<Album>> {
        Ok(sqlx::query_as::<_, Album>(
            "SELECT a.id, a.name, a.dir FROM albums a
             JOIN album_aliases x ON x.album_id = a.id
             WHERE x.alias = ?",
        )
        .bind(alias)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn album_by_name(&self, name: &str) -> anyhow::Result<Option<Album>> {
        Ok(
            sqlx::query_as::<_, Album>("SELECT id, name, dir FROM albums WHERE name = ?")
                .bind(name)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn album_list(&self) -> anyhow::Result<Vec<(Album, Vec<String>)>> {
        let albums = sqlx::query_as::<_, Album>("SELECT id, name, dir FROM albums ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::new();
        for a in albums {
            let aliases: Vec<String> =
                sqlx::query("SELECT alias FROM album_aliases WHERE album_id = ?")
                    .bind(a.id)
                    .fetch_all(&self.pool)
                    .await?
                    .into_iter()
                    .map(|r| r.get::<String, _>(0))
                    .collect();
            out.push((a, aliases));
        }
        Ok(out)
    }

    pub async fn album_add(&self, name: &str) -> anyhow::Result<Album> {
        if let Some(a) = self.album_by_name(name).await? {
            return Ok(a);
        }
        let dir = sanitize_dir(name);
        sqlx::query("INSERT INTO albums(name, dir) VALUES(?, ?)")
            .bind(name)
            .bind(&dir)
            .execute(&self.pool)
            .await?;
        let album = self.album_by_name(name).await?.expect("just inserted");
        sqlx::query("INSERT OR IGNORE INTO album_aliases(alias, album_id) VALUES(?, ?)")
            .bind(name)
            .bind(album.id)
            .execute(&self.pool)
            .await?;
        tokio::fs::create_dir_all(self.image_root.join(&album.dir))
            .await
            .ok();
        Ok(album)
    }

    pub async fn album_alias(&self, name: &str, alias: &str) -> anyhow::Result<String> {
        let album = self
            .album_by_name(name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("图集「{name}」不存在"))?;
        sqlx::query(
            "INSERT INTO album_aliases(alias, album_id) VALUES(?, ?)
             ON CONFLICT(alias) DO UPDATE SET album_id = excluded.album_id",
        )
        .bind(alias)
        .bind(album.id)
        .execute(&self.pool)
        .await?;
        Ok(format!("{alias} -> {name}"))
    }

    pub async fn album_merge(&self, from: &str, to: &str) -> anyhow::Result<String> {
        let src = self
            .album_by_name(from)
            .await?
            .ok_or_else(|| anyhow::anyhow!("图集「{from}」不存在"))?;
        let dst = self.album_add(to).await?;
        if src.id == dst.id {
            return Ok("已经是同一个图集".into());
        }
        let images = sqlx::query_as::<_, AlbumImage>(
            "SELECT id, album_id, rel_path, md5, added_by, added_at FROM album_images WHERE album_id = ?",
        )
        .bind(src.id)
        .fetch_all(&self.pool)
        .await?;
        for img in images {
            let src_path = self.image_root.join(&img.rel_path);
            let ext = Path::new(&img.rel_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("jpg");
            let new_rel = format!("{}/{}.{}", dst.dir, img.md5, ext);
            let dst_path = self.image_root.join(&new_rel);
            if src_path.exists() && !dst_path.exists() {
                if let Some(p) = dst_path.parent() {
                    tokio::fs::create_dir_all(p).await.ok();
                }
                let _ = tokio::fs::copy(&src_path, &dst_path).await;
            }
            sqlx::query(
                "INSERT OR IGNORE INTO album_images(album_id, rel_path, md5, added_by, added_at)
                 VALUES(?, ?, ?, ?, ?)",
            )
            .bind(dst.id)
            .bind(&new_rel)
            .bind(&img.md5)
            .bind(img.added_by)
            .bind(img.added_at)
            .execute(&self.pool)
            .await?;
        }
        sqlx::query("UPDATE album_aliases SET album_id = ? WHERE album_id = ?")
            .bind(dst.id)
            .bind(src.id)
            .execute(&self.pool)
            .await?;
        sqlx::query("INSERT OR IGNORE INTO album_aliases(alias, album_id) VALUES(?, ?)")
            .bind(from)
            .bind(dst.id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM album_images WHERE album_id = ?")
            .bind(src.id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM albums WHERE id = ?")
            .bind(src.id)
            .execute(&self.pool)
            .await?;
        Ok(format!("已合并 {from} -> {to}"))
    }

    pub async fn album_del(&self, name: &str, delete_files: bool) -> anyhow::Result<bool> {
        let Some(album) = self.album_by_name(name).await? else {
            return Ok(false);
        };
        if delete_files {
            let dir = self.image_root.join(&album.dir);
            let _ = tokio::fs::remove_dir_all(dir).await;
        }
        sqlx::query("DELETE FROM albums WHERE id = ?")
            .bind(album.id)
            .execute(&self.pool)
            .await?;
        Ok(true)
    }

    pub async fn album_random_image(&self, alias: &str) -> anyhow::Result<Option<PathBuf>> {
        let Some(album) = self.album_by_alias(alias).await? else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT rel_path FROM album_images WHERE album_id = ? ORDER BY RANDOM() LIMIT 1",
        )
        .bind(album.id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| self.image_root.join(r.get::<String, _>(0))))
    }

    pub async fn album_add_image(
        &self,
        album_name: &str,
        bytes: &[u8],
        ext: &str,
        added_by: i64,
    ) -> anyhow::Result<AlbumAddResult> {
        let album = self.album_add(album_name).await?;
        let md5 = md5_hex(bytes);
        let exists = sqlx::query("SELECT 1 FROM album_images WHERE album_id = ? AND md5 = ?")
            .bind(album.id)
            .bind(&md5)
            .fetch_optional(&self.pool)
            .await?
            .is_some();
        if exists {
            return Ok(AlbumAddResult::Duplicate);
        }
        let rel = format!("{}/{}.{}", album.dir, md5, ext);
        let path = self.image_root.join(&rel);
        if let Some(p) = path.parent() {
            tokio::fs::create_dir_all(p).await?;
        }
        tokio::fs::write(&path, bytes).await?;
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO album_images(album_id, rel_path, md5, added_by, added_at)
             VALUES(?, ?, ?, ?, ?)",
        )
        .bind(album.id)
        .bind(&rel)
        .bind(&md5)
        .bind(added_by)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(AlbumAddResult::Added)
    }

    pub async fn album_del_image_by_md5(
        &self,
        album_name: &str,
        md5: &str,
        delete_file: bool,
    ) -> anyhow::Result<bool> {
        let Some(album) = self.album_by_alias(album_name).await? else {
            return Ok(false);
        };
        let row =
            sqlx::query("SELECT id, rel_path FROM album_images WHERE album_id = ? AND md5 = ?")
                .bind(album.id)
                .bind(md5)
                .fetch_optional(&self.pool)
                .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let rel: String = row.get(1);
        sqlx::query("DELETE FROM album_images WHERE id = ?")
            .bind(row.get::<i64, _>(0))
            .execute(&self.pool)
            .await?;
        if delete_file {
            let _ = tokio::fs::remove_file(self.image_root.join(rel)).await;
        }
        Ok(true)
    }
}

#[derive(Debug)]
pub enum AlbumAddResult {
    Added,
    Duplicate,
}

pub fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn sanitize_dir(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "album".into()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn learn_roundtrip() {
        let dir = std::env::temp_dir().join(format!("fujiang-test-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let store = Store::open(db.to_str().unwrap(), dir.join("img"))
            .await
            .unwrap();
        store.learn_add("hi", "hello", 1, None).await.unwrap();
        store.learn_add("hi", "hey", 1, None).await.unwrap();
        let list = store.learn_list(Some("hi")).await.unwrap();
        assert_eq!(list.len(), 2);
        let n = store.learn_del("hi", Some(1)).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.learn_list(Some("hi")).await.unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn uuid_like() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }
}
