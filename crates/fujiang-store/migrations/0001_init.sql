CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS contests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    oj TEXT NOT NULL,
    title TEXT NOT NULL,
    begin_ts INTEGER NOT NULL,
    end_ts INTEGER NOT NULL,
    url TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_contests_oj ON contests(oj);
CREATE INDEX IF NOT EXISTS idx_contests_begin ON contests(begin_ts);

CREATE TABLE IF NOT EXISTS remind_groups (
    group_id INTEGER PRIMARY KEY,
    hour INTEGER NOT NULL,
    min INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS cf_users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    year INTEGER NOT NULL,
    name TEXT NOT NULL,
    handle TEXT NOT NULL UNIQUE,
    last_rating INTEGER NOT NULL DEFAULT 0,
    max_rating INTEGER NOT NULL DEFAULT 0,
    solved INTEGER NOT NULL DEFAULT 0,
    last_month INTEGER NOT NULL DEFAULT 0,
    valid_rating INTEGER NOT NULL DEFAULT 0,
    is_main INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS contest_standings (
    contest_id TEXT NOT NULL,
    handle TEXT NOT NULL,
    label TEXT NOT NULL,
    rank INTEGER NOT NULL,
    old_rating INTEGER NOT NULL,
    new_rating INTEGER NOT NULL,
    PRIMARY KEY (contest_id, handle)
);

CREATE TABLE IF NOT EXISTS daily_problems (
    date TEXT NOT NULL,
    band TEXT NOT NULL,
    contest_id INTEGER NOT NULL,
    idx TEXT NOT NULL,
    url TEXT NOT NULL,
    PRIMARY KEY (date, band)
);

CREATE TABLE IF NOT EXISTS learn_replies (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    trigger TEXT NOT NULL,
    reply TEXT NOT NULL,
    created_by INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    group_id INTEGER
);

CREATE INDEX IF NOT EXISTS idx_learn_trigger ON learn_replies(trigger);

CREATE TABLE IF NOT EXISTS stars (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    url TEXT NOT NULL,
    created_by INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS albums (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    dir TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS album_aliases (
    alias TEXT PRIMARY KEY,
    album_id INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS album_images (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    album_id INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    rel_path TEXT NOT NULL,
    md5 TEXT NOT NULL,
    added_by INTEGER NOT NULL DEFAULT 0,
    added_at INTEGER NOT NULL,
    UNIQUE(album_id, md5)
);
