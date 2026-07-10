-- Persist equity alongside comp. Equity has fed `content_hash` since 0.4.0, but was
-- never stored as a column — so the version log can't answer "when did equity change",
-- and `changed_fields` had no stored value to compare against, mislabeling every equity
-- move as a "description" edit.
--
-- Existing rows default to `none`; the real value backfills on the next fetch. That
-- backfill is silent: the content hash ALREADY reflects equity, so an unchanged posting
-- keeps its hash and never reports a spurious CHANGED — only the column catches up.
ALTER TABLE postings ADD COLUMN equity TEXT NOT NULL DEFAULT '{"kind":"none"}';
