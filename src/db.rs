use crate::client::Post;
use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const FALLBACK_INSTANCES: &[&str] = &[
    "http://75.119.154.243:8081",
    "https://red.artemislena.eu",
    "https://redlib.privacyredirect.com",
    "https://redlib.privadency.com",
    "https://redlib.catsarch.com",
    "https://redlib.nadeko.net",
    "https://redlib.perennialte.ch",
    "https://redlib.r4fo.com",
    "https://redlib.cow.rip",
];

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn db_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME not set");
    let dir = PathBuf::from(home).join(".config").join("reddit");
    std::fs::create_dir_all(&dir).expect("failed to create ~/.config/reddit");
    dir.join("reddit.db")
}

pub struct CachePolicy {
    pub fresh_secs: i64,
    pub stale_secs: i64,
    pub race_ms: u64,
}

pub struct Db {
    conn: Connection,
}

pub struct InstanceInfo {
    pub url: String,
    pub success_count: u32,
    pub failure_count: u32,
    pub avg_latency_ms: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Default)]
pub struct Stats {
    pub total_fetches: u64,
    pub total_instance_queries: u64,
    pub total_cache_hits: u64,
    pub total_stale_hits: u64,
}

pub struct CacheEntry {
    pub posts: Vec<Post>,
    pub age_secs: i64,
}

pub struct CacheSummary {
    pub key: String,
    pub result_count: i32,
    pub age_secs: i64,
}

pub struct CacheStats {
    pub entry_count: u64,
    pub total_results: u64,
    pub db_size_bytes: u64,
    pub oldest_entry_secs: Option<i64>,
    pub newest_entry_secs: Option<i64>,
}

impl Db {
    pub fn open() -> Self {
        let path = db_path();
        let conn = Connection::open(&path).expect("failed to open database");

        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;",
        )
        .expect("failed to set pragmas");

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS instances (
                url TEXT PRIMARY KEY,
                last_success INTEGER,
                last_failure INTEGER,
                last_error TEXT,
                success_count INTEGER DEFAULT 0,
                failure_count INTEGER DEFAULT 0,
                avg_latency_ms INTEGER,
                reachable INTEGER DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS cache (
                key TEXT PRIMARY KEY,
                posts_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                result_count INTEGER DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS stats (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                total_fetches INTEGER DEFAULT 0,
                total_instance_queries INTEGER DEFAULT 0,
                total_cache_hits INTEGER DEFAULT 0,
                total_stale_hits INTEGER DEFAULT 0
            );
            INSERT OR IGNORE INTO stats (id) VALUES (1);",
        )
        .expect("failed to create schema");

        let db = Db { conn };
        db.seed_fallback();
        db.maybe_cleanup();
        db
    }

    fn seed_fallback(&self) {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM instances", [], |r| r.get(0))
            .unwrap_or(0);

        if count == 0 {
            let mut stmt = self
                .conn
                .prepare("INSERT OR IGNORE INTO instances (url) VALUES (?1)")
                .unwrap();
            for url in FALLBACK_INSTANCES {
                stmt.execute(params![url]).ok();
            }
        }
    }

    fn maybe_cleanup(&self) {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT total_fetches FROM stats WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if n > 0 && n % 50 == 0 {
            self.cleanup();
        }
    }

    pub fn cleanup(&self) {
        let cutoff = now() - 7200;
        self.conn
            .execute("DELETE FROM cache WHERE created_at < ?1", params![cutoff])
            .ok();
    }

    pub fn pick_instances(&self, n: usize) -> Vec<String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT url, success_count, failure_count, avg_latency_ms, last_failure
                 FROM instances WHERE reachable = 1",
            )
            .unwrap();

        let mut scored: Vec<(String, f64)> = stmt
            .query_map([], |row| {
                let url: String = row.get(0)?;
                let succ: f64 = row.get::<_, i64>(1)? as f64;
                let fail: f64 = row.get::<_, i64>(2)? as f64;
                let latency: f64 = row.get::<_, Option<i64>>(3)?.unwrap_or(1000) as f64;
                let last_fail: Option<i64> = row.get(4)?;

                let total = succ + fail;
                let ratio = if total > 0.0 { succ / total } else { 0.5 };
                let recency_penalty = match last_fail {
                    Some(ts) => {
                        let age = (now() - ts) as f64;
                        if age < 300.0 {
                            20.0
                        } else {
                            0.0
                        }
                    }
                    None => 0.0,
                };
                let jitter = rand::random::<f64>() * 10.0;
                let score = ratio * 100.0 - latency / 50.0 - recency_penalty + jitter;

                Ok((url, score))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        scored.into_iter().take(n).map(|(url, _)| url).collect()
    }

    pub fn record_success(&self, url: &str, latency_ms: u64) {
        self.conn
            .execute(
                "UPDATE instances SET
                last_success = ?2,
                success_count = success_count + 1,
                reachable = 1,
                avg_latency_ms = CASE
                    WHEN avg_latency_ms IS NULL THEN ?3
                    ELSE (avg_latency_ms * 3 + ?3) / 4
                END
             WHERE url = ?1",
                params![url, now(), latency_ms as i64],
            )
            .ok();
    }

    pub fn record_failure(&self, url: &str, error: &str) {
        self.conn
            .execute(
                "UPDATE instances SET
                last_failure = ?2,
                last_error = ?3,
                failure_count = failure_count + 1
             WHERE url = ?1",
                params![url, now(), error],
            )
            .ok();
    }

    pub fn mark_unreachable(&self, url: &str) {
        self.conn
            .execute(
                "UPDATE instances SET reachable = 0 WHERE url = ?1",
                params![url],
            )
            .ok();
    }

    pub fn cache_load(&self, key: &str) -> Option<CacheEntry> {
        let (json, created_at): (String, i64) = self
            .conn
            .query_row(
                "SELECT posts_json, created_at FROM cache WHERE key = ?1",
                params![key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok()?;
        let posts: Vec<Post> = serde_json::from_str(&json).ok()?;
        let age_secs = now() - created_at;
        Some(CacheEntry { posts, age_secs })
    }

    pub fn cache_put(&self, key: &str, posts: &[Post]) {
        let json = serde_json::to_string(posts).unwrap();
        let count = posts.len() as i64;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO cache (key, posts_json, created_at, result_count)
             VALUES (?1, ?2, ?3, ?4)",
                params![key, json, now(), count],
            )
            .ok();
    }

    pub fn cache_list(&self) -> Vec<CacheSummary> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, result_count, created_at FROM cache ORDER BY created_at DESC")
            .unwrap();

        let n = now();
        stmt.query_map([], |row| {
            let created_at: i64 = row.get(2)?;
            Ok(CacheSummary {
                key: row.get(0)?,
                result_count: row.get(1)?,
                age_secs: n - created_at,
            })
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn cache_clear_all(&self) -> usize {
        self.conn.execute("DELETE FROM cache", []).unwrap_or(0)
    }

    pub fn cache_clear_query(&self, key: &str) -> usize {
        self.conn
            .execute("DELETE FROM cache WHERE key = ?1", params![key])
            .unwrap_or(0)
    }

    pub fn cache_stats(&self) -> CacheStats {
        let entry_count: u64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM cache", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as u64;

        let total_results: u64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(result_count), 0) FROM cache",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64;

        let page_count: u64 = self
            .conn
            .query_row("PRAGMA page_count", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as u64;
        let page_size: u64 = self
            .conn
            .query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0))
            .unwrap_or(4096) as u64;
        let db_size_bytes = page_count * page_size;

        let n = now();
        let oldest: Option<i64> = self
            .conn
            .query_row("SELECT MIN(created_at) FROM cache", [], |r| r.get(0))
            .unwrap_or(None);
        let newest: Option<i64> = self
            .conn
            .query_row("SELECT MAX(created_at) FROM cache", [], |r| r.get(0))
            .unwrap_or(None);

        CacheStats {
            entry_count,
            total_results,
            db_size_bytes,
            oldest_entry_secs: oldest.map(|t| n - t),
            newest_entry_secs: newest.map(|t| n - t),
        }
    }

    pub fn bump_stats(&self, fetches: u64, queries: u64, cache_hits: u64, stale_hits: u64) {
        self.conn
            .execute(
                "UPDATE stats SET
                total_fetches = total_fetches + ?1,
                total_instance_queries = total_instance_queries + ?2,
                total_cache_hits = total_cache_hits + ?3,
                total_stale_hits = total_stale_hits + ?4
             WHERE id = 1",
                params![
                    fetches as i64,
                    queries as i64,
                    cache_hits as i64,
                    stale_hits as i64
                ],
            )
            .ok();
    }

    pub fn stats(&self) -> Stats {
        self.conn
            .query_row(
                "SELECT total_fetches, total_instance_queries, total_cache_hits, total_stale_hits
             FROM stats WHERE id = 1",
                [],
                |row| {
                    Ok(Stats {
                        total_fetches: row.get::<_, i64>(0)? as u64,
                        total_instance_queries: row.get::<_, i64>(1)? as u64,
                        total_cache_hits: row.get::<_, i64>(2)? as u64,
                        total_stale_hits: row.get::<_, i64>(3)? as u64,
                    })
                },
            )
            .unwrap_or_default()
    }

    pub fn all_instances(&self) -> Vec<InstanceInfo> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT url, success_count, failure_count, avg_latency_ms,
                        last_success, last_failure, last_error
                 FROM instances ORDER BY success_count DESC",
            )
            .unwrap();

        stmt.query_map([], |row| {
            Ok(InstanceInfo {
                url: row.get(0)?,
                success_count: row.get::<_, i64>(1)? as u32,
                failure_count: row.get::<_, i64>(2)? as u32,
                avg_latency_ms: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                last_error: row.get(6)?,
            })
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn add_instance(&self, url: &str) {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO instances (url) VALUES (?1)",
                params![url],
            )
            .ok();
    }

    pub fn vacuum(&self) {
        self.conn.execute_batch("VACUUM;").ok();
    }
}
