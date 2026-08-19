ALTER TABLE ideas RENAME TO tags;
ALTER TABLE idea_aliases RENAME TO tag_aliases;
ALTER TABLE image_ideas RENAME TO image_tags;
ALTER TABLE tag_aliases RENAME COLUMN idea_id TO tag_id;
ALTER TABLE image_tags RENAME COLUMN idea_id TO tag_id;
DROP INDEX IF EXISTS idx_image_ideas_idea;
CREATE INDEX IF NOT EXISTS idx_image_tags_tag ON image_tags(tag_id);
