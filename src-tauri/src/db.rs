use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::AppError;
use crate::models::{
    BootstrapState, CleanedItem, Edition, PersistedBrowserSession, SyncRun, SyncRunTimings,
    TweetDbEntry, UserSettings, XConnectionSummary,
};

fn normalize_schema_sql(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn editions_table_uses_date_unique_constraint(schema: &str) -> bool {
    normalize_schema_sql(schema).contains("edition_date text not null unique")
}

#[derive(Debug, Clone)]
pub struct Database {
    path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct ProcessedItemHistory {
    pub item_ids: HashSet<String>,
    pub fingerprints: HashSet<String>,
}

impl Database {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, AppError> {
        let path = path.as_ref().to_path_buf();

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let db = Self { path };
        db.migrate()?;
        Ok(db)
    }

    fn connect(&self) -> Result<Connection, AppError> {
        let conn = Connection::open(&self.path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        Ok(conn)
    }

    fn migrate(&self) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute_batch(
            r#"
      CREATE TABLE IF NOT EXISTS app_meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
      );

      CREATE TABLE IF NOT EXISTS x_connection (
        user_id TEXT PRIMARY KEY,
        handle TEXT NOT NULL,
        name TEXT NOT NULL,
        connected_at TEXT NOT NULL
      );

      CREATE TABLE IF NOT EXISTS sync_runs (
        id TEXT PRIMARY KEY,
        started_at TEXT NOT NULL,
        finished_at TEXT,
        status TEXT NOT NULL,
        item_count INTEGER NOT NULL DEFAULT 0,
        kept_count INTEGER NOT NULL DEFAULT 0,
        error_message TEXT,
        edition_id TEXT
      );

      CREATE TABLE IF NOT EXISTS feed_items (
        id TEXT PRIMARY KEY,
        source TEXT NOT NULL,
        author_name TEXT NOT NULL,
        author_handle TEXT NOT NULL,
        text TEXT NOT NULL,
        source_url TEXT NOT NULL,
        posted_at TEXT NOT NULL,
        fingerprint TEXT NOT NULL UNIQUE,
        raw_json TEXT NOT NULL
      );

      CREATE TABLE IF NOT EXISTS tweetdb (
        tweet_id TEXT PRIMARY KEY,
        source TEXT NOT NULL,
        author_name TEXT NOT NULL,
        author_handle TEXT NOT NULL,
        text TEXT NOT NULL,
        source_url TEXT NOT NULL,
        posted_at TEXT NOT NULL,
        fingerprint TEXT NOT NULL,
        first_seen_at TEXT NOT NULL,
        last_seen_at TEXT NOT NULL,
        seen_count INTEGER NOT NULL DEFAULT 1,
        last_sync_run_id TEXT,
        raw_json TEXT NOT NULL
      );
      CREATE INDEX IF NOT EXISTS tweetdb_posted_at_idx ON tweetdb(posted_at DESC);
      CREATE INDEX IF NOT EXISTS tweetdb_first_seen_at_idx ON tweetdb(first_seen_at DESC);
      CREATE INDEX IF NOT EXISTS tweetdb_fingerprint_idx ON tweetdb(fingerprint);

      CREATE TABLE IF NOT EXISTS item_decisions (
        item_id TEXT PRIMARY KEY,
        keep INTEGER NOT NULL,
        category TEXT NOT NULL,
        headline TEXT NOT NULL,
        summary TEXT NOT NULL,
        why_it_matters TEXT NOT NULL,
        reasons_json TEXT NOT NULL,
        run_id TEXT NOT NULL
      );

      CREATE TABLE IF NOT EXISTS editions (
        id TEXT PRIMARY KEY,
        edition_date TEXT NOT NULL,
        title TEXT NOT NULL,
        front_page_summary TEXT NOT NULL,
        created_at TEXT NOT NULL,
        json_content TEXT NOT NULL,
        sync_run_id TEXT NOT NULL
      );
      "#,
        )?;
        self.migrate_editions_table(&conn)?;
        self.migrate_sync_runs_table(&conn)?;

        if self.load_settings()?.lm_studio.base_url.is_empty() {
            self.save_settings(&UserSettings::default())?;
        }

        Ok(())
    }

    fn migrate_editions_table(&self, conn: &Connection) -> Result<(), AppError> {
        let schema = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'editions'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        if schema
            .as_deref()
            .is_some_and(editions_table_uses_date_unique_constraint)
        {
            conn.execute_batch(
                r#"
        BEGIN IMMEDIATE;
        CREATE TABLE editions__migrated (
          id TEXT PRIMARY KEY,
          edition_date TEXT NOT NULL,
          title TEXT NOT NULL,
          front_page_summary TEXT NOT NULL,
          created_at TEXT NOT NULL,
          json_content TEXT NOT NULL,
          sync_run_id TEXT NOT NULL
        );
        INSERT INTO editions__migrated(
          id,
          edition_date,
          title,
          front_page_summary,
          created_at,
          json_content,
          sync_run_id
        )
        SELECT
          id,
          edition_date,
          title,
          front_page_summary,
          created_at,
          json_content,
          sync_run_id
        FROM editions
        ORDER BY created_at ASC;
        DROP TABLE editions;
        ALTER TABLE editions__migrated RENAME TO editions;
        COMMIT;
        "#,
            )?;
        }

        Ok(())
    }

    fn migrate_sync_runs_table(&self, conn: &Connection) -> Result<(), AppError> {
        let mut stmt = conn.prepare("PRAGMA table_info(sync_runs)")?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<HashSet<_>, _>>()?;

        if !columns.contains("reason") {
            conn.execute("ALTER TABLE sync_runs ADD COLUMN reason TEXT NOT NULL DEFAULT 'manual'", [])?;
        }
        if !columns.contains("schedule_rule_id") {
            conn.execute("ALTER TABLE sync_runs ADD COLUMN schedule_rule_id TEXT", [])?;
        }
        if !columns.contains("schedule_rule_label") {
            conn.execute("ALTER TABLE sync_runs ADD COLUMN schedule_rule_label TEXT", [])?;
        }
        if !columns.contains("schedule_slot_key") {
            conn.execute("ALTER TABLE sync_runs ADD COLUMN schedule_slot_key TEXT", [])?;
        }
        if !columns.contains("timings_json") {
            conn.execute(
                "ALTER TABLE sync_runs ADD COLUMN timings_json TEXT NOT NULL DEFAULT '{\"captureMs\":0,\"rankingMs\":0,\"frontPageMs\":0,\"savingMs\":0,\"totalMs\":0}'",
                [],
            )?;
        }

        conn.execute(
            "CREATE INDEX IF NOT EXISTS sync_runs_schedule_slot_idx ON sync_runs(schedule_rule_id, schedule_slot_key)",
            [],
        )?;

        Ok(())
    }

    pub fn load_settings(&self) -> Result<UserSettings, AppError> {
        let conn = self.connect()?;
        let raw = conn
            .query_row(
                "SELECT value FROM app_meta WHERE key = 'settings'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        if let Some(raw) = raw {
            let settings = serde_json::from_str::<UserSettings>(&raw)?;
            if settings.lm_studio.auth_token.is_some() {
                let sanitized = settings.without_secrets();
                self.save_settings(&sanitized)?;
                Ok(sanitized)
            } else {
                Ok(settings)
            }
        } else {
            Ok(UserSettings::default())
        }
    }

    pub fn save_settings(&self, settings: &UserSettings) -> Result<UserSettings, AppError> {
        let sanitized = settings.without_secrets();
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO app_meta(key, value) VALUES('settings', ?1)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![serde_json::to_string(&sanitized)?],
        )?;
        Ok(sanitized)
    }

    pub fn load_bootstrap(&self) -> Result<BootstrapState, AppError> {
        self.mark_incomplete_sync_runs_interrupted()?;
        Ok(BootstrapState {
            settings: self.load_settings()?,
            editions: self.load_editions()?,
            latest_run: self.load_latest_run()?,
            run_history: self.load_run_history()?,
            x_connection: self.load_x_connection()?,
        })
    }

    pub fn load_editions(&self) -> Result<Vec<Edition>, AppError> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT json_content FROM editions ORDER BY created_at DESC, id DESC LIMIT 30",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let editions = rows
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|raw| serde_json::from_str::<Edition>(&raw))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(editions)
    }

    pub fn load_latest_edition(&self) -> Result<Option<Edition>, AppError> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT json_content FROM editions ORDER BY created_at DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|raw| serde_json::from_str::<Edition>(&raw))
        .transpose()
        .map_err(AppError::from)
    }

    pub fn load_latest_run(&self) -> Result<Option<SyncRun>, AppError> {
        let conn = self.connect()?;
        conn
      .query_row(
        "SELECT id, reason, schedule_rule_id, schedule_rule_label, schedule_slot_key, started_at, finished_at, status, item_count, kept_count, error_message, edition_id, timings_json
         FROM sync_runs
         ORDER BY started_at DESC
         LIMIT 1",
        [],
        Self::map_sync_run_row,
      )
      .optional()
      .map_err(AppError::from)
    }

    pub fn load_run_history(&self) -> Result<Vec<SyncRun>, AppError> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT id, reason, schedule_rule_id, schedule_rule_label, schedule_slot_key, started_at, finished_at, status, item_count, kept_count, error_message, edition_id, timings_json
             FROM sync_runs
             ORDER BY started_at DESC
             LIMIT 40",
        )?;
        let rows = stmt.query_map([], Self::map_sync_run_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn has_run_for_schedule_slot(
        &self,
        schedule_rule_id: &str,
        schedule_slot_key: &str,
    ) -> Result<bool, AppError> {
        let conn = self.connect()?;
        let exists: i64 = conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_runs
                WHERE schedule_rule_id = ?1 AND schedule_slot_key = ?2
            )",
            params![schedule_rule_id, schedule_slot_key],
            |row| row.get(0),
        )?;
        Ok(exists > 0)
    }

    fn map_sync_run_row(row: &rusqlite::Row<'_>) -> Result<SyncRun, rusqlite::Error> {
        Ok(SyncRun {
            id: row.get(0)?,
            reason: serde_json::from_str(&format!("\"{}\"", row.get::<_, String>(1)?)).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(error))
            })?,
            schedule_rule_id: row.get(2)?,
            schedule_rule_label: row.get(3)?,
            schedule_slot_key: row.get(4)?,
            started_at: row.get(5)?,
            finished_at: row.get(6)?,
            status: serde_json::from_str(&format!("\"{}\"", row.get::<_, String>(7)?)).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(error))
            })?,
            item_count: row.get(8)?,
            kept_count: row.get(9)?,
            error_message: row.get(10)?,
            edition_id: row.get(11)?,
            timings: row
                .get::<_, Option<String>>(12)?
                .map(|raw| serde_json::from_str::<SyncRunTimings>(&raw))
                .transpose()
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(12, rusqlite::types::Type::Text, Box::new(error))
                })?
                .unwrap_or_default(),
        })
    }

    pub fn load_x_connection(&self) -> Result<Option<XConnectionSummary>, AppError> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT user_id, handle, name, connected_at FROM x_connection LIMIT 1",
            [],
            |row| {
                Ok(XConnectionSummary {
                    user_id: row.get(0)?,
                    handle: row.get(1)?,
                    name: row.get(2)?,
                    connected_at: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(AppError::from)
    }

    pub fn upsert_x_connection(&self, summary: &XConnectionSummary) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute("DELETE FROM x_connection", [])?;
        conn.execute(
            "INSERT INTO x_connection(user_id, handle, name, connected_at) VALUES(?1, ?2, ?3, ?4)",
            params![
                summary.user_id,
                summary.handle,
                summary.name,
                summary.connected_at
            ],
        )?;
        Ok(())
    }

    pub fn clear_x_connection(&self) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute("DELETE FROM x_connection", [])?;
        Ok(())
    }

    pub fn load_persisted_x_session(&self) -> Result<Option<PersistedBrowserSession>, AppError> {
        let conn = self.connect()?;
        let raw = conn
            .query_row(
                "SELECT value FROM app_meta WHERE key = 'x_session_restore'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        raw.map(|raw| serde_json::from_str(&raw))
            .transpose()
            .map_err(AppError::from)
    }

    pub fn save_persisted_x_session(
        &self,
        session: &PersistedBrowserSession,
    ) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO app_meta(key, value) VALUES('x_session_restore', ?1)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![serde_json::to_string(session)?],
        )?;
        Ok(())
    }

    pub fn clear_persisted_x_session(&self) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute("DELETE FROM app_meta WHERE key = 'x_session_restore'", [])?;
        Ok(())
    }

    pub fn load_persisted_linkedin_session(
        &self,
    ) -> Result<Option<PersistedBrowserSession>, AppError> {
        let conn = self.connect()?;
        let raw = conn
            .query_row(
                "SELECT value FROM app_meta WHERE key = 'linkedin_session_restore'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        raw.map(|raw| serde_json::from_str(&raw))
            .transpose()
            .map_err(AppError::from)
    }

    pub fn save_persisted_linkedin_session(
        &self,
        session: &PersistedBrowserSession,
    ) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO app_meta(key, value) VALUES('linkedin_session_restore', ?1)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![serde_json::to_string(session)?],
        )?;
        Ok(())
    }

    pub fn clear_persisted_linkedin_session(&self) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute("DELETE FROM app_meta WHERE key = 'linkedin_session_restore'", [])?;
        Ok(())
    }

    pub fn load_persisted_reddit_session(
        &self,
    ) -> Result<Option<PersistedBrowserSession>, AppError> {
        let conn = self.connect()?;
        let raw = conn
            .query_row(
                "SELECT value FROM app_meta WHERE key = 'reddit_session_restore'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        raw.map(|raw| serde_json::from_str(&raw))
            .transpose()
            .map_err(AppError::from)
    }

    pub fn save_persisted_reddit_session(
        &self,
        session: &PersistedBrowserSession,
    ) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO app_meta(key, value) VALUES('reddit_session_restore', ?1)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![serde_json::to_string(session)?],
        )?;
        Ok(())
    }

    pub fn clear_persisted_reddit_session(&self) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute("DELETE FROM app_meta WHERE key = 'reddit_session_restore'", [])?;
        Ok(())
    }

    pub fn insert_sync_run(&self, run: &SyncRun) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute(
      "INSERT INTO sync_runs(id, reason, schedule_rule_id, schedule_rule_label, schedule_slot_key, started_at, finished_at, status, item_count, kept_count, error_message, edition_id, timings_json)
       VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
       ON CONFLICT(id) DO UPDATE SET
         reason = excluded.reason,
         schedule_rule_id = excluded.schedule_rule_id,
         schedule_rule_label = excluded.schedule_rule_label,
         schedule_slot_key = excluded.schedule_slot_key,
         finished_at = excluded.finished_at,
         status = excluded.status,
         item_count = excluded.item_count,
         kept_count = excluded.kept_count,
         error_message = excluded.error_message,
         edition_id = excluded.edition_id,
         timings_json = excluded.timings_json",
      params![
        run.id,
        run.reason.as_str(),
        run.schedule_rule_id,
        run.schedule_rule_label,
        run.schedule_slot_key,
        run.started_at,
        run.finished_at,
        serde_json::to_string(&run.status)?.replace('\"', ""),
        run.item_count,
        run.kept_count,
        run.error_message,
        run.edition_id,
        serde_json::to_string(&run.timings)?
      ],
    )?;
        Ok(())
    }

    pub fn mark_incomplete_sync_runs_interrupted(&self) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE sync_runs
             SET status = 'error',
                 finished_at = COALESCE(finished_at, ?1),
                 error_message = COALESCE(error_message, 'SIFT was closed before this refresh finished.')
             WHERE status = 'running'",
            params![chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn insert_feed_items(&self, items: &[crate::models::FeedItem]) -> Result<(), AppError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;

        for item in items {
            tx.execute(
        "INSERT OR IGNORE INTO feed_items(id, source, author_name, author_handle, text, source_url, posted_at, fingerprint, raw_json)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
          item.id,
          item.source,
          item.author_name,
          item.author_handle,
          item.text,
          item.source_url,
          item.posted_at,
          item.fingerprint,
          serde_json::to_string(&item.raw_json)?
        ],
      )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn upsert_tweets(
        &self,
        items: &[crate::models::FeedItem],
        seen_at: &str,
        run_id: &str,
    ) -> Result<(), AppError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;

        for item in items {
            tx.execute(
                "INSERT INTO tweetdb(
                   tweet_id, source, author_name, author_handle, text, source_url, posted_at,
                   fingerprint, first_seen_at, last_seen_at, seen_count, last_sync_run_id, raw_json
                 )
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11, ?12)
                 ON CONFLICT(tweet_id) DO UPDATE SET
                   source = excluded.source,
                   author_name = excluded.author_name,
                   author_handle = excluded.author_handle,
                   text = excluded.text,
                   source_url = excluded.source_url,
                   posted_at = excluded.posted_at,
                   fingerprint = excluded.fingerprint,
                   last_seen_at = excluded.last_seen_at,
                   seen_count = tweetdb.seen_count + 1,
                   last_sync_run_id = excluded.last_sync_run_id,
                   raw_json = excluded.raw_json",
                params![
                    item.id,
                    item.source,
                    item.author_name,
                    item.author_handle,
                    item.text,
                    item.source_url,
                    item.posted_at,
                    item.fingerprint,
                    seen_at,
                    seen_at,
                    run_id,
                    serde_json::to_string(&item.raw_json)?
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn load_tweetdb_entries(
        &self,
        tweet_ids: &[String],
    ) -> Result<HashMap<String, TweetDbEntry>, AppError> {
        if tweet_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let conn = self.connect()?;
        let placeholders = vec!["?"; tweet_ids.len()].join(", ");
        let query = format!(
            "SELECT tweet_id, first_seen_at, last_seen_at, seen_count
             FROM tweetdb
             WHERE tweet_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&query)?;
        let rows = stmt.query_map(params_from_iter(tweet_ids.iter()), |row| {
            Ok(TweetDbEntry {
                tweet_id: row.get(0)?,
                first_seen_at: row.get(1)?,
                last_seen_at: row.get(2)?,
                seen_count: row.get(3)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|entry| (entry.tweet_id.clone(), entry))
                    .collect::<HashMap<_, _>>()
            })
            .map_err(AppError::from)
    }

    pub fn save_edition(
        &self,
        edition: &Edition,
        decisions: &[CleanedItem],
        run: &SyncRun,
    ) -> Result<(), AppError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;

        tx.execute(
      "INSERT INTO editions(id, edition_date, title, front_page_summary, created_at, json_content, sync_run_id)
       VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
       ON CONFLICT(id) DO UPDATE SET
         edition_date = excluded.edition_date,
         title = excluded.title,
         front_page_summary = excluded.front_page_summary,
         created_at = excluded.created_at,
         json_content = excluded.json_content,
         sync_run_id = excluded.sync_run_id",
      params![
        edition.id,
        edition.edition_date,
        edition.title,
        edition.front_page_summary,
        edition.created_at,
        serde_json::to_string(edition)?,
        run.id
      ],
    )?;

        for decision in decisions {
            tx.execute(
        "INSERT OR REPLACE INTO item_decisions(item_id, keep, category, headline, summary, why_it_matters, reasons_json, run_id)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
          decision.item_id,
          decision.keep as i64,
          decision.category,
          decision.headline,
          decision.summary,
          decision.why_it_matters,
          serde_json::to_string(&decision.reasons)?,
          run.id
        ],
      )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn load_processed_item_history(&self) -> Result<ProcessedItemHistory, AppError> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT d.item_id, COALESCE(t.fingerprint, f.fingerprint)
             FROM item_decisions d
             INNER JOIN sync_runs r ON r.id = d.run_id
             LEFT JOIN tweetdb t ON t.tweet_id = d.item_id
             LEFT JOIN feed_items f ON f.id = d.item_id
             WHERE r.status = 'success' AND r.edition_id IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;

        let mut history = ProcessedItemHistory::default();
        for row in rows {
            let (item_id, fingerprint) = row?;
            history.item_ids.insert(item_id);
            if let Some(fingerprint) = fingerprint.filter(|value| !value.is_empty()) {
                history.fingerprints.insert(fingerprint);
            }
        }

        Ok(history)
    }

    pub fn has_edition_for_date(&self, edition_date: &str) -> Result<bool, AppError> {
        let conn = self.connect()?;
        let exists: i64 = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM editions WHERE edition_date = ?1)",
            params![edition_date],
            |row| row.get(0),
        )?;
        Ok(exists > 0)
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::Database;
    use crate::models::{
        Edition, EditionSection, EditionView, FeedItem, PersistedBrowserSession, SyncReason,
        SyncRun, SyncRunTimings, SyncStatus, UserSettings,
    };

    fn sample_sync_run(id: &str) -> SyncRun {
        SyncRun {
            id: id.into(),
            reason: SyncReason::Manual,
            schedule_rule_id: None,
            schedule_rule_label: None,
            schedule_slot_key: None,
            started_at: "2026-04-16T12:10:00Z".into(),
            finished_at: Some("2026-04-16T12:11:00Z".into()),
            status: SyncStatus::Success,
            item_count: 0,
            kept_count: 0,
            error_message: None,
            edition_id: None,
            timings: SyncRunTimings::default(),
        }
    }

    #[test]
    fn persisted_x_session_round_trips_and_clears() {
        let temp_dir = tempdir().expect("temporary database directory");
        let db = Database::new(temp_dir.path().join("sift.sqlite")).expect("database");

        assert!(
            db.load_persisted_x_session()
                .expect("load empty persisted x session")
                .is_none()
        );

        let session = PersistedBrowserSession {
            last_known_url: "https://x.com/home".into(),
            is_authenticated: true,
        };

        db.save_persisted_x_session(&session)
            .expect("save persisted x session");

        let loaded = db
            .load_persisted_x_session()
            .expect("load persisted x session");
        assert!(loaded.is_some());
        let loaded = loaded.expect("persisted x session");
        assert_eq!(loaded.last_known_url, session.last_known_url);
        assert!(loaded.is_authenticated);

        db.clear_persisted_x_session()
            .expect("clear persisted x session");
        assert!(
            db.load_persisted_x_session()
                .expect("load cleared persisted x session")
                .is_none()
        );
    }

    #[test]
    fn persisted_reddit_session_round_trips_and_clears() {
        let temp_dir = tempdir().expect("temporary database directory");
        let db = Database::new(temp_dir.path().join("sift.sqlite")).expect("database");

        assert!(
            db.load_persisted_reddit_session()
                .expect("load empty persisted reddit session")
                .is_none()
        );

        let session = PersistedBrowserSession {
            last_known_url: "https://www.reddit.com/".into(),
            is_authenticated: true,
        };

        db.save_persisted_reddit_session(&session)
            .expect("save persisted reddit session");

        let loaded = db
            .load_persisted_reddit_session()
            .expect("load persisted reddit session");
        assert!(loaded.is_some());
        let loaded = loaded.expect("persisted reddit session");
        assert_eq!(loaded.last_known_url, session.last_known_url);
        assert!(loaded.is_authenticated);

        db.clear_persisted_reddit_session()
            .expect("clear persisted reddit session");
        assert!(
            db.load_persisted_reddit_session()
                .expect("load cleared persisted reddit session")
                .is_none()
        );
    }

    #[test]
    fn settings_strip_lm_studio_auth_tokens_from_storage() {
        let temp_dir = tempdir().expect("temporary database directory");
        let db = Database::new(temp_dir.path().join("sift.sqlite")).expect("database");

        let settings = UserSettings {
            lm_studio: crate::models::LmStudioSettings {
                auth_token: Some("top-secret".into()),
                ..UserSettings::default().lm_studio
            },
            ..UserSettings::default()
        };

        let saved = db.save_settings(&settings).expect("save settings");
        assert_eq!(saved.lm_studio.auth_token, None);

        let loaded = db.load_settings().expect("load settings");
        assert_eq!(loaded.lm_studio.auth_token, None);
    }

    #[test]
    fn settings_without_include_images_default_to_false() {
        let temp_dir = tempdir().expect("temporary database directory");
        let db = Database::new(temp_dir.path().join("sift.sqlite")).expect("database");
        let conn =
            Connection::open(temp_dir.path().join("sift.sqlite")).expect("sqlite connection");

        conn.execute(
            "INSERT INTO app_meta(key, value) VALUES('settings', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![r#"{"schedule":{"enabled":true,"timeOfDay":"07:30","timezone":"UTC"},"cleanup":{"hideReplies":true,"hideRetweets":true,"removeBait":true,"mutedKeywords":[],"mutedAuthors":[]},"lmStudio":{"baseUrl":"http://127.0.0.1:1234","authToken":null,"selectedModel":"vision-model"}}"#],
        )
        .expect("insert legacy settings");

        let loaded = db.load_settings().expect("load legacy settings");
        assert!(!loaded.lm_studio.include_images);
        assert_eq!(
            loaded.lm_studio.selected_model.as_deref(),
            Some("vision-model")
        );
        assert_eq!(loaded.schedule.rules.len(), 1);
        assert_eq!(loaded.schedule.rules[0].time_of_day, "07:30");
        assert_eq!(loaded.capture.browse_page_count.x, 12);
    }

    #[test]
    fn processed_item_history_only_uses_successful_saved_runs() {
        let temp_dir = tempdir().expect("temporary database directory");
        let db = Database::new(temp_dir.path().join("sift.sqlite")).expect("database");

        let feed_item = FeedItem {
            id: "post-1".into(),
            source: "x-session".into(),
            author_name: "Builder".into(),
            author_handle: "builder".into(),
            text: "Shipping a new local-first search release".into(),
            source_url: "https://x.com/builder/status/1".into(),
            posted_at: "2026-04-16T12:00:00Z".into(),
            raw_json: serde_json::json!({}),
            fingerprint: "fingerprint-1".into(),
        };
        db.insert_feed_items(&[feed_item])
            .expect("insert feed item");

        let mut success_run = sample_sync_run("run-success");
        success_run.item_count = 1;
        success_run.kept_count = 1;
        success_run.edition_id = Some("edition-1".into());
        db.insert_sync_run(&success_run)
            .expect("insert successful sync run");

        let edition = Edition {
            id: "edition-1".into(),
            edition_date: "2026-04-16".into(),
            title: "Your SIFT for 2026-04-16".into(),
            front_page_summary: "A solid shipping day.".into(),
            created_at: "2026-04-16T12:11:00Z".into(),
            run_id: success_run.id.clone(),
            view: EditionView::X,
            sections: vec![EditionSection {
                id: "releases".into(),
                title: "Releases".into(),
                dek: "Worth your attention".into(),
                cards: Vec::new(),
            }],
        };
        let decision = crate::models::CleanedItem {
            item_id: "post-1".into(),
            keep: true,
            category: "Releases".into(),
            headline: "Local-first search ships".into(),
            summary: "A builder shipped a new local-first search release.".into(),
            why_it_matters: "It expands on-device search options.".into(),
            reasons: vec!["Concrete product update".into()],
            author_name: "Builder".into(),
            author_handle: "builder".into(),
            source_url: "https://x.com/builder/status/1".into(),
            posted_at: "2026-04-16T12:00:00Z".into(),
        };
        db.save_edition(&edition, &[decision], &success_run)
            .expect("save edition");

        let mut pending_run = sample_sync_run("run-pending");
        pending_run.started_at = "2026-04-16T13:00:00Z".into();
        pending_run.finished_at = Some("2026-04-16T13:01:00Z".into());
        pending_run.status = SyncStatus::Error;
        pending_run.item_count = 1;
        pending_run.error_message = Some("LM Studio crashed".into());
        db.insert_sync_run(&pending_run)
            .expect("insert error sync run");

        let history = db
            .load_processed_item_history()
            .expect("load processed item history");
        assert!(history.item_ids.contains("post-1"));
        assert!(history.fingerprints.contains("fingerprint-1"));
    }

    #[test]
    fn tweetdb_tracks_first_seen_last_seen_and_counts() {
        let temp_dir = tempdir().expect("temporary database directory");
        let db = Database::new(temp_dir.path().join("sift.sqlite")).expect("database");

        let tweet = FeedItem {
            id: "tweet-1".into(),
            source: "x-session".into(),
            author_name: "Builder".into(),
            author_handle: "builder".into(),
            text: "Shipped a neat release".into(),
            source_url: "https://x.com/builder/status/1".into(),
            posted_at: "2026-04-16T12:00:00Z".into(),
            raw_json: serde_json::json!({}),
            fingerprint: "fp-1".into(),
        };

        db.upsert_tweets(
            std::slice::from_ref(&tweet),
            "2026-04-16T12:10:00Z",
            "run-1",
        )
        .expect("insert tweetdb row");
        db.upsert_tweets(&[tweet], "2026-04-16T12:15:00Z", "run-2")
            .expect("update tweetdb row");

        let entries = db
            .load_tweetdb_entries(&["tweet-1".into()])
            .expect("load tweetdb entries");
        let entry = entries.get("tweet-1").expect("tweetdb entry");
        assert_eq!(entry.first_seen_at, "2026-04-16T12:10:00Z");
        assert_eq!(entry.last_seen_at, "2026-04-16T12:15:00Z");
        assert_eq!(entry.seen_count, 2);
    }

    #[test]
    fn latest_edition_uses_created_at_ordering() {
        let temp_dir = tempdir().expect("temporary database directory");
        let db = Database::new(temp_dir.path().join("sift.sqlite")).expect("database");

        let older = Edition {
            id: "edition-older".into(),
            edition_date: "2026-04-15".into(),
            title: "Older".into(),
            front_page_summary: "Yesterday".into(),
            created_at: "2026-04-15T08:00:00Z".into(),
            run_id: "run-older".into(),
            view: EditionView::Consolidated,
            sections: vec![],
        };
        let newer = Edition {
            id: "edition-newer".into(),
            edition_date: "2026-04-16".into(),
            title: "Newer".into(),
            front_page_summary: "Today".into(),
            created_at: "2026-04-16T08:00:00Z".into(),
            run_id: "run-newer".into(),
            view: EditionView::Consolidated,
            sections: vec![],
        };
        let mut older_run = sample_sync_run("run-older");
        older_run.started_at = "2026-04-15T08:00:00Z".into();
        older_run.finished_at = Some("2026-04-15T08:01:00Z".into());
        older_run.edition_id = Some(older.id.clone());
        let mut newer_run = sample_sync_run("run-newer");
        newer_run.started_at = "2026-04-16T08:00:00Z".into();
        newer_run.finished_at = Some("2026-04-16T08:01:00Z".into());
        newer_run.edition_id = Some(newer.id.clone());

        db.insert_sync_run(&older_run).expect("insert older run");
        db.insert_sync_run(&newer_run).expect("insert newer run");
        db.save_edition(&older, &[], &older_run)
            .expect("save older edition");
        db.save_edition(&newer, &[], &newer_run)
            .expect("save newer edition");

        let latest = db.load_latest_edition().expect("load latest edition");
        assert_eq!(latest.expect("latest edition").id, "edition-newer");
    }

    #[test]
    fn editions_keep_multiple_versions_from_same_day() {
        let temp_dir = tempdir().expect("temporary database directory");
        let db = Database::new(temp_dir.path().join("sift.sqlite")).expect("database");

        let earlier = Edition {
            id: "edition-morning".into(),
            edition_date: "2026-04-16".into(),
            title: "Your SIFT for 2026-04-16".into(),
            front_page_summary: "Morning digest".into(),
            created_at: "2026-04-16T08:00:00Z".into(),
            run_id: "run-morning".into(),
            view: EditionView::Consolidated,
            sections: vec![],
        };
        let later = Edition {
            id: "edition-afternoon".into(),
            edition_date: "2026-04-16".into(),
            title: "Your SIFT for 2026-04-16".into(),
            front_page_summary: "Afternoon digest".into(),
            created_at: "2026-04-16T15:30:00Z".into(),
            run_id: "run-afternoon".into(),
            view: EditionView::Consolidated,
            sections: vec![],
        };
        let mut earlier_run = sample_sync_run("run-morning");
        earlier_run.started_at = "2026-04-16T08:00:00Z".into();
        earlier_run.finished_at = Some("2026-04-16T08:02:00Z".into());
        earlier_run.item_count = 5;
        earlier_run.kept_count = 3;
        earlier_run.edition_id = Some(earlier.id.clone());
        let mut later_run = sample_sync_run("run-afternoon");
        later_run.started_at = "2026-04-16T15:30:00Z".into();
        later_run.finished_at = Some("2026-04-16T15:32:00Z".into());
        later_run.item_count = 7;
        later_run.kept_count = 4;
        later_run.edition_id = Some(later.id.clone());

        db.insert_sync_run(&earlier_run)
            .expect("insert earlier run");
        db.insert_sync_run(&later_run).expect("insert later run");
        db.save_edition(&earlier, &[], &earlier_run)
            .expect("save earlier edition");
        db.save_edition(&later, &[], &later_run)
            .expect("save later edition");

        let editions = db.load_editions().expect("load editions");
        assert_eq!(editions.len(), 2);
        assert_eq!(editions[0].id, later.id);
        assert_eq!(editions[1].id, earlier.id);
    }

    #[test]
    fn load_bootstrap_marks_abandoned_running_runs_as_interrupted() {
        let temp_dir = tempdir().expect("temporary database directory");
        let db = Database::new(temp_dir.path().join("sift.sqlite")).expect("database");

        let mut stale_run = sample_sync_run("run-stale");
        stale_run.started_at = "2026-04-16T08:00:00Z".into();
        stale_run.finished_at = None;
        stale_run.status = SyncStatus::Running;
        stale_run.edition_id = None;
        db.insert_sync_run(&stale_run)
        .expect("insert stale running run");

        let bootstrap = db.load_bootstrap().expect("load bootstrap");
        let latest_run = bootstrap.latest_run.expect("latest run");

        assert_eq!(latest_run.status, SyncStatus::Error);
        assert!(latest_run.finished_at.is_some());
        assert_eq!(
            latest_run.error_message.as_deref(),
            Some("SIFT was closed before this refresh finished.")
        );
    }

    #[test]
    fn migrate_removes_same_day_unique_constraint_from_editions() {
        let temp_dir = tempdir().expect("temporary database directory");
        let path = temp_dir.path().join("sift.sqlite");

        {
            let conn = Connection::open(&path).expect("legacy database");
            conn.execute_batch(
                r#"
          CREATE TABLE app_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
          );
          CREATE TABLE editions (
            id TEXT PRIMARY KEY,
            edition_date TEXT NOT NULL UNIQUE,
            title TEXT NOT NULL,
            front_page_summary TEXT NOT NULL,
            created_at TEXT NOT NULL,
            json_content TEXT NOT NULL,
            sync_run_id TEXT NOT NULL
          );
          "#,
            )
            .expect("create legacy schema");
        }

        let db = Database::new(&path).expect("migrated database");

        let first = Edition {
            id: "edition-first".into(),
            edition_date: "2026-04-16".into(),
            title: "Your SIFT for 2026-04-16".into(),
            front_page_summary: "First run".into(),
            created_at: "2026-04-16T09:00:00Z".into(),
            run_id: "run-first".into(),
            view: EditionView::Consolidated,
            sections: vec![],
        };
        let second = Edition {
            id: "edition-second".into(),
            edition_date: "2026-04-16".into(),
            title: "Your SIFT for 2026-04-16".into(),
            front_page_summary: "Second run".into(),
            created_at: "2026-04-16T10:00:00Z".into(),
            run_id: "run-second".into(),
            view: EditionView::Consolidated,
            sections: vec![],
        };
        let mut first_run = sample_sync_run("run-first");
        first_run.started_at = "2026-04-16T09:00:00Z".into();
        first_run.finished_at = Some("2026-04-16T09:01:00Z".into());
        first_run.item_count = 3;
        first_run.kept_count = 2;
        first_run.edition_id = Some(first.id.clone());
        let mut second_run = sample_sync_run("run-second");
        second_run.started_at = "2026-04-16T10:00:00Z".into();
        second_run.finished_at = Some("2026-04-16T10:01:00Z".into());
        second_run.item_count = 4;
        second_run.kept_count = 3;
        second_run.edition_id = Some(second.id.clone());

        db.insert_sync_run(&first_run).expect("insert first run");
        db.insert_sync_run(&second_run).expect("insert second run");
        db.save_edition(&first, &[], &first_run)
            .expect("save first edition");
        db.save_edition(&second, &[], &second_run)
            .expect("save second edition");

        let editions = db.load_editions().expect("load migrated editions");
        assert_eq!(editions.len(), 2);
        assert_eq!(editions[0].id, second.id);
        assert_eq!(editions[1].id, first.id);
    }
}
