use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use dirs::home_dir;
use rusqlite::{
    Connection, OptionalExtension, ToSql, TransactionBehavior, params, params_from_iter,
};

use crate::error::{Result, XurlError};
use crate::model::ProviderKind;

const INDEX_SQLITE_VERSION: i64 = 3;
const DEFAULT_WORKER_LEASE_SECS: u64 = 30;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SearchIndexKey {
    pub provider: ProviderKind,
    pub thread_id: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PreparedMatches {
    pub fresh_keys: HashSet<SearchIndexKey>,
    pub matched_previews: HashMap<SearchIndexKey, String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PreparedRefreshCandidates {
    pub existing_fts_keys: HashSet<SearchIndexKey>,
    pub fresh_keys: HashSet<SearchIndexKey>,
    pub materialized_docs: HashMap<SearchIndexKey, OwnedSearchIndexDocument>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SearchIndexCandidate<'a> {
    pub provider: ProviderKind,
    pub thread_id: &'a str,
    pub source_fingerprint: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SearchIndexDocument<'a> {
    pub provider: ProviderKind,
    pub thread_id: &'a str,
    pub uri: &'a str,
    pub thread_source: &'a str,
    pub scope_path: Option<&'a str>,
    pub updated_epoch: Option<u64>,
    pub source_fingerprint: &'a str,
    pub search_text: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnedSearchIndexDocument {
    pub provider: ProviderKind,
    pub thread_id: String,
    pub uri: String,
    pub thread_source: String,
    pub scope_path: Option<String>,
    pub updated_epoch: Option<u64>,
    pub source_fingerprint: String,
    pub search_text: String,
}

impl OwnedSearchIndexDocument {
    fn as_document(&self) -> SearchIndexDocument<'_> {
        SearchIndexDocument {
            provider: self.provider,
            thread_id: &self.thread_id,
            uri: &self.uri,
            thread_source: &self.thread_source,
            scope_path: self.scope_path.as_deref(),
            updated_epoch: self.updated_epoch,
            source_fingerprint: &self.source_fingerprint,
            search_text: &self.search_text,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct IndexedMatch {
    pub provider: ProviderKind,
    pub thread_id: String,
    pub uri: String,
    pub updated_epoch: Option<u64>,
    pub matched_preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderManifestEntry {
    pub provider: ProviderKind,
    pub thread_id: String,
    pub uri: String,
    pub thread_source: String,
    pub scope_path: Option<String>,
}

#[derive(Debug)]
pub(crate) struct SearchIndex {
    conn: Connection,
    path: PathBuf,
}

impl SearchIndex {
    pub(crate) fn open_best_effort() -> (Option<Self>, Vec<String>) {
        if search_index_disabled() {
            return (None, Vec::new());
        }

        match Self::open() {
            Ok(index) => (Some(index), Vec::new()),
            Err(err) => (None, vec![format!("search index unavailable: {err}")]),
        }
    }

    fn open() -> Result<Self> {
        Self::open_at(search_index_path()?)
    }

    fn open_at(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| XurlError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        prepare_sqlite_cache_path(&path)?;

        let conn = Connection::open(&path).map_err(|source| XurlError::Sqlite {
            path: path.clone(),
            source,
        })?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|source| XurlError::Sqlite {
                path: path.clone(),
                source,
            })?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|source| XurlError::Sqlite {
                path: path.clone(),
                source,
            })?;
        conn.pragma_update(None, "temp_store", "MEMORY")
            .map_err(|source| XurlError::Sqlite {
                path: path.clone(),
                source,
            })?;

        let mut index = Self { conn, path };
        index.ensure_schema()?;
        Ok(index)
    }

    pub(crate) fn try_acquire_worker_lease(&mut self) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
        let now = now_epoch();
        let current = tx
            .query_row(
                "SELECT value FROM xurl_meta WHERE key = 'worker_lease_until'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?
            .and_then(|raw| raw.parse::<i64>().ok())
            .unwrap_or(0);

        if current > now {
            tx.rollback().map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
            return Ok(false);
        }

        let lease_until = now + i64::try_from(DEFAULT_WORKER_LEASE_SECS).unwrap_or(30);
        tx.execute(
            "INSERT OR REPLACE INTO xurl_meta (key, value) VALUES ('worker_lease_until', ?1)",
            [lease_until.to_string()],
        )
        .map_err(|source| XurlError::Sqlite {
            path: self.path.clone(),
            source,
        })?;
        tx.commit().map_err(|source| XurlError::Sqlite {
            path: self.path.clone(),
            source,
        })?;
        Ok(true)
    }

    pub(crate) fn release_worker_lease(&mut self) -> Result<()> {
        self.conn
            .execute("DELETE FROM xurl_meta WHERE key = 'worker_lease_until'", [])
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
        Ok(())
    }

    fn ensure_schema(&mut self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS xurl_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                 CREATE TABLE IF NOT EXISTS provider_watermarks (
                    provider TEXT PRIMARY KEY,
                    watermark TEXT NOT NULL,
                    refreshed_at_epoch INTEGER NOT NULL
                );
                 CREATE TABLE IF NOT EXISTS provider_manifest (
                    provider TEXT NOT NULL,
                    thread_id TEXT NOT NULL,
                    uri TEXT NOT NULL,
                    thread_source TEXT NOT NULL,
                    scope_path TEXT,
                    PRIMARY KEY(provider, thread_id)
                );
                 CREATE TABLE IF NOT EXISTS thread_materialization (
                    provider TEXT NOT NULL,
                    thread_id TEXT NOT NULL,
                    uri TEXT NOT NULL,
                    thread_source TEXT NOT NULL,
                    scope_path TEXT,
                    updated_epoch INTEGER,
                    source_fingerprint TEXT NOT NULL,
                    materialized_at_epoch INTEGER NOT NULL,
                    search_text TEXT NOT NULL,
                    PRIMARY KEY(provider, thread_id)
                );",
            )
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;

        self.conn
            .execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS thread_fts USING fts5(
                    provider UNINDEXED,
                    thread_id UNINDEXED,
                    uri UNINDEXED,
                    thread_source UNINDEXED,
                    scope_path UNINDEXED,
                    updated_epoch UNINDEXED,
                    source_fingerprint UNINDEXED,
                    indexed_at_epoch UNINDEXED,
                    search_text,
                    tokenize = 'trigram'
                );",
            )
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;

        self.conn
            .pragma_update(None, "user_version", INDEX_SQLITE_VERSION)
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;

        Ok(())
    }

    pub(crate) fn load_provider_manifest(
        &self,
        provider: ProviderKind,
        watermark: &str,
    ) -> Result<Option<Vec<ProviderManifestEntry>>> {
        let provider = provider.to_string();
        let current = self
            .conn
            .query_row(
                "SELECT watermark FROM provider_watermarks WHERE provider = ?1",
                [provider.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
        if current.as_deref() != Some(watermark) {
            return Ok(None);
        }

        let mut stmt = self
            .conn
            .prepare(
                "SELECT thread_id, uri, thread_source, scope_path
                 FROM provider_manifest
                 WHERE provider = ?1
                 ORDER BY thread_id",
            )
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map([provider.as_str()], |row| {
                Ok(ProviderManifestEntry {
                    provider: provider_from_str(&provider).expect("valid provider"),
                    thread_id: row.get::<_, String>(0)?,
                    uri: row.get::<_, String>(1)?,
                    thread_source: row.get::<_, String>(2)?,
                    scope_path: row.get::<_, Option<String>>(3)?,
                })
            })
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map(Some)
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) fn replace_provider_manifest(
        &mut self,
        provider: ProviderKind,
        watermark: &str,
        entries: &[ProviderManifestEntry],
    ) -> Result<()> {
        let provider = provider.to_string();
        let refreshed_at_epoch = now_epoch();
        let tx = self
            .conn
            .transaction()
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
        tx.execute(
            "DELETE FROM provider_manifest WHERE provider = ?1",
            [provider.as_str()],
        )
        .map_err(|source| XurlError::Sqlite {
            path: self.path.clone(),
            source,
        })?;
        {
            let mut insert_stmt = tx
                .prepare(
                    "INSERT INTO provider_manifest (
                        provider,
                        thread_id,
                        uri,
                        thread_source,
                        scope_path
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(|source| XurlError::Sqlite {
                    path: self.path.clone(),
                    source,
                })?;
            for entry in entries {
                insert_stmt
                    .execute(params![
                        provider.as_str(),
                        &entry.thread_id,
                        &entry.uri,
                        &entry.thread_source,
                        &entry.scope_path
                    ])
                    .map_err(|source| XurlError::Sqlite {
                        path: self.path.clone(),
                        source,
                    })?;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO provider_watermarks (
                provider,
                watermark,
                refreshed_at_epoch
             ) VALUES (?1, ?2, ?3)",
            params![provider.as_str(), watermark, refreshed_at_epoch],
        )
        .map_err(|source| XurlError::Sqlite {
            path: self.path.clone(),
            source,
        })?;
        tx.commit().map_err(|source| XurlError::Sqlite {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    pub(crate) fn is_fresh(
        &self,
        provider: ProviderKind,
        thread_id: &str,
        source_fingerprint: &str,
    ) -> Result<bool> {
        let provider = provider.to_string();
        let exists = self
            .conn
            .query_row(
                "SELECT 1
                 FROM thread_fts
                 WHERE provider = ?1
                   AND thread_id = ?2
                   AND source_fingerprint = ?3
                 LIMIT 1",
                params![provider, thread_id, source_fingerprint],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?
            .is_some();
        Ok(exists)
    }

    pub(crate) fn replace_document(&mut self, doc: SearchIndexDocument<'_>) -> Result<()> {
        let tx = self
            .conn
            .transaction()
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
        let path = self.path.clone();
        let owned = OwnedSearchIndexDocument {
            provider: doc.provider,
            thread_id: doc.thread_id.to_string(),
            uri: doc.uri.to_string(),
            thread_source: doc.thread_source.to_string(),
            scope_path: doc.scope_path.map(ToString::to_string),
            updated_epoch: doc.updated_epoch,
            source_fingerprint: doc.source_fingerprint.to_string(),
            search_text: doc.search_text.to_string(),
        };
        Self::write_documents(&path, &tx, std::slice::from_ref(&owned), true, true)?;
        tx.commit().map_err(|source| XurlError::Sqlite {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    pub(crate) fn prepare_matches(
        &mut self,
        candidates: &[SearchIndexCandidate<'_>],
        keyword: &str,
    ) -> Result<PreparedMatches> {
        let keyword = keyword.trim();
        if keyword.is_empty() || keyword.chars().count() < 3 || candidates.is_empty() {
            return Ok(PreparedMatches::default());
        }

        self.load_query_candidates(candidates)?;

        let fresh_keys = self.query_fresh_keys()?;
        let matched_previews = self.query_matched_previews(keyword)?;
        Ok(PreparedMatches {
            fresh_keys,
            matched_previews,
        })
    }

    pub(crate) fn query_matches(
        &self,
        provider: ProviderKind,
        keyword: &str,
        scope_path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<IndexedMatch>> {
        let keyword = keyword.trim();
        if keyword.is_empty() || keyword.chars().count() < 3 || limit == 0 {
            return Ok(Vec::new());
        }

        let like = format!("%{}%", escape_like(keyword));
        let provider = provider.to_string();
        let sql = if scope_path_prefix.is_some() {
            "SELECT provider, thread_id, uri, updated_epoch, search_text
             FROM thread_fts
             WHERE provider = ?1
               AND search_text LIKE ?2 ESCAPE '\\'
               AND (scope_path = ?3 OR scope_path LIKE ?4 ESCAPE '\\')
             ORDER BY updated_epoch DESC
             LIMIT ?5"
        } else {
            "SELECT provider, thread_id, uri, updated_epoch, search_text
             FROM thread_fts
             WHERE provider = ?1
               AND search_text LIKE ?2 ESCAPE '\\'
             ORDER BY updated_epoch DESC
             LIMIT ?3"
        };

        let mut owned = vec![
            rusqlite::types::Value::from(provider),
            rusqlite::types::Value::from(like),
        ];
        if let Some(scope_path_prefix) = scope_path_prefix {
            let escaped = escape_like(scope_path_prefix);
            owned.push(rusqlite::types::Value::from(scope_path_prefix.to_string()));
            owned.push(rusqlite::types::Value::from(format!("{escaped}/%")));
        }
        owned.push(rusqlite::types::Value::from(
            i64::try_from(limit).unwrap_or(i64::MAX),
        ));
        let params = owned.iter().map(|value| value as &dyn ToSql);

        let mut stmt = self.conn.prepare(sql).map_err(|source| XurlError::Sqlite {
            path: self.path.clone(),
            source,
        })?;
        let rows = stmt
            .query_map(params_from_iter(params), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;

        let mut matches = Vec::new();
        for row in rows {
            let (provider, thread_id, uri, updated_epoch, search_text) =
                row.map_err(|source| XurlError::Sqlite {
                    path: self.path.clone(),
                    source,
                })?;
            let Some(provider) = provider_from_str(&provider) else {
                continue;
            };
            let Some(matched_preview) =
                super::service::match_first_preview_in_text(&search_text, keyword)
            else {
                continue;
            };
            matches.push(IndexedMatch {
                provider,
                thread_id,
                uri,
                updated_epoch: updated_epoch.and_then(|value| u64::try_from(value).ok()),
                matched_preview,
            });
        }

        Ok(matches)
    }

    pub(crate) fn prepare_refresh_candidates(
        &mut self,
        candidates: &[SearchIndexCandidate<'_>],
    ) -> Result<PreparedRefreshCandidates> {
        if candidates.is_empty() {
            return Ok(PreparedRefreshCandidates::default());
        }
        self.load_query_candidates(candidates)?;
        let fresh_keys = self.query_fresh_keys()?;
        let stale_candidates = candidates
            .iter()
            .copied()
            .filter(|candidate| {
                !fresh_keys.contains(&SearchIndexKey {
                    provider: candidate.provider,
                    thread_id: candidate.thread_id.to_string(),
                })
            })
            .collect::<Vec<_>>();
        let materialized_docs = if stale_candidates.is_empty() {
            HashMap::new()
        } else {
            self.load_query_candidates(&stale_candidates)?;
            self.query_materialized_documents()?
        };
        Ok(PreparedRefreshCandidates {
            existing_fts_keys: self.query_existing_fts_keys()?,
            fresh_keys,
            materialized_docs,
        })
    }

    pub(crate) fn insert_documents(&mut self, docs: &[OwnedSearchIndexDocument]) -> Result<()> {
        self.write_documents_with_mode(docs, true, false)
    }

    pub(crate) fn replace_documents(&mut self, docs: &[OwnedSearchIndexDocument]) -> Result<()> {
        self.write_documents_with_mode(docs, true, true)
    }

    pub(crate) fn insert_materialized_documents(
        &mut self,
        docs: &[OwnedSearchIndexDocument],
    ) -> Result<()> {
        self.write_documents_with_mode(docs, false, false)
    }

    pub(crate) fn replace_materialized_documents(
        &mut self,
        docs: &[OwnedSearchIndexDocument],
    ) -> Result<()> {
        self.write_documents_with_mode(docs, false, true)
    }

    fn write_documents_with_mode(
        &mut self,
        docs: &[OwnedSearchIndexDocument],
        persist_materialization: bool,
        replace_existing: bool,
    ) -> Result<()> {
        if docs.is_empty() {
            return Ok(());
        }

        let tx = self
            .conn
            .transaction()
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
        let path = self.path.clone();
        Self::write_documents(&path, &tx, docs, persist_materialization, replace_existing)?;
        tx.commit().map_err(|source| XurlError::Sqlite {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    fn write_documents(
        path: &Path,
        tx: &rusqlite::Transaction<'_>,
        docs: &[OwnedSearchIndexDocument],
        persist_materialization: bool,
        replace_existing: bool,
    ) -> Result<()> {
        let mut delete_fts_stmt = if replace_existing {
            Some(
                tx.prepare("DELETE FROM thread_fts WHERE provider = ?1 AND thread_id = ?2")
                    .map_err(|source| XurlError::Sqlite {
                        path: path.to_path_buf(),
                        source,
                    })?,
            )
        } else {
            None
        };
        let mut insert_fts_stmt = tx
            .prepare(
                "INSERT INTO thread_fts (
                    provider,
                    thread_id,
                    uri,
                    thread_source,
                    scope_path,
                    updated_epoch,
                    source_fingerprint,
                    indexed_at_epoch,
                    search_text
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )
            .map_err(|source| XurlError::Sqlite {
                path: path.to_path_buf(),
                source,
            })?;
        let mut insert_materialization_stmt = if persist_materialization {
            Some(
                tx.prepare(
                    "INSERT OR REPLACE INTO thread_materialization (
                        provider,
                        thread_id,
                        uri,
                        thread_source,
                        scope_path,
                        updated_epoch,
                        source_fingerprint,
                        materialized_at_epoch,
                        search_text
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )
                .map_err(|source| XurlError::Sqlite {
                    path: path.to_path_buf(),
                    source,
                })?,
            )
        } else {
            None
        };
        let indexed_at_epoch = now_epoch();
        let materialized_at_epoch = indexed_at_epoch;
        for doc in docs {
            let doc = doc.as_document();
            let provider = doc.provider.to_string();
            let updated_epoch = doc
                .updated_epoch
                .and_then(|value| i64::try_from(value).ok());
            if let Some(delete_fts_stmt) = delete_fts_stmt.as_mut() {
                delete_fts_stmt
                    .execute(params![provider, doc.thread_id])
                    .map_err(|source| XurlError::Sqlite {
                        path: path.to_path_buf(),
                        source,
                    })?;
            }
            insert_fts_stmt
                .execute(params![
                    provider,
                    doc.thread_id,
                    doc.uri,
                    doc.thread_source,
                    doc.scope_path,
                    updated_epoch,
                    doc.source_fingerprint,
                    indexed_at_epoch,
                    doc.search_text
                ])
                .map_err(|source| XurlError::Sqlite {
                    path: path.to_path_buf(),
                    source,
                })?;
            if let Some(insert_materialization_stmt) = insert_materialization_stmt.as_mut() {
                insert_materialization_stmt
                    .execute(params![
                        provider,
                        doc.thread_id,
                        doc.uri,
                        doc.thread_source,
                        doc.scope_path,
                        updated_epoch,
                        doc.source_fingerprint,
                        materialized_at_epoch,
                        doc.search_text
                    ])
                    .map_err(|source| XurlError::Sqlite {
                        path: path.to_path_buf(),
                        source,
                    })?;
            }
        }
        Ok(())
    }

    fn query_fresh_keys(&self) -> Result<HashSet<SearchIndexKey>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DISTINCT f.provider, f.thread_id
                 FROM query_candidates q
                 JOIN thread_fts f
                   ON f.provider = q.provider
                  AND f.thread_id = q.thread_id
                  AND f.source_fingerprint = q.source_fingerprint",
            )
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;

        let mut keys = HashSet::new();
        for row in rows {
            let (provider, thread_id) = row.map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
            if let Some(provider) = provider_from_str(&provider) {
                keys.insert(SearchIndexKey {
                    provider,
                    thread_id,
                });
            }
        }

        Ok(keys)
    }

    fn query_existing_fts_keys(&self) -> Result<HashSet<SearchIndexKey>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DISTINCT f.provider, f.thread_id
                 FROM query_candidates q
                 JOIN thread_fts f
                   ON f.provider = q.provider
                  AND f.thread_id = q.thread_id",
            )
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;

        let mut keys = HashSet::new();
        for row in rows {
            let (provider, thread_id) = row.map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
            if let Some(provider) = provider_from_str(&provider) {
                keys.insert(SearchIndexKey {
                    provider,
                    thread_id,
                });
            }
        }

        Ok(keys)
    }

    fn query_matched_previews(&self, keyword: &str) -> Result<HashMap<SearchIndexKey, String>> {
        let like = format!("%{}%", escape_like(keyword));
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DISTINCT f.provider, f.thread_id, f.search_text
                 FROM query_candidates q
                 JOIN thread_fts f
                   ON f.provider = q.provider
                  AND f.thread_id = q.thread_id
                  AND f.source_fingerprint = q.source_fingerprint
                 WHERE f.search_text LIKE ?1 ESCAPE '\\'",
            )
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map([like], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;

        let mut matches = HashMap::new();
        for row in rows {
            let (provider, thread_id, search_text) = row.map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
            let Some(provider) = provider_from_str(&provider) else {
                continue;
            };
            if let Some(preview) =
                super::service::match_first_preview_in_text(&search_text, keyword)
            {
                matches.insert(
                    SearchIndexKey {
                        provider,
                        thread_id,
                    },
                    preview,
                );
            }
        }

        Ok(matches)
    }

    fn query_materialized_documents(
        &self,
    ) -> Result<HashMap<SearchIndexKey, OwnedSearchIndexDocument>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DISTINCT m.provider, m.thread_id, m.uri, m.thread_source, m.scope_path,
                        m.updated_epoch, m.source_fingerprint, m.search_text
                 FROM query_candidates q
                 JOIN thread_materialization m
                   ON m.provider = q.provider
                  AND m.thread_id = q.thread_id
                  AND m.source_fingerprint = q.source_fingerprint",
            )
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;

        let mut docs = HashMap::new();
        for row in rows {
            let (
                provider,
                thread_id,
                uri,
                thread_source,
                scope_path,
                updated_epoch,
                source_fingerprint,
                search_text,
            ) = row.map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
            let Some(provider) = provider_from_str(&provider) else {
                continue;
            };
            let key = SearchIndexKey {
                provider,
                thread_id: thread_id.clone(),
            };
            docs.insert(
                key,
                OwnedSearchIndexDocument {
                    provider,
                    thread_id,
                    uri,
                    thread_source,
                    scope_path,
                    updated_epoch: updated_epoch.and_then(|value| u64::try_from(value).ok()),
                    source_fingerprint,
                    search_text,
                },
            );
        }

        Ok(docs)
    }

    fn load_query_candidates(&mut self, candidates: &[SearchIndexCandidate<'_>]) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TEMP TABLE IF NOT EXISTS query_candidates (
                    provider TEXT NOT NULL,
                    thread_id TEXT NOT NULL,
                    source_fingerprint TEXT NOT NULL
                );
                 DELETE FROM query_candidates;",
            )
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;

        let tx = self
            .conn
            .transaction()
            .map_err(|source| XurlError::Sqlite {
                path: self.path.clone(),
                source,
            })?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO query_candidates (provider, thread_id, source_fingerprint)
                     VALUES (?1, ?2, ?3)",
                )
                .map_err(|source| XurlError::Sqlite {
                    path: self.path.clone(),
                    source,
                })?;

            for candidate in candidates {
                stmt.execute(params![
                    candidate.provider.to_string(),
                    candidate.thread_id,
                    candidate.source_fingerprint
                ])
                .map_err(|source| XurlError::Sqlite {
                    path: self.path.clone(),
                    source,
                })?;
            }
        }
        tx.commit().map_err(|source| XurlError::Sqlite {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }
}

fn prepare_sqlite_cache_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    let conn = match Connection::open(path) {
        Ok(conn) => conn,
        Err(_) => {
            remove_sqlite_cache_family(path)?;
            return Ok(());
        }
    };
    let version = conn
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .unwrap_or_default();
    drop(conn);

    if version != INDEX_SQLITE_VERSION {
        remove_sqlite_cache_family(path)?;
    }

    Ok(())
}

fn remove_sqlite_cache_family(path: &Path) -> Result<()> {
    for candidate in sqlite_cache_family_paths(path) {
        if let Err(source) = fs::remove_file(&candidate)
            && source.kind() != std::io::ErrorKind::NotFound
        {
            return Err(XurlError::Io {
                path: candidate,
                source,
            });
        }
    }
    Ok(())
}

fn sqlite_cache_family_paths(path: &Path) -> [PathBuf; 3] {
    let base = path.to_string_lossy();
    [
        path.to_path_buf(),
        PathBuf::from(format!("{base}-wal")),
        PathBuf::from(format!("{base}-shm")),
    ]
}

fn search_index_disabled() -> bool {
    env::var_os("XURL_DISABLE_SEARCH_INDEX")
        .filter(|value| !value.is_empty() && value != "0")
        .is_some()
}

fn search_index_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("XURL_SEARCH_INDEX_PATH").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    let home = home_dir().ok_or(XurlError::HomeDirectoryNotFound)?;
    let state_root = env::var_os("XDG_STATE_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state"));
    Ok(state_root.join("xurl/search-index-v1.sqlite3"))
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn provider_from_str(raw: &str) -> Option<ProviderKind> {
    match raw {
        "amp" => Some(ProviderKind::Amp),
        "copilot" => Some(ProviderKind::Copilot),
        "codex" => Some(ProviderKind::Codex),
        "claude" => Some(ProviderKind::Claude),
        "cursor" => Some(ProviderKind::Cursor),
        "gemini" => Some(ProviderKind::Gemini),
        "kimi" => Some(ProviderKind::Kimi),
        "pi" => Some(ProviderKind::Pi),
        "opencode" => Some(ProviderKind::Opencode),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recreates_outdated_database_before_open() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("search-index.sqlite3");
        let conn = Connection::open(&path).expect("open sqlite");
        conn.pragma_update(None, "user_version", 1_i64)
            .expect("set old version");
        conn.execute("CREATE TABLE stale (id INTEGER PRIMARY KEY)", [])
            .expect("create stale table");
        drop(conn);

        let index = SearchIndex::open_at(path.clone()).expect("open search index");
        drop(index);

        let conn = Connection::open(&path).expect("reopen sqlite");
        let user_version = conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("read user_version");
        let stale_exists = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'stale'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("query sqlite_master");

        assert_eq!(user_version, INDEX_SQLITE_VERSION);
        assert_eq!(stale_exists, 0);
    }

    #[test]
    fn round_trips_provider_manifest() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("search-index.sqlite3");
        let mut index = SearchIndex::open_at(path).expect("open search index");
        let entry = ProviderManifestEntry {
            provider: ProviderKind::Codex,
            thread_id: "thread-1".to_string(),
            uri: "agents://codex/thread-1".to_string(),
            thread_source: "/tmp/thread-1.jsonl".to_string(),
            scope_path: Some("/tmp/workspace".to_string()),
        };

        index
            .replace_provider_manifest(
                ProviderKind::Codex,
                "watermark-1",
                std::slice::from_ref(&entry),
            )
            .expect("replace provider manifest");

        let loaded = index
            .load_provider_manifest(ProviderKind::Codex, "watermark-1")
            .expect("load provider manifest");
        let stale = index
            .load_provider_manifest(ProviderKind::Codex, "watermark-2")
            .expect("load stale provider manifest");

        assert_eq!(loaded, Some(vec![entry]));
        assert_eq!(stale, None);
    }

    #[test]
    fn reuses_materialized_document_when_fts_rows_are_missing() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("search-index.sqlite3");
        let mut index = SearchIndex::open_at(path).expect("open search index");
        index
            .replace_document(SearchIndexDocument {
                provider: ProviderKind::Codex,
                thread_id: "thread-1",
                uri: "agents://codex/thread-1",
                thread_source: "/tmp/thread-1.jsonl",
                scope_path: Some("/tmp/workspace"),
                updated_epoch: Some(42),
                source_fingerprint: "file:42:1024",
                search_text: "hello materialized world",
            })
            .expect("replace search document");

        index
            .conn
            .execute("DELETE FROM thread_fts", [])
            .expect("delete thread fts rows");

        let prepared = index
            .prepare_refresh_candidates(&[SearchIndexCandidate {
                provider: ProviderKind::Codex,
                thread_id: "thread-1",
                source_fingerprint: "file:42:1024",
            }])
            .expect("prepare refresh candidates");

        assert!(prepared.fresh_keys.is_empty());
        assert_eq!(
            prepared.materialized_docs.get(&SearchIndexKey {
                provider: ProviderKind::Codex,
                thread_id: "thread-1".to_string(),
            }),
            Some(&OwnedSearchIndexDocument {
                provider: ProviderKind::Codex,
                thread_id: "thread-1".to_string(),
                uri: "agents://codex/thread-1".to_string(),
                thread_source: "/tmp/thread-1.jsonl".to_string(),
                scope_path: Some("/tmp/workspace".to_string()),
                updated_epoch: Some(42),
                source_fingerprint: "file:42:1024".to_string(),
                search_text: "hello materialized world".to_string(),
            })
        );
    }
}
