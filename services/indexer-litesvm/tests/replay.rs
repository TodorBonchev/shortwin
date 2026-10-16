//! The M8 replay harness (INDEXER.md §11.3, §11.4; ANALYTICS + REPLAY): LiteSVM sessions
//! recorded at test time from the current program builds, then
//!
//! - the session format: every session round-trips through its JSON Lines file, and every
//!   transaction decodes back from its `getTransaction` JSON;
//! - the contract's own rules against the program: the harness's independent decoding and
//!   reference model of INDEXER.md §5.3 / §6 equals the chain at every checkpoint (market
//!   accounting, both SOL ledgers of §6.4, positions and their locked terms, lenders across
//!   share epochs, share-epoch records);
//! - the DB-side assertions (`assert`) on the harness's reference writer (`seed`): they hold on
//!   a faithful database and each catches a tampered row; the analytics views and the hourly
//!   rollups reproduce the model's numbers on the recorded sessions;
//! - §11.4 in full through the INGEST pipeline (`ingest::ingest_session`, `ingest::rebuild`, the
//!   finality tracker's rollback, the rollups), plus the API's read-model assumptions — the M8
//!   exit criterion "replays an E2E session into correct DB state" (ARCH §9);
//! - the catalog: across the session set every event kind the program emits appears.
//!
//! `SHORTWIN_RECORD_SESSIONS=<dir>` also writes every session into `<dir>`.

use std::path::PathBuf;
use std::sync::OnceLock;

use shortwin_indexer::ingest::session::{self, Session};
use shortwin_indexer::testing::pg::TestPg;
use shortwin_indexer_litesvm::chain::{self, ChainState, Model};
use shortwin_indexer_litesvm::{assert as check, recorder, scenarios, seed};

fn tmp() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
}

macro_rules! session {
    ($name:ident) => {
        fn $name() -> &'static Session {
            static S: OnceLock<Session> = OnceLock::new();
            S.get_or_init(scenarios::$name)
        }
    };
}

session!(keeper_liquidation);
session!(mock_lifecycle);
session!(mock_admin);
session!(keeper_coverage_pause);
session!(raydium_lifecycle);
session!(pool_switch);
session!(pump_synthetic_exit);
session!(pump_graduation);
session!(pump_graduation_raced);

/// The scenarios this binary records, by name.
fn all() -> Vec<(&'static str, &'static Session)> {
    vec![
        ("keeper_liquidation", keeper_liquidation()),
        ("mock_lifecycle", mock_lifecycle()),
        ("mock_admin", mock_admin()),
        ("keeper_coverage_pause", keeper_coverage_pause()),
        ("raydium_lifecycle", raydium_lifecycle()),
        ("pool_switch", pool_switch()),
        ("pump_synthetic_exit", pump_synthetic_exit()),
        ("pump_graduation", pump_graduation()),
        ("pump_graduation_raced", pump_graduation_raced()),
    ]
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
}

fn fails(what: &str, v: &[String]) {
    assert!(
        v.is_empty(),
        "{what}: {} mismatches\n{}",
        v.len(),
        v.iter().take(40).cloned().collect::<Vec<_>>().join("\n")
    );
}

// ---- the sessions ----------------------------------------------------------------------------

#[test]
fn sessions_round_trip_through_their_files() {
    for (name, s) in all() {
        let dir = std::env::var_os(recorder::RECORD_SESSIONS_ENV)
            .map_or_else(|| tmp().join("sessions"), PathBuf::from);
        let path = recorder::write_to_dir(&dir, name, s).expect("write");
        let back = session::read(&path).expect("read");
        assert!(back == *s, "{name}: the session changed through its file");
        assert!(
            !s.txs.is_empty() && !s.checkpoints.is_empty(),
            "{name}: empty"
        );
        // Every transaction decodes from its getTransaction JSON, with its signature, slot and
        // block position; positions are strictly increasing in chain order.
        let mut last = None;
        for t in &s.txs {
            let tx = t
                .transaction
                .transaction
                .transaction
                .decode()
                .unwrap_or_else(|| panic!("{name}: undecodable transaction at slot {}", t.slot));
            assert!(!tx.signatures.is_empty());
            assert_eq!(t.transaction.slot, t.slot);
            assert_eq!(t.transaction.transaction_index, Some(t.tx_index));
            let key = (t.slot, t.tx_index);
            assert!(
                last.is_none_or(|l| l < key),
                "{name}: chain order at {key:?}"
            );
            last = Some(key);
        }
        eprintln!(
            "{name}: {} transactions, {} checkpoints, {} bytes",
            s.txs.len(),
            s.checkpoints.len(),
            std::fs::metadata(&path).map_or(0, |m| m.len())
        );
    }
}

/// The committed session set (INDEXER.md §11.3): `SHORTWIN_RECORD_SESSIONS=<dir>` writes its
/// thinned copies into `<dir>/committed/` (copy them to `services/indexer/tests/data/sessions/`
/// to regenerate the set); the committed files must still read, decode and match their own
/// checkpoints with the current program types (a layout change fails here first).
#[test]
fn the_committed_sessions_are_readable_and_consistent() {
    if let Some(dir) = std::env::var_os(recorder::RECORD_SESSIONS_ENV) {
        let dir = PathBuf::from(dir);
        for (name, s) in all() {
            if recorder::COMMITTED.contains(&name) {
                recorder::write_to_dir(&dir.join("committed"), name, &recorder::thinned(s))
                    .expect("write");
            }
            if recorder::INGEST_FIXTURES.contains(&name) {
                recorder::write_to_dir(&dir.join("ingest"), name, &recorder::thinned(s))
                    .expect("write");
            }
        }
    }
    // INGEST's recorded fixtures (`<dir>/ingest/*.jsonl`, copied to
    // `services/indexer/tests/data/ingest/`): readable, decoded and consistent with their own
    // checkpoints on the current program types.
    for name in recorder::INGEST_FIXTURES {
        let path = recorder::ingest_fixtures_dir().join(format!("{name}.jsonl"));
        let s = session::read(&path).unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
        assert_eq!(s.header.recorder, recorder::RECORDER);
        assert_eq!(s.header.scenario, name);
        assert!(s.checkpoints.len() >= 2 && !s.txs.is_empty());
        let (_, problems) = chain::check_session(&s).expect("check");
        fails(name, &problems);
    }
    for name in recorder::COMMITTED {
        let path = recorder::committed_dir().join(format!("{name}.jsonl"));
        let s = session::read(&path).unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
        assert_eq!(s.header.recorder, recorder::RECORDER);
        assert!(s.checkpoints.len() >= 2 && !s.txs.is_empty());
        let (model, problems) = chain::check_session(&s).expect("check");
        fails(name, &problems);
        assert!(model.kinds.len() > 10, "{name}: {:?}", model.kinds);
    }
}

#[test]
fn the_contract_rules_equal_the_chain_at_every_checkpoint() {
    for (name, s) in all() {
        let (model, problems) = chain::check_session(s).expect("check");
        fails(name, &problems);
        eprintln!("{name}: {:?}", model.kinds);
    }
}

#[test]
fn the_mock_lifecycle_covers_its_flows() {
    let (m, _) = chain::check_session(mock_lifecycle()).expect("check");
    for kind in [
        "MarketCreated",
        "Deposit",
        "Withdraw",
        "ClaimSol",
        "DeficitCovered",
        "LenderSynced",
        "Open",
        "Close",
        "Liquidate",
        "Expire",
        "SolSettle",
        "SettleArmed",
        "EpochRolled",
        "ProtocolFeesSwept",
        "MarginAdded",
        "PriceCranked",
        "RouteChainChanged",
        "TraderSwept",
        "MintSafetyTripped",
        "VenueUpgradeFlagged",
        "VenueRevalidated",
        "CoverageGap",
        "PauseSet",
        "VenueDisabled",
        "RouteBrokenSet",
    ] {
        assert!(m.kinds.contains_key(kind), "mock_lifecycle: no {kind}");
    }
    let conditions: Vec<u8> = m.settlements.iter().map(|s| s.condition).collect();
    for c in [1, 2, 3] {
        assert!(
            conditions.contains(&c),
            "mock_lifecycle: no S{c} ({conditions:?})"
        );
    }
    // Both epoch rolls: a full wipe and a rebase; the wipe's record donation was swept.
    // KI-67: a wiped lender's claim closed its position; the end state lists it closed.
    let end = mock_lifecycle().end_state().expect("end state");
    assert!(
        m.lenders.iter().any(|((market, owner), l)| {
            l.claims > 0
                && l.shares == 0
                && end.closed.contains(
                    &shortwin_litesvm_tests::pda::lender(market, owner)
                        .0
                        .to_string(),
                )
        }),
        "mock_lifecycle: no LenderPosition closed by claim_sol"
    );
    let nums: Vec<u8> = m.share_epochs.values().map(|(n, _)| *n).collect();
    assert!(nums.contains(&0) && nums.contains(&1), "{nums:?}");
    // The record donations: M4's full wipe and M5's flushing rebase.
    assert_eq!(
        m.donations.values().sum::<u64>(),
        7_000_000 + scenarios::M5_DONATION
    );
    assert!(
        m.donations.iter().any(|(market, d)| {
            *d == scenarios::M5_DONATION
                && m.share_epochs
                    .iter()
                    .any(|((mk, _), (num, _))| mk == market && *num > 0)
                && m.settlements
                    .iter()
                    .any(|s| s.market == *market && s.vest_flushed > 0)
        }),
        "mock_lifecycle: no flushing rebase with a record donation"
    );
    // AIR-07: a settlement flushed the vest on a market that never rolled its epoch (M6), and
    // the contract's `vest_bal` followed it (checked against the chain at every checkpoint).
    assert!(
        m.settlements
            .iter()
            .any(|s| s.vest_flushed > 0
                && !m.share_epochs.keys().any(|(market, _)| *market == s.market)),
        "mock_lifecycle: no vest flush without a roll"
    );
    // The flushed vests are still there at the end (M6; M7, where the last shares left a running
    // vest and were paid it, R3B-2), so the DB-side assertions check a flushed
    // `markets.vest_bal` on the end state too (`assert::projections_vs_chain`).
    let end_state = ChainState::from_checkpoint(end).expect("end state");
    let flushed: Vec<_> = end_state
        .markets
        .values()
        .filter(|mk| mk.vest_bal == 0 && mk.vest_end > mk.vest_last)
        .collect();
    assert!(
        flushed
            .iter()
            .any(|mk| mk.share_epoch == 0 && mk.total_shares.get() > 0),
        "mock_lifecycle: no flush without a roll left at the end"
    );
    assert!(
        flushed
            .iter()
            .any(|mk| mk.total_shares.get() == 0
                && !mk.has_flag(shortwin::state::MARKET_FLAG_WIPE_LIVE)),
        "mock_lifecycle: the last shares did not leave a running vest"
    );
    // An S1 gap settlement realised a loss for the lenders (SPEC §13).
    assert!(
        m.settlements
            .iter()
            .any(|s| s.condition == 1 && s.realised_lender_loss > 0),
        "{:?}",
        m.settlements
            .iter()
            .map(|s| (s.condition, s.p_ref_source, s.lender_pnl))
            .collect::<Vec<_>>()
    );
}

// ---- the DB-side assertions on the reference writer ---------------------------------------------

/// A database holding `s` through the reference writer, with the model and the end state.
async fn seeded(pg: &TestPg, s: &Session) -> (sqlx::PgPool, Model, ChainState) {
    let txs = chain::decode_session(s).expect("decode");
    let mut model = Model::new(s).expect("model").collecting();
    model.apply_all(&txs);
    let end = ChainState::from_checkpoint(s.end_state().expect("end state")).expect("state");
    let pool = pg.fresh_db().await.expect("db");
    seed::write(&pool, &txs, &model, Some(&end))
        .await
        .expect("seed");
    (pool, model, end)
}

#[test]
fn the_assertions_hold_on_a_faithful_database_and_catch_tampering() {
    // Recorded first: the keeper scenarios run their own runtime.
    let sessions = all();
    runtime().block_on(async {
        let pg = TestPg::start_in(&tmp()).await.expect("postgres");
        for (name, s) in sessions {
            let (pool, model, end) = seeded(&pg, s).await;
            fails(
                name,
                &check::projections_vs_chain(&pool, &end).await.expect("1-5"),
            );
            fails(name, &check::ledgers(&pool, &model).await.expect("6"));
            fails(
                name,
                &check::analytics(&pool, &end, &model).await.expect("7"),
            );
            fails(
                name,
                &check::rollups(&pool, &model).await.expect("7 rollups"),
            );
            fails(name, &check::quarantined(&pool).await.expect("12"));
            fails(name, &check::read_model(&pool).await.expect("read model"));
            fails(
                name,
                &check::vest_vs_model(&pool, &model).await.expect("vest_bal"),
            );
            // A rebuild-free equality check of the dump with itself is trivial; a dump differs
            // once a row changes.
            let before = check::dump(&pool).await.expect("dump");

            // Each tampered row is caught by the assertion that owns it.
            let market: String = sqlx::query_scalar("SELECT market FROM markets LIMIT 1")
                .fetch_one(&pool)
                .await
                .expect("a market");
            sqlx::query("UPDATE markets SET tokens_free = tokens_free + 1 WHERE market = $1")
                .bind(&market)
                .execute(&pool)
                .await
                .expect("tamper");
            let d = check::projections_vs_chain(&pool, &end).await.expect("1-5");
            assert!(
                d.iter().any(|x| x.contains("tokens_free")),
                "{name}: a tampered T went unnoticed: {d:?}"
            );
            assert!(
                !check::diff_dumps("tamper", &before, &check::dump(&pool).await.expect("dump"))
                    .is_empty()
            );
            sqlx::query("UPDATE markets SET tokens_free = tokens_free - 1 WHERE market = $1")
                .bind(&market)
                .execute(&pool)
                .await
                .expect("untamper");
            // AIR-07: a stale `vest_bal` after a flush is caught against the chain and against the
            // model (mock_lifecycle ends with M6's flushed vest).
            let flushed = end
                .markets
                .iter()
                .find(|(_, m)| m.vest_bal == 0 && m.vest_end > m.vest_last)
                .map(|(k, _)| k.to_string());
            assert!(
                flushed.is_some() || name != "mock_lifecycle",
                "{name}: no flushed vest at the end"
            );
            if let Some(mk) = flushed {
                sqlx::query("UPDATE markets SET vest_bal = 1 WHERE market = $1")
                    .bind(&mk)
                    .execute(&pool)
                    .await
                    .expect("tamper vest_bal");
                let d = check::projections_vs_chain(&pool, &end).await.expect("1");
                assert!(
                    d.iter().any(|x| x.contains("vest_bal (flushed)")),
                    "{name}: a stale vest_bal went unnoticed: {d:?}"
                );
                let d = check::vest_vs_model(&pool, &model).await.expect("vest_bal");
                assert!(
                    d.iter().any(|x| x.contains(&mk)),
                    "{name}: a stale vest_bal went unnoticed by the model check: {d:?}"
                );
                sqlx::query("UPDATE markets SET vest_bal = 0 WHERE market = $1")
                    .bind(&mk)
                    .execute(&pool)
                    .await
                    .expect("untamper vest_bal");
            }
            sqlx::query(
                "UPDATE fees SET amount = amount + 1 WHERE ctid = \
                 (SELECT ctid FROM fees WHERE recipient = 'protocol' LIMIT 1)",
            )
            .execute(&pool)
            .await
            .expect("tamper fee");
            let d = check::ledgers(&pool, &model).await.expect("6");
            assert!(
                d.iter().any(|x| x.contains("protocol_fees vs"))
                    && d.iter().any(|x| x.starts_with("fees ")),
                "{name}: a tampered fee went unnoticed: {d:?}"
            );
            if !model.settlements.is_empty() {
                sqlx::query(
                    "UPDATE settlements SET realised_lender_loss = realised_lender_loss + 1 \
                     WHERE ctid = (SELECT ctid FROM settlements LIMIT 1)",
                )
                .execute(&pool)
                .await
                .expect("tamper settlement");
                let d = check::analytics(&pool, &end, &model).await.expect("7");
                assert!(
                    d.iter().any(|x| x.contains("realised_lender_loss")),
                    "{name}: a tampered loss went unnoticed: {d:?}"
                );
            }
            let open: Option<String> =
                sqlx::query_scalar("SELECT position FROM positions WHERE status = 'open' LIMIT 1")
                    .fetch_optional(&pool)
                    .await
                    .expect("positions");
            if let Some(p) = open {
                sqlx::query("UPDATE positions SET status = 'closed' WHERE position = $1")
                    .bind(&p)
                    .execute(&pool)
                    .await
                    .expect("tamper position");
                let d = check::projections_vs_chain(&pool, &end).await.expect("3");
                assert!(d.iter().any(|x| x.contains(&p)), "{name}: {d:?}");
            }
            pool.close().await;
        }
        pg.stop().await.expect("stop");
    });
}

#[test]
fn the_analytics_reproduce_the_sessions_numbers() {
    let s = mock_lifecycle();
    runtime().block_on(async {
        let pg = TestPg::start_in(&tmp()).await.expect("postgres");
        let (pool, model, _) = seeded(&pg, s).await;
        // SPEC §13 by condition: S1 / S2 / S3 counts and the realised lender loss.
        let (s1, s2, s3, loss): (i64, i64, i64, String) = sqlx::query_as(
            "SELECT settlements_s1, settlements_s2, settlements_s3, realised_lender_loss::text \
             FROM v_global_stats",
        )
        .fetch_one(&pool)
        .await
        .expect("v_global_stats");
        let count = |c: u8| {
            i64::try_from(
                model
                    .settlements
                    .iter()
                    .filter(|x| x.condition == c)
                    .count(),
            )
            .expect("n")
        };
        assert_eq!((s1, s2, s3), (count(1), count(2), count(3)));
        assert_eq!(
            loss,
            model
                .settlements
                .iter()
                .map(|x| x.realised_lender_loss)
                .sum::<u128>()
                .to_string()
        );
        assert!(s1 >= 1 && s2 >= 1 && s3 >= 1);
        // Keeper coverage: the gaps the program logged, the observations sent.
        let (gaps, gap_s): (i64, String) = sqlx::query_as(
            "SELECT sum(gaps)::bigint, sum(gap_s_total)::text FROM v_market_coverage",
        )
        .fetch_one(&pool)
        .await
        .expect("v_market_coverage");
        assert_eq!(gaps, i64::try_from(model.coverage_gaps.len()).expect("n"));
        assert_eq!(
            gap_s,
            model
                .coverage_gaps
                .iter()
                .map(|(_, g)| *g)
                .sum::<i64>()
                .to_string()
        );
        // The rollups over the whole session reproduce the views and the totals.
        fails(
            "rollups",
            &check::rollups(&pool, &model).await.expect("rollups"),
        );
        let hours: i64 = sqlx::query_scalar("SELECT count(*) FROM market_stats_hourly")
            .fetch_one(&pool)
            .await
            .expect("hours");
        assert!(hours > 24, "{hours} market-hours");
        // Positions were open for part of the session, and the observations the keeper's
        // instructions recorded are counted.
        let (open_s, obs): (i64, i64) = sqlx::query_as(
            "SELECT sum(positions_open_s)::bigint, sum(observations)::bigint \
             FROM market_stats_hourly",
        )
        .fetch_one(&pool)
        .await
        .expect("flows");
        assert!(open_s > 0 && obs > 0, "{open_s} {obs}");
        let invocations: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM invocations WHERE NOT failed AND sw_observing(name) \
             AND market IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .expect("invocations");
        assert_eq!(obs, invocations);
        // Failed keeper attempts (§8.5): every failed transaction naming a keeper instruction,
        // by wallet, instruction and program error.
        let txs = chain::decode_session(s).expect("decode");
        let keeper_ixs = [
            "crank_price",
            "liquidate",
            "expire",
            "sol_settle",
            "sweep_trader",
            "propose_pool",
            "execute_pool_switch",
            "set_coverage_pause",
        ];
        let want = txs
            .iter()
            .filter(|t| !t.ok)
            .map(|t| {
                t.invocations
                    .iter()
                    .map(|i| i.name.as_str())
                    .filter(|n| keeper_ixs.contains(n))
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
            })
            .sum::<usize>();
        let failures: Option<i64> =
            sqlx::query_scalar("SELECT sum(failures)::bigint FROM v_keeper_failures")
                .fetch_one(&pool)
                .await
                .expect("v_keeper_failures");
        assert_eq!(failures.unwrap_or(0), i64::try_from(want).expect("n"));
        assert!(
            want > 0,
            "the lifecycle's refused exits are failed keeper attempts"
        );
        pool.close().await;
        pg.stop().await.expect("stop");
    });
}

// ---- §11.4 through the INGEST pipeline -------------------------------------------------------------

#[test]
fn every_session_replays_into_the_chain_state() {
    // Recorded first: the keeper scenarios run their own runtime.
    let sessions = all();
    runtime().block_on(async {
        let pg = TestPg::start_in(&tmp()).await.expect("postgres");
        for (name, s) in sessions {
            match check::replay_session(&pg, s, check::ReplayOptions::default()).await {
                Ok(r) => {
                    eprintln!(
                        "{name}: {} checkpoints compared, {} transactions ingested",
                        r.checkpoints, r.ingest.txs_written
                    );
                    fails(name, &r.failures);
                }
                Err(e) => panic!("{name}: {e:?}"),
            }
        }
        pg.stop().await.expect("stop");
    });
}

// ---- the catalog --------------------------------------------------------------------------------

/// Kinds no session reaches, with the reason.
const NEVER_EMITTED: [(&str, &str); 1] = [(
    "VenueRewardsClaimed",
    "claim_venue_rewards is not built: the program never emits it",
)];

#[test]
fn the_sessions_cover_the_event_catalog() {
    let models: Vec<Model> = all()
        .into_iter()
        .map(|(_, s)| chain::check_session(s).expect("check").0)
        .collect();
    let refs: Vec<&Model> = models.iter().collect();
    let gaps = check::catalog_gaps(&refs, &NEVER_EMITTED);
    fails("catalog", &gaps);
}
