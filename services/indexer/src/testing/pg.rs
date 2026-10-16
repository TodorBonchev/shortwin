//! The embedded-Postgres test fixture (INDEXER.md §11.1). FOUNDATION.
//!
//! [`TestPg::start`] runs a real PostgreSQL server as the current user through
//! `postgresql_embedded`: the binaries ([`PG_VERSION`], the zonky "embedded-postgres-binaries"
//! builds from Maven Central, which bundle their shared libraries and need only glibc) are
//! downloaded once (~15 MB) into the user cache — `$SHORTWIN_PG_INSTALL_DIR`, else
//! `$XDG_CACHE_HOME/shortwin/postgresql-zonky`, else `~/.cache/shortwin/postgresql-zonky` — and
//! reused; the data directory is a fresh directory under the data root (`$SHORTWIN_PG_TMP`, else
//! the given root, else the system temp dir), removed when the fixture drops. No Docker, no
//! root, no system Postgres.
//!
//! Each test takes its own database ([`TestPg::fresh_db`]: created, migrated with
//! [`crate::db::MIGRATOR`]), so tests in one binary can share a server or start their own.
//! The server is stopped (`pg_ctl stop -m fast`) when the [`TestPg`] drops.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::Context;
use postgresql_embedded::{PostgreSQL, Settings, VersionReq};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// The PostgreSQL release the fixture runs (the docker-compose deployment runs the same major,
/// §12.1).
pub const PG_VERSION: &str = "=17.11.0";

/// `postgresql_archive`'s registry key for the zonky builds (its `configuration::zonky::URL`;
/// the archives themselves come from `repo1.maven.org/maven2/io/zonky/test/postgres`).
const ZONKY_RELEASES: &str = "https://github.com/zonkyio/embedded-postgres-binaries";

/// Serialises first-time installs within one process (`postgresql_archive` already locks
/// across processes).
static SETUP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static DB_SEQ: AtomicU32 = AtomicU32::new(0);

/// A running PostgreSQL server (stopped, and its data removed, on drop).
pub struct TestPg {
    pg: Option<PostgreSQL>,
    data_root: PathBuf,
}

/// Where the binaries are cached.
pub fn install_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SHORTWIN_PG_INSTALL_DIR") {
        return PathBuf::from(dir);
    }
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    cache.join("shortwin").join("postgresql-zonky")
}

impl TestPg {
    /// Starts a server with its data under `$SHORTWIN_PG_TMP` or the system temp dir.
    pub async fn start() -> anyhow::Result<Self> {
        let root = std::env::var_os("SHORTWIN_PG_TMP")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        Self::start_in(&root).await
    }

    /// Starts a server with its data in a fresh directory under `root` (an integration test
    /// passes `env!("CARGO_TARGET_TMPDIR")`; `$SHORTWIN_PG_TMP` still wins).
    pub async fn start_in(root: &Path) -> anyhow::Result<Self> {
        let root = std::env::var_os("SHORTWIN_PG_TMP")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.to_path_buf());
        let n = DB_SEQ.fetch_add(1, Ordering::Relaxed);
        let data_root = root.join(format!("shortwin-pg-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&data_root)
            .with_context(|| format!("creating {}", data_root.display()))?;
        // Built field by field: `Settings::new()` would leave two empty temp directories behind
        // on every start (its default password file and data directory).
        let configuration = [
            // A disposable test server: durability off.
            ("fsync", "off"),
            ("synchronous_commit", "off"),
            ("full_page_writes", "off"),
            ("max_connections", "200"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let settings = Settings {
            releases_url: ZONKY_RELEASES.to_string(),
            version: VersionReq::parse(PG_VERSION)?,
            installation_dir: install_dir(),
            password_file: data_root.join(".pgpass"),
            data_dir: data_root.join("data"),
            host: "127.0.0.1".to_string(),
            port: 0,
            username: postgresql_embedded::BOOTSTRAP_SUPERUSER.to_string(),
            // A throwaway local test server (127.0.0.1, removed on drop).
            password: format!("shortwin-test-{}-{n}", std::process::id()),
            temporary: true,
            // initdb and the first start can take a while on a loaded machine.
            timeout: Some(Duration::from_secs(120)),
            configuration,
            trust_installation_dir: false,
            socket_dir: None,
        };
        let mut pg = PostgreSQL::new(settings);
        let started: anyhow::Result<()> = async {
            {
                let _guard = SETUP.lock().await;
                pg.setup()
                    .await
                    .context("installing / initialising the embedded PostgreSQL")?;
            }
            pg.start().await.context("starting the embedded PostgreSQL")
        }
        .await;
        if let Err(e) = started {
            drop(pg);
            let _ = std::fs::remove_dir_all(&data_root);
            return Err(e);
        }
        Ok(Self {
            pg: Some(pg),
            data_root,
        })
    }

    fn server(&self) -> &PostgreSQL {
        // Only `stop` and `drop` take the server out, and both consume the fixture.
        self.pg
            .as_ref()
            .unwrap_or_else(|| unreachable!("the server is running"))
    }

    /// The connection URL of database `db`.
    pub fn url(&self, db: &str) -> String {
        self.server().settings().url(db)
    }

    /// Creates an empty database `name` and returns a pool on it.
    pub async fn create_db(&self, name: &str) -> anyhow::Result<PgPool> {
        self.server()
            .create_database(name)
            .await
            .with_context(|| format!("creating database {name}"))?;
        PgPoolOptions::new()
            .max_connections(8)
            .connect(&self.url(name))
            .await
            .with_context(|| format!("connecting to {name}"))
    }

    /// Creates database `name` and applies every migration.
    pub async fn migrated_db(&self, name: &str) -> anyhow::Result<PgPool> {
        let pool = self.create_db(name).await?;
        crate::db::migrate(&pool).await?;
        Ok(pool)
    }

    /// A fresh migrated database with a unique name.
    pub async fn fresh_db(&self) -> anyhow::Result<PgPool> {
        let n = DB_SEQ.fetch_add(1, Ordering::Relaxed);
        self.migrated_db(&format!("shortwin_test_{n}")).await
    }

    /// Stops the server now (dropping does the same, blocking).
    pub async fn stop(mut self) -> anyhow::Result<()> {
        if let Some(pg) = self.pg.take() {
            pg.stop()
                .await
                .context("stopping the embedded PostgreSQL")?;
        }
        Ok(())
    }
}

impl Drop for TestPg {
    fn drop(&mut self) {
        // `PostgreSQL`'s own drop stops a running server and removes its data directory.
        drop(self.pg.take());
        let _ = std::fs::remove_dir_all(&self.data_root);
    }
}
