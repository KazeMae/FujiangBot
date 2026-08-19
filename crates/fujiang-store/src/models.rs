use sqlx::FromRow;

#[derive(Debug, Clone, FromRow)]
pub struct ContestRow {
    pub id: i64,
    pub oj: String,
    pub title: String,
    pub begin_ts: i64,
    pub end_ts: i64,
    pub url: String,
    pub source: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct RemindGroup {
    pub group_id: i64,
    pub hour: i64,
    pub min: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct CfUser {
    pub id: i64,
    pub year: i64,
    pub name: String,
    pub handle: String,
    pub last_rating: i64,
    pub max_rating: i64,
    pub solved: i64,
    pub last_month: i64,
    pub valid_rating: i64,
    pub is_main: i64,
    pub updated_at: i64,
}

impl Default for CfUser {
    fn default() -> Self {
        Self {
            id: 0,
            year: 0,
            name: String::new(),
            handle: String::new(),
            last_rating: 0,
            max_rating: 0,
            solved: 0,
            last_month: 0,
            valid_rating: 0,
            is_main: 0,
            updated_at: 0,
        }
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct StandingRow {
    pub contest_id: String,
    pub handle: String,
    pub label: String,
    pub rank: i64,
    pub old_rating: i64,
    pub new_rating: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct DailyProblem {
    pub date: String,
    pub band: String,
    pub contest_id: i64,
    pub idx: String,
    pub url: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct LearnReply {
    pub id: i64,
    pub trigger: String,
    pub reply: String,
    pub created_by: i64,
    pub created_at: i64,
    pub group_id: Option<i64>,
}

#[derive(Debug, Clone, FromRow)]
pub struct Star {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub created_by: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct Album {
    pub id: i64,
    pub name: String,
    pub dir: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct AlbumImage {
    pub id: i64,
    pub album_id: i64,
    pub rel_path: String,
    pub md5: String,
    pub added_by: i64,
    pub added_at: i64,
}
