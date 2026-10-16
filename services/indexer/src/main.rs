//! `shortwin-indexer` (INDEXER.md §12): one binary, one subcommand per process role.
//!
//! ```text
//! shortwin-indexer [--config indexer.toml] <command>
//!   migrate                    apply the migrations (the ingest role's DDL rights, §12.1)
//!   backfill                   every signature stream back to its start, at finalized (§2.2)
//!   live                       live ingest at confirmed + finality tracker + refresher (§2.3)
//!   serve                      the REST + WebSocket API (§9)
//!   rollups                    the analytics job (§8.6)
//!   run                        live + rollups + serve in one process (small deployments)
//!   replay <session.jsonl>     ingest a recorded session (§11.3)
//!   rebuild [--market <key>]   recompute projections from events (§3.4)
//! ```
//!
//! `DATABASE_URL`, `SHORTWIN_RPC_URL`, `SHORTWIN_WS_URL` override the file; logs go to stderr
//! (`RUST_LOG`, default `info`). Owner: INGEST (the dispatcher); the API and ANALYTICS commands
//! call their modules' entry points.

use std::path::PathBuf;

use anyhow::{Context, bail};
use shortwin_indexer::{IndexerConfig, analytics, api, db, ingest};

fn usage() -> &'static str {
    "usage: shortwin-indexer [--config indexer.toml] \
     <migrate | backfill | live | serve | rollups | run | replay <session.jsonl> | \
     rebuild [--market <pubkey>]>"
}

enum Command {
    Migrate,
    Backfill,
    Live,
    Serve,
    Rollups,
    Run,
    Replay(PathBuf),
    Rebuild(Option<String>),
}

fn parse_args() -> anyhow::Result<(Option<PathBuf>, Command)> {
    let mut config = None;
    let mut command = None;
    let mut market = None;
    let mut session = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => {
                config = Some(PathBuf::from(it.next().context("--config needs a value")?))
            }
            "--market" => market = Some(it.next().context("--market needs a value")?),
            "-h" | "--help" => bail!("{}", usage()),
            other if command.is_none() => command = Some(other.to_string()),
            other if command.as_deref() == Some("replay") && session.is_none() => {
                session = Some(PathBuf::from(other));
            }
            other => bail!("unexpected argument {other}\n{}", usage()),
        }
    }
    let cmd = match command.as_deref() {
        Some("migrate") => Command::Migrate,
        Some("backfill") => Command::Backfill,
        Some("live") => Command::Live,
        Some("serve") => Command::Serve,
        Some("rollups") => Command::Rollups,
        Some("run") => Command::Run,
        Some("replay") => Command::Replay(session.context("replay needs a session file")?),
        Some("rebuild") => Command::Rebuild(market),
        Some(other) => bail!("unknown command {other}\n{}", usage()),
        None => bail!("{}", usage()),
    };
    Ok((config, cmd))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let (config, cmd) = parse_args()?;
    let cfg = IndexerConfig::load(config.as_deref())?;
    let pool = db::connect(&cfg.database).await?;
    if cfg.database.migrate_on_start || matches!(cmd, Command::Migrate) {
        db::migrate(&pool).await?;
    }
    match cmd {
        Command::Migrate => Ok(()),
        Command::Backfill => {
            let report = ingest::run_backfill(&cfg, &pool).await?;
            tracing::info!(?report, "backfill complete");
            Ok(())
        }
        Command::Live => ingest::run_live(&cfg, &pool).await,
        Command::Serve => api::serve(&cfg, pool).await,
        Command::Rollups => analytics::run_jobs(&cfg, &pool).await,
        Command::Run => {
            // The first role that stops ends the process (a supervisor restarts it): an API
            // serving a database whose ingest died would go stale silently.
            tokio::try_join!(
                ingest::run_live(&cfg, &pool),
                analytics::run_jobs(&cfg, &pool),
                api::serve(&cfg, pool.clone()),
            )?;
            Ok(())
        }
        Command::Replay(path) => {
            let report = ingest::replay_file(&cfg, &pool, &path).await?;
            tracing::info!(?report, "replay complete");
            Ok(())
        }
        Command::Rebuild(market) => {
            let market = market
                .map(|m| shortwin_indexer::numeric::parse_key(&m))
                .transpose()?;
            let n = ingest::rebuild(&pool, market).await?;
            tracing::info!(markets = n, "rebuild complete");
            Ok(())
        }
    }
}
