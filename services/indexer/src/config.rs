//! The indexer's configuration file (TOML; INDEXER.md §12.2). FOUNDATION: the top-level shape is
//! frozen; each section's struct lives in its owner's module (`ingest::IngestConfig`,
//! `api::ApiConfig`, `analytics::AnalyticsConfig`), so a track edits only its own section.
//!
//! Every key is optional (`indexer.example.toml` lists the defaults); `DATABASE_URL`,
//! `SHORTWIN_RPC_URL` and `SHORTWIN_WS_URL` override the file (12-factor deployments, §12).

use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

/// The whole configuration file.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexerConfig {
    pub database: DatabaseConfig,
    pub ingest: crate::ingest::IngestConfig,
    pub api: crate::api::ApiConfig,
    pub analytics: crate::analytics::AnalyticsConfig,
}

/// `[database]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    /// `postgres://user:password@host:port/db` (`DATABASE_URL` overrides).
    pub url: String,
    /// Pool size of this process.
    pub max_connections: u32,
    /// Run the migrations at start (`migrate` does it explicitly; the API role may lack DDL
    /// rights, §12.1).
    pub migrate_on_start: bool,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "postgres://shortwin@localhost:5432/shortwin".to_string(),
            max_connections: 16,
            migrate_on_start: false,
        }
    }
}

impl IndexerConfig {
    /// Reads `path` (the defaults when `None`), then applies the environment overrides.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let mut cfg: Self = match path {
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .with_context(|| format!("reading {}", p.display()))?;
                toml::from_str(&text).with_context(|| format!("parsing {}", p.display()))?
            }
            None => Self::default(),
        };
        if let Ok(url) = std::env::var("DATABASE_URL") {
            cfg.database.url = url;
        }
        if let Ok(url) = std::env::var("SHORTWIN_RPC_URL") {
            cfg.ingest.rpc_url = url;
        }
        if let Ok(url) = std::env::var("SHORTWIN_WS_URL") {
            cfg.ingest.ws_url = url;
        }
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_file_parses_and_matches_the_defaults_shape() {
        let text = include_str!("../indexer.example.toml");
        let cfg: IndexerConfig = toml::from_str(text).expect("indexer.example.toml parses");
        assert!(cfg.database.url.starts_with("postgres://"));
        assert!(cfg.api.max_page_limit >= cfg.api.default_page_limit);
        // The example lists every key with its default: a new key or a changed default that is
        // not mirrored in the example fails here.
        assert_eq!(
            format!("{cfg:?}"),
            format!("{:?}", IndexerConfig::default()),
            "indexer.example.toml drifted from the defaults"
        );
    }

    /// The straddle helper's ingestion is retired (design pump-final §9, owner decision D2): a
    /// configuration that still names `complete_curve_id` fails to load instead of being ignored.
    #[test]
    fn a_config_naming_the_retired_helper_is_refused() {
        let e = toml::from_str::<IndexerConfig>(
            "[ingest]\ncomplete_curve_id = \"4B5mGykX9EWwaYdbTvsp8qXCUTbc7caQourz16Gat6LJ\"\n",
        )
        .expect_err("refused");
        assert!(
            e.to_string().contains("unknown field `complete_curve_id`"),
            "{e}"
        );
    }

    #[test]
    fn the_reference_deployment_config_parses() {
        let text = include_str!("../deploy/indexer.toml");
        let cfg: IndexerConfig = toml::from_str(text).expect("deploy/indexer.toml parses");
        assert!(cfg.api.max_page_limit >= cfg.api.default_page_limit);
        cfg.api.check().expect("deploy/indexer.toml [api]");
        cfg.ingest.check().expect("deploy/indexer.toml [ingest]");
        // M8-SEC-8: the reference deployment puts a load balancer in front, so the balancer's
        // address must be trusted for X-Forwarded-For (else every client shares one bucket), and
        // the API ports are reachable only through it.
        assert!(
            !cfg.api.trusted_proxies.is_empty(),
            "deploy/indexer.toml must list the load balancer in [api] trusted_proxies"
        );
        let compose = include_str!("../deploy/docker-compose.yml");
        assert!(compose.contains("\"127.0.0.1:8080-8081:8080\""));
        for p in &cfg.api.trusted_proxies {
            assert!(
                compose.contains(&format!("gateway: {p}")),
                "trusted proxy {p} is the compose network's gateway"
            );
        }
    }
}
