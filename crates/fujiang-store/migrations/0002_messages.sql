CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id INTEGER NOT NULL,
    time INTEGER NOT NULL,
    self_id INTEGER NOT NULL,
    user_id INTEGER NOT NULL,
    group_id INTEGER,
    nickname TEXT,
    card TEXT,
    raw_text TEXT NOT NULL,
    segments_json TEXT NOT NULL DEFAULT '[]',
    inserted_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_message_id ON messages(message_id);
CREATE INDEX IF NOT EXISTS idx_messages_group_time ON messages(group_id, time);
CREATE INDEX IF NOT EXISTS idx_messages_user_time ON messages(user_id, time);

CREATE INDEX IF NOT EXISTS idx_learn_group_trigger ON learn_replies(group_id, trigger);
