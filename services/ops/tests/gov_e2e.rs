//! T-OPS (M10 WP-7): `shortwin-gov` end to end on **the mainnet Squads v4 ELF** and short.win's
//! release build (LiteSVM; never a real cluster). The commands run exactly as the binary runs
//! them (`shortwin_ops::run` with the command line, the layout file and keypair files), against
//! [`EnvChain`]: a `Chain` over the program harness's `Env` whose `send` checks the tool's
//! transaction (every signature valid, ≤ 1,232 bytes) and replays its instructions through
//! **`Env::send`** — the invariant hooks after every instruction, I4's vault path included — with
//! the tool's fee payer and signers.
//!
//! | Test | What |
//! |---|---|
//! | T-OPS-1 | the example layout is refused for anything that sends (`status` and `verify` still run); `--dry-run` simulates and sends nothing; `verify` on an empty cluster fails on every missing piece |
//! | T-OPS-2 | the whole sequence: both multisigs; `init-config` with the vault PDAs (a stale guard slot refused first); `stage-upgrade` refused while the deployer still holds the upgrade authority; the checked upgrade-authority transfer of short.win through Governance (its whole upgrade set since the straddle helper `complete_curve` was retired, pump-final D2; the deployer co-signs; `--checked` required; Squads refuses 1 s early); an `Authorities` change through both timelocks (Squads 172,800 s, then short.win's `TimelockNotElapsed` 1 s early); an upgrade through the vault (a buffer the vault does not own, and one whose ELF outgrows the ProgramData, refused by the tool — the first, staged anyway, by the loader after the time lock); a cancelled stale upgrade; `status` decoding a staged transaction; Emergency `set-pause` in two transactions (a rerun refused as a duplicate); the worst-case `set_venue_extras` through the transaction buffer, and three of them in one message through a buffer create and extends; a buffer an interrupted run left resumed with `--force-buffer`, a foreign one refused and closed with `close-buffer`; `verify` passes, and fails on a layout that drifted |

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anchor_lang::prelude::Pubkey;
use sha2::{Digest, Sha256};
use shortwin::errors::ShortwinError;
use shortwin::state::{VenueExtras, VenueKind};
use shortwin_litesvm_tests::squads::{MultisigError, loader as harness_loader};
use shortwin_litesvm_tests::{Env, read};
use shortwin_ops::ixs::loader;
use shortwin_ops::squads::StageOpts;
use shortwin_ops::verify::FEE_VAULT_PREFUND;
use shortwin_ops::{Layout, TxFailure, failure};
use shortwin_services_common::tx;
use shortwin_services_common::{AccountData, Chain, ChainClock, Memcmp, TxError, TxOutcome};
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use squads_v4_ix::state::{Multisig, Proposal, ProposalStatus};

const EXAMPLE: &str = include_str!("../governance.example.toml");
const GOV_TL: i64 = 172_800;

// ---- the chain -----------------------------------------------------------------------------

/// A `Chain` over the harness's `Env` (module docs). `keys` holds every keypair the test wrote
/// to a file, so `send` can hand the tool's signers to `Env::send`.
struct EnvChain {
    env: Rc<RefCell<Env>>,
    keys: RefCell<HashMap<Pubkey, Keypair>>,
    /// Every transaction sent: its wire size as the tool built it.
    sizes: RefCell<Vec<usize>>,
}

fn outcome_of(meta: &litesvm::types::TransactionMetadata, err: Option<TxError>) -> TxOutcome {
    TxOutcome {
        signature: meta.signature,
        err,
        logs: meta.logs.clone(),
        units_consumed: meta.compute_units_consumed,
        fee: meta.fee,
        inner: Vec::new(),
        slot: 0,
    }
}

impl Chain for EnvChain {
    async fn clock(&self) -> anyhow::Result<ChainClock> {
        let c = self.env.borrow().clock();
        Ok(ChainClock {
            slot: c.slot,
            unix_timestamp: c.unix_timestamp,
        })
    }

    async fn accounts(&self, keys: &[Pubkey]) -> anyhow::Result<Vec<Option<AccountData>>> {
        let env = self.env.borrow();
        Ok(keys
            .iter()
            .map(|k| {
                env.svm
                    .get_account(k)
                    .filter(|a| a.lamports > 0)
                    .map(|a| AccountData {
                        lamports: a.lamports,
                        owner: a.owner,
                        executable: a.executable,
                        data: a.data,
                    })
            })
            .collect())
    }

    async fn program_accounts(
        &self,
        _program: &Pubkey,
        _filters: &[Memcmp],
    ) -> anyhow::Result<Vec<(Pubkey, AccountData)>> {
        anyhow::bail!("not used by shortwin-gov")
    }

    async fn latest_blockhash(&self) -> anyhow::Result<solana_hash::Hash> {
        Ok(self.env.borrow().svm.latest_blockhash())
    }

    async fn simulate(&self, t: &VersionedTransaction) -> anyhow::Result<TxOutcome> {
        let env = self.env.borrow();
        Ok(match env.svm.simulate_transaction(t.clone()) {
            Ok(info) => outcome_of(&info.meta, None),
            Err(f) => outcome_of(&f.meta, Some(TxError::from_transaction_error(f.err))),
        })
    }

    async fn send(&self, t: &VersionedTransaction) -> anyhow::Result<TxOutcome> {
        // The tool's own transaction: every signature valid, within the packet limit.
        let msg = t.message.serialize();
        let statics = t.message.static_account_keys();
        for (sig, key) in t.signatures.iter().zip(statics) {
            assert!(sig.verify(key.as_ref(), &msg), "bad signature of {key}");
        }
        let size = tx::size(t);
        assert!(size <= 1_232, "a {size}-byte transaction");
        self.sizes.borrow_mut().push(size);
        // Replayed through Env::send (the hooks) with the same payer and signers.
        let ixs = tx::decompile(t, &[])?;
        let n = usize::from(t.message.header().num_required_signatures);
        let keys = self.keys.borrow();
        let kp = |k: &Pubkey| {
            keys.get(k)
                .unwrap_or_else(|| panic!("the tool signed with {k}, a key the test never wrote"))
                .insecure_clone()
        };
        let payer = kp(&statics[0]);
        let signers: Vec<Keypair> = statics[1..n].iter().map(kp).collect();
        let refs: Vec<&Keypair> = signers.iter().collect();
        let mut env = self.env.borrow_mut();
        let saved = std::mem::replace(&mut env.payer, payer);
        let res = env.send(&ixs, &refs);
        env.payer = saved;
        Ok(match res {
            Ok(meta) => outcome_of(&meta, None),
            Err(f) => outcome_of(&f.meta, Some(TxError::from_transaction_error(f.err))),
        })
    }

    async fn recent_priority_fees(&self, _writable: &[Pubkey]) -> anyhow::Result<Vec<u64>> {
        Ok(Vec::new())
    }
}

// ---- the harness ---------------------------------------------------------------------------

/// The test's keys (as files), layout file and runtime.
struct H {
    chain: EnvChain,
    dir: PathBuf,
    config: PathBuf,
    files: HashMap<String, PathBuf>,
    rt: tokio::runtime::Runtime,
}

const MEMBER_FUNDS: u64 = 10_000_000_000;

impl H {
    /// `Env::with_squads` (short.win deployed upgradeable with the deployer as its upgrade
    /// authority), the keys of the owner's layout written to files, and the layout file: the
    /// example with the placeholders replaced and `example = true` dropped.
    fn new(name: &str) -> Self {
        let env = Env::with_squads();
        let deployer = env.upgrade_authority.insecure_clone();
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("gov_e2e")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut h = Self {
            chain: EnvChain {
                env: Rc::new(RefCell::new(env)),
                keys: RefCell::new(HashMap::new()),
                sizes: RefCell::new(Vec::new()),
            },
            config: dir.join("governance.toml"),
            dir,
            files: HashMap::new(),
            rt: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        };
        h.key("deployer", deployer);
        let mut text = EXAMPLE.replace("example = true\n", "");
        for (prefix, n) in [("GovMember", 5), ("EmgMember", 4)] {
            for i in 1..=n {
                let k = h.new_key(&format!("{}{}", prefix[..3].to_lowercase(), i - 1));
                text = text.replace(&placeholder(&format!("{prefix}{i}")), &k.to_string());
            }
        }
        for (p, name) in [
            ("GovCreateKey", "gov_create"),
            ("EmgCreateKey", "emg_create"),
            ("CoverageKeeperHotKey", "coverage"),
        ] {
            let k = h.new_key(name);
            text = text.replace(&placeholder(p), &k.to_string());
        }
        h.new_key("payer");
        h.new_key("coverage2");
        assert!(
            !text.contains("1111111111111111111111\""),
            "a placeholder left"
        );
        std::fs::write(&h.config, text).unwrap();
        h
    }

    fn env(&self) -> RefMut<'_, Env> {
        self.chain.env.borrow_mut()
    }

    /// Writes `kp` to `<name>.json` and registers it.
    fn key(&mut self, name: &str, kp: Keypair) -> Pubkey {
        let path = self.dir.join(format!("{name}.json"));
        solana_keypair::write_keypair_file(&kp, &path).unwrap();
        let k = kp.pubkey();
        self.chain.keys.borrow_mut().insert(k, kp);
        self.files.insert(name.to_string(), path);
        k
    }

    /// A new funded key `name`.
    fn new_key(&mut self, name: &str) -> Pubkey {
        let k = self.key(name, Keypair::new());
        self.env().fund(&k, MEMBER_FUNDS);
        k
    }

    fn keypair(&self, name: &str) -> Keypair {
        solana_keypair::read_keypair_file(&self.files[name]).unwrap()
    }

    fn pubkey(&self, name: &str) -> Pubkey {
        let path = &self.files[name];
        solana_keypair::read_keypair_file(path).unwrap().pubkey()
    }

    fn layout(&self) -> Layout {
        Layout::load(&self.config).unwrap()
    }

    /// Runs `cmd` (`@name` = that key's file) with `--config` appended; returns the result and
    /// the report.
    fn run(&self, cmd: &str) -> (anyhow::Result<bool>, String) {
        let mut argv: Vec<String> = cmd
            .split_whitespace()
            .map(|w| match w.strip_prefix('@') {
                Some(name) => self.files[name].display().to_string(),
                None => w.to_string(),
            })
            .collect();
        argv.push("--config".into());
        argv.push(self.config.display().to_string());
        let mut out = Vec::new();
        let res = self
            .rt
            .block_on(shortwin_ops::run(&self.chain, &argv, &mut out));
        let text = String::from_utf8(out).unwrap();
        println!("$ shortwin-gov {cmd}\n{text}");
        (res, text)
    }

    /// `cmd` succeeds; returns its report.
    #[track_caller]
    fn ok(&self, cmd: &str) -> String {
        let (res, text) = self.run(cmd);
        match res {
            Ok(true) => text,
            Ok(false) => panic!("{cmd}: checks failed\n{text}"),
            Err(e) => panic!("{cmd}: {e:#}\n{text}"),
        }
    }

    /// `cmd` fails; returns the error.
    #[track_caller]
    fn err(&self, cmd: &str) -> anyhow::Error {
        match self.run(cmd).0 {
            Err(e) => e,
            Ok(ok) => panic!("{cmd}: expected an error, got Ok({ok})"),
        }
    }

    fn warp(&self, s: i64) {
        self.env().warp(s);
    }

    fn account(&self, k: &Pubkey) -> Option<solana_account::Account> {
        self.env().svm.get_account(k).filter(|a| a.lamports > 0)
    }

    fn multisig(&self, k: &Pubkey) -> Multisig {
        Multisig::decode(&self.account(k).expect("multisig").data).unwrap()
    }

    fn status(&self, ms: &Pubkey, index: u64) -> ProposalStatus {
        let p = squads_v4_ix::pda::proposal(ms, index);
        Proposal::decode(&self.account(&p).expect("proposal").data)
            .unwrap()
            .status
    }

    fn upgrade_authority(&self, program: &Pubkey) -> Option<Pubkey> {
        let acc = self.account(&loader::program_data(program)).unwrap();
        shortwin_ops::verify::upgrade_authority(&AccountData {
            lamports: acc.lamports,
            owner: acc.owner,
            executable: acc.executable,
            data: acc.data,
        })
        .unwrap()
    }

    fn verify(&self) -> (bool, String) {
        let (res, text) = self.run("verify");
        (res.unwrap(), text)
    }
}

/// The base58 placeholder of `governance.example.toml` that starts with `prefix`.
fn placeholder(prefix: &str) -> String {
    EXAMPLE
        .split('"')
        .find(|s| s.starts_with(prefix))
        .unwrap_or_else(|| panic!("no placeholder {prefix}"))
        .to_string()
}

/// The Squads message of `ixs` for `vault` (as `stage` compiles it).
fn sq_compile(
    vault: &Pubkey,
    ixs: &[anchor_lang::solana_program::instruction::Instruction],
) -> Vec<u8> {
    squads_v4_ix::compile(vault, ixs, &[]).unwrap().bytes
}

#[track_caller]
fn tx_failure(e: &anyhow::Error) -> &TxFailure {
    failure(e).unwrap_or_else(|| panic!("not a transaction failure: {e:#}"))
}

/// The proposal index a `stage*` report names.
fn staged_index(report: &str) -> u64 {
    let at = report.find("staged ").expect("staged");
    let rest = &report[at..];
    let hash = rest.find('#').unwrap();
    rest[hash + 1..]
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

// ---- T-OPS-1 -------------------------------------------------------------------------------

/// T-OPS-1: the example's placeholder keys are refused for anything that sends (verify and
/// status still run); `--dry-run` simulates the first multisig creation and changes nothing;
/// `verify` on a cluster where nothing was set up fails on every missing piece.
#[test]
fn t_ops_1_example_refused_dry_run_sends_nothing() {
    let h = H::new("t_ops_1");
    let example = h.dir.join("example.toml");
    std::fs::write(&example, EXAMPLE).unwrap();
    let mut argv: Vec<String> = ["create-multisigs", "--payer", "x", "--config"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.push(example.display().to_string());
    let mut out = Vec::new();
    let e =
        h.rt.block_on(shortwin_ops::run(&h.chain, &argv, &mut out))
            .unwrap_err();
    assert!(format!("{e:#}").contains("example layout"), "{e:#}");
    assert!(h.chain.sizes.borrow().is_empty());
    // status and verify send nothing: they run on the example (its addresses are derived).
    for cmd in ["status", "verify"] {
        let argv: Vec<String> = [cmd, "--config", &example.display().to_string()]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut out = Vec::new();
        let res = h.rt.block_on(shortwin_ops::run(&h.chain, &argv, &mut out));
        let text = String::from_utf8(out).unwrap();
        assert_eq!(res.unwrap(), cmd == "status", "{cmd}\n{text}");
        if cmd == "status" {
            let ex = Layout::parse(EXAMPLE).unwrap();
            for line in [
                format!("Governance: multisig {} (3-of-5", ex.gov.key),
                format!("Governance multisig {} does not exist", ex.gov.key),
                format!("Emergency multisig {} does not exist", ex.emg.key),
                "GlobalConfig missing (init-config)".to_string(),
            ] {
                assert!(text.contains(&line), "{line}\n{text}");
            }
        }
    }

    let l = h.layout();
    let report = h.ok(
        "create-multisigs --payer @payer --gov-create-key @gov_create --emg-create-key @emg_create --dry-run",
    );
    assert!(report.contains("dry-run: create Governance") && report.contains("NOT sent"));
    assert!(report.contains("stopping after the first transaction"));
    assert!(h.account(&l.gov.key).is_none() && h.account(&l.emg.key).is_none());
    assert!(
        h.chain.sizes.borrow().is_empty(),
        "nothing reached Env::send"
    );

    let (passed, text) = h.verify();
    assert!(!passed);
    assert!(
        text.contains("ok   Squads v4 immutable: no upgrade authority"),
        "{text}"
    );
    for missing in [
        format!("FAIL Governance multisig {}: missing", l.gov.key),
        format!("FAIL Emergency multisig {}: missing", l.emg.key),
        "FAIL GlobalConfig".to_string(),
        "FAIL fee_recipient (Governance vault 1)".to_string(),
    ] {
        assert!(text.contains(&missing), "{missing}\n{text}");
    }
    // The upgrade authority is still the deployer's; short.win is the only program checked
    // (the straddle helper is retired, pump-final D2).
    assert!(text.contains("FAIL shortwin upgrade authority"), "{text}");
    assert!(!text.contains("complete_curve"), "{text}");
}

// ---- T-OPS-2 -------------------------------------------------------------------------------

/// T-OPS-2: the whole sequence (module docs), then `verify` passes.
#[test]
fn t_ops_2_full_sequence_through_both_timelocks() {
    let h = H::new("t_ops_2");
    let l = h.layout();
    let gov = l.gov.key;
    let emg = l.emg.key;
    let vault = l.governance();

    // 1. Both multisigs and the fee vault.
    h.ok(
        "create-multisigs --payer @payer --gov-create-key @gov_create --emg-create-key @emg_create",
    );
    let m = h.multisig(&gov);
    assert_eq!(
        (
            m.threshold,
            m.time_lock,
            m.members.len(),
            m.config_authority
        ),
        (3, 172_800, 5, Pubkey::default())
    );
    assert_eq!(m.rent_collector, Some(vault));
    let m = h.multisig(&emg);
    assert_eq!((m.threshold, m.time_lock, m.members.len()), (2, 0, 4));
    assert_eq!(
        h.account(&l.fee_recipient()).unwrap().lamports,
        FEE_VAULT_PREFUND
    );
    // Idempotent: a second run creates nothing and funds nothing.
    let n = h.chain.sizes.borrow().len();
    let again = h.ok(
        "create-multisigs --payer @payer --gov-create-key @gov_create --emg-create-key @emg_create",
    );
    assert!(again.contains("exists: skipped") && again.contains("funded"));
    assert_eq!(h.chain.sizes.borrow().len(), n);

    // 2. init-config: a stale guard slot (a venue upgraded since the layout was written) is
    //    refused; with the live slot it initializes with the vault PDAs.
    let amm_v4_pd = shortwin::constants::VENUE_PROGRAM_DATA[3];
    let configured = l.guard_slots.unwrap()[3];
    let plant = |h: &H, slot: u64| {
        let mut data = shortwin_venue_state::program_data::encode_header(slot, None).to_vec();
        data.resize(64, 0);
        h.env()
            .svm
            .set_account(
                amm_v4_pd,
                solana_account::Account {
                    lamports: 1_000_000,
                    data,
                    owner: loader::ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
    };
    plant(&h, configured + 1);
    let e = h.err("init-config --authority @deployer");
    assert!(
        format!("{e:#}").contains(&format!("deployed at {}", configured + 1)),
        "{e:#}"
    );
    // Not the upgrade authority: refused before anything is sent.
    let e = h.err("init-config --authority @payer");
    assert!(format!("{e:#}").contains("must be signed by short.win's upgrade authority"));
    plant(&h, configured);
    h.ok("init-config --authority @deployer");
    let c = h.env().config();
    assert_eq!(
        [
            c.governance,
            c.pause_authority,
            c.coverage_authority,
            c.fee_recipient
        ],
        l.authorities()
    );
    assert_eq!(c.timelock_s, l.timelock_s);
    let (passed, text) = h.verify();
    assert!(!passed);
    let fails: Vec<&str> = text.lines().filter(|l| l.starts_with("FAIL")).collect();
    assert_eq!(
        fails.len(),
        1,
        "only short.win's upgrade authority:\n{text}"
    );
    // Before the upgrade authority moves (R4), stage-upgrade is refused even from a buffer the
    // vault owns: the loader would refuse it only at execution, after the 48 h time lock.
    let elf = std::fs::read(shortwin_litesvm_tests::env::program_so_path()).unwrap();
    let deployer = h.pubkey("deployer");
    let early = harness_loader::plant_buffer(&mut h.env(), &elf, &vault);
    let e = h.err(&format!("stage-upgrade --key @gov0 --buffer {early}"));
    assert!(
        format!("{e:#}").contains(&format!(
            "shortwin's upgrade authority is {deployer}, not Governance vault 0"
        )),
        "{e:#}"
    );
    assert_eq!(h.multisig(&gov).transaction_index, 0, "nothing staged");

    // 3. The checked upgrade-authority transfer of short.win, Governance's whole upgrade set.
    let e = h.err("stage-upgrade-authority --key @gov0");
    assert!(format!("{e:#}").contains("--checked"));
    let r = h.ok("stage-upgrade-authority --checked --key @gov0");
    let ua = staged_index(&r);
    assert_eq!(ua, 1);
    assert!(
        r.contains("loader SetAuthorityChecked")
            && r.contains(&format!("shortwin: {deployer} -> {vault} (checked)"))
            && !r.contains("complete_curve"),
        "{r}"
    );
    // A non-member cannot approve; members 1 and 2 reach the threshold.
    let e = h.err(&format!("approve --ms governance --index {ua} --key @emg0"));
    assert!(format!("{e:#}").contains("is not a member of Governance"));
    h.ok(&format!("approve --ms governance --index {ua} --key @gov1"));
    let r = h.ok(&format!("approve --ms governance --index {ua} --key @gov2"));
    assert!(r.contains("execution co-signed by") && r.contains(&h.pubkey("deployer").to_string()));
    assert!(matches!(h.status(&gov, ua), ProposalStatus::Approved(_)));
    // Without the deployer's signature the tool refuses; 1 s before the time lock Squads does.
    let e = h.err(&format!("execute --ms governance --index {ua} --key @gov2"));
    assert!(format!("{e:#}").contains("--co-signer"), "{e:#}");
    h.warp(GOV_TL - 1);
    let e = h.err(&format!(
        "execute --ms governance --index {ua} --key @gov2 --co-signer @deployer"
    ));
    let f = tx_failure(&e);
    assert_eq!(
        (f.program, f.code()),
        (
            Some(squads_v4_ix::ID),
            Some(MultisigError::TimeLockNotReleased.code())
        )
    );
    h.warp(1);
    h.ok(&format!(
        "execute --ms governance --index {ua} --key @gov2 --co-signer @deployer"
    ));
    assert_eq!(h.upgrade_authority(&shortwin::ID), Some(vault));
    let (passed, text) = h.verify();
    assert!(passed, "{text}");
    let e = h.err("stage-upgrade-authority --checked --key @gov0");
    assert!(format!("{e:#}").contains("nothing to transfer"));

    // 4. An Authorities change (coverage key rotation) through both timelocks.
    let e = h.err("stage-authorities --key @gov0");
    assert!(format!("{e:#}").contains("nothing to change"));
    let old_cov = h.pubkey("coverage");
    let new_cov = h.pubkey("coverage2");
    let text = std::fs::read_to_string(&h.config).unwrap();
    std::fs::write(
        &h.config,
        text.replace(&old_cov.to_string(), &new_cov.to_string()),
    )
    .unwrap();
    // verify reads the drift before anything is staged.
    let (passed, text) = h.verify();
    assert!(
        !passed
            && text.contains(&format!(
                "FAIL config.coverage_authority: {old_cov}, expected {new_cov}"
            )),
        "{text}"
    );
    let r = h.ok("stage-authorities --key @gov0");
    let au = staged_index(&r);
    assert!(r.contains("propose_config") && r.contains(&format!("coverage_authority: {new_cov}")));
    h.ok(&format!("approve --ms governance --index {au} --key @gov3"));
    h.ok(&format!("approve --ms governance --index {au} --key @gov4"));
    let e = h.err("apply --key @payer");
    assert!(format!("{e:#}").contains("no pending config change"));
    h.warp(GOV_TL);
    h.ok(&format!("execute --ms governance --index {au} --key @gov4"));
    let not_before = h.env().config().pending.not_before;
    assert_eq!(not_before, h.env().now() + i64::from(l.timelock_s));
    let (_, text) = h.verify();
    assert!(
        text.contains("WARN pending config change Authorities"),
        "{text}"
    );
    h.warp(i64::from(l.timelock_s) - 1);
    let e = h.err("apply --key @payer");
    let f = tx_failure(&e);
    assert_eq!(
        (f.program, f.code(), f.name.as_deref()),
        (
            Some(shortwin::ID),
            Some(u32::from(ShortwinError::TimelockNotElapsed)),
            Some("TimelockNotElapsed")
        )
    );
    h.warp(1);
    h.ok("apply --key @payer");
    assert_eq!(h.env().config().coverage_authority, new_cov);
    assert!(!h.env().config().pending.is_some());

    // 5. An upgrade through the Governance vault.
    // A buffer the vault does not own: the tool refuses before staging...
    let wrong = harness_loader::plant_buffer(&mut h.env(), &elf, &deployer);
    let e = h.err(&format!("stage-upgrade --key @gov0 --buffer {wrong}"));
    assert!(format!("{e:#}").contains("not Governance vault 0"), "{e:#}");
    // ...which saves the 48 h the loader would take to refuse it (staged without the check).
    {
        let member = solana_keypair::read_keypair_file(&h.files["gov0"]).unwrap();
        let ix = loader::upgrade(&shortwin::ID, &wrong, &member.pubkey(), &vault);
        let mut out = Vec::new();
        let ops = shortwin_ops::send::Ops::new(&h.chain, false, &mut out);
        let idx =
            h.rt.block_on(shortwin_ops::squads::stage(
                &ops,
                &l.gov,
                &member,
                0,
                &[ix],
                StageOpts::default(),
            ))
            .unwrap();
        drop(ops);
        h.ok(&format!(
            "approve --ms governance --index {idx} --key @gov1"
        ));
        h.ok(&format!(
            "approve --ms governance --index {idx} --key @gov2"
        ));
        h.warp(GOV_TL);
        let e = h.err(&format!(
            "execute --ms governance --index {idx} --key @gov2"
        ));
        assert_eq!(
            tx_failure(&e).err,
            TxError::Instruction {
                index: 0,
                detail: "IncorrectAuthority".into()
            }
        );
        // Still Approved: verify warns until Governance cancels it (3 votes).
        let (passed, text) = h.verify();
        assert!(
            passed && text.contains(&format!("WARN Governance #{idx} is Approved")),
            "{text}"
        );
        for m in ["gov0", "gov1"] {
            h.ok(&format!("cancel --ms governance --index {idx} --key @{m}"));
        }
        assert!(matches!(h.status(&gov, idx), ProposalStatus::Approved(_)));
        h.ok(&format!("cancel --ms governance --index {idx} --key @gov2"));
        assert!(matches!(h.status(&gov, idx), ProposalStatus::Cancelled(_)));
    }
    // An ELF one byte larger than the ProgramData's room: refused, with the extension to run.
    let room = h
        .account(&loader::program_data(&shortwin::ID))
        .unwrap()
        .data
        .len()
        - loader::PROGRAM_DATA_METADATA;
    let mut big = elf.clone();
    big.resize(room + 1, 0);
    let large = harness_loader::plant_buffer(&mut h.env(), &big, &vault);
    let e = h.err(&format!("stage-upgrade --key @gov0 --buffer {large}"));
    assert!(
        format!("{e:#}").contains(&format!(
            "the new ELF ({} B) does not fit shortwin's ProgramData ({room} B): extend it by 1 bytes",
            room + 1
        )),
        "{e:#}"
    );
    let good = harness_loader::plant_buffer(&mut h.env(), &elf, &vault);
    let spill = h.pubkey("gov0");
    let r = h.ok(&format!("stage-upgrade --key @gov0 --buffer {good}"));
    let up = staged_index(&r);
    assert!(r.contains("loader Upgrade") && r.contains(&format!("spill {spill}")));
    // status decodes what the members are asked to approve.
    let st = h.ok(&format!("status --ms governance --index {up}"));
    for line in [
        format!("Governance #{up}: Active("),
        format!("approved by 1 of 3 needed ({spill})"),
        format!("runs as vault 0 ({vault}), created by {spill}"),
        format!(
            "[0] loader Upgrade: program {} from buffer {good} (spill {spill}, authority {vault})",
            shortwin::ID
        ),
    ] {
        assert!(st.contains(&line), "{line}\n{st}");
    }
    h.ok(&format!("approve --ms governance --index {up} --key @gov1"));
    h.ok(&format!("approve --ms governance --index {up} --key @gov2"));
    h.warp(GOV_TL);
    let slot_before = shortwin_venue_state::bytes::u64_at(
        &h.account(&loader::program_data(&shortwin::ID))
            .unwrap()
            .data,
        4,
    );
    h.ok(&format!("execute --ms governance --index {up} --key @gov2"));
    let pd = h.account(&loader::program_data(&shortwin::ID)).unwrap();
    let slot = shortwin_venue_state::bytes::u64_at(&pd.data, 4);
    assert!(slot > slot_before, "redeployed at the current slot");
    assert_eq!(slot, h.env().clock().slot);
    assert_eq!(
        h.upgrade_authority(&shortwin::ID),
        Some(vault),
        "authority kept"
    );
    assert!(h.account(&good).is_none(), "buffer closed");
    h.warp(1);

    // 6. The Emergency: set-pause in two transactions (stage + approve --execute).
    let emg_vault = l.pause_authority();
    let before = h.chain.sizes.borrow().len();
    let r = h.ok("stage set-pause global on --key @emg0");
    let p = staged_index(&r);
    //    The second member reviews it with status, decoded.
    let st = h.ok(&format!("status --ms emergency --index {p}"));
    for line in [
        format!("Emergency #{p}: Active("),
        "approved by 1 of 2 needed".to_string(),
        format!("[0] short.win set_pause (signer {emg_vault}) scope Global, paused true"),
    ] {
        assert!(st.contains(&line), "{line}\n{st}");
    }
    //    A rerun of the same staging (after a send reported as dropped that landed) is refused
    //    while #p is open; --allow-duplicate overrides (dry-run here: nothing sent).
    let e = h.err("stage set-pause global on --key @emg1");
    assert!(
        format!("{e:#}").contains(&format!("Emergency #{p} already holds this message")),
        "{e:#}"
    );
    let r = h.ok("stage set-pause global on --key @emg1 --allow-duplicate --dry-run");
    assert!(
        r.contains(&format!("dry-run: stage Emergency #{}", p + 1)),
        "{r}"
    );
    h.ok(&format!(
        "approve --ms emergency --index {p} --key @emg1 --execute"
    ));
    assert_eq!(h.chain.sizes.borrow().len() - before, 2);
    assert!(h.env().config().paused_opens());
    // Governance cannot use an Emergency-only power, nor the Emergency a propose-*.
    let e = h.err("stage disable-venue raydium_v4 --key @gov0 --ms governance");
    assert!(format!("{e:#}").contains("only the Emergency vault"));
    let e = h.err("stage propose-kink 7000 --key @emg0 --ms emergency");
    assert!(format!("{e:#}").contains("only Governance proposes"));

    //    The worst-case set_venue_extras (4 templates × 4 32-byte seeds): too large to stage in
    //    one transaction, so it goes through the transaction buffer.
    let mut extras = String::new();
    for _ in 0..4 {
        extras.push_str(&format!(
            "[[entry]]\nkind = 2\nkey = \"{}\"\nseeds = [{}]\n",
            Pubkey::new_unique(),
            ["{ kind = 1, hex = \"0707070707070707070707070707070707070707070707070707070707070707\" }"; 4].join(", ")
        ));
    }
    let file = h.dir.join("extras.toml");
    std::fs::write(&file, extras).unwrap();
    let r = h.ok(&format!(
        "stage set-venue-extras pump_swap sell {} --key @emg0",
        file.display()
    ));
    assert!(r.contains("through transaction buffer 0"), "{r}");
    let staged_direct: usize = r
        .split("staged directly: ")
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .unwrap()
        .parse()
        .unwrap();
    assert!(staged_direct > 1_232, "{staged_direct}");
    let ex = staged_index(&r);
    h.ok(&format!(
        "approve --ms emergency --index {ex} --key @emg2 --execute"
    ));
    let ve: VenueExtras = read::zc(
        &h.env().svm,
        &shortwin_ops::ixs::pdas::venue_extras(VenueKind::PumpSwap as u8),
    )
    .unwrap();
    assert_eq!(
        (ve.sets[0].current.count, ve.sets[0].current.version),
        (4, 1)
    );
    //    Three worst-case sets in one message (sell, buy and claim of raydium_cpmm): more than
    //    one buffer create carries, so the rest goes in extends.
    let entries =
        shortwin_ops::cli::parse_extras(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let three: Vec<_> = (0..3)
        .map(|which| {
            shortwin_ops::ixs::set_venue_extras(
                &emg_vault,
                VenueKind::RaydiumCpmm as u8,
                which,
                entries.clone(),
            )
        })
        .collect();
    let (big, text) = {
        let member = h.keypair("emg1");
        let mut out = Vec::new();
        let ops = shortwin_ops::send::Ops::new(&h.chain, false, &mut out);
        let opts = StageOpts {
            buffer_index: 1,
            ..StageOpts::default()
        };
        let idx =
            h.rt.block_on(shortwin_ops::squads::stage(
                &ops, &l.emg, &member, 0, &three, opts,
            ))
            .unwrap();
        drop(ops);
        (idx, String::from_utf8(out).unwrap())
    };
    println!("{text}");
    let message: usize = text
        .split("message ")
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .unwrap()
        .parse()
        .unwrap();
    let created: usize = text
        .split(&format!("buffer Emergency #{big} create ("))
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .unwrap()
        .parse()
        .unwrap();
    let extends: Vec<usize> = text
        .split(&format!("buffer Emergency #{big} extend ("))
        .skip(1)
        .map(|s| s.split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert!(
        message > 2 * created,
        "{message} B over {created} B creates"
    );
    assert!(extends.len() >= 2, "{text}");
    assert_eq!(created + extends.iter().sum::<usize>(), message);
    assert!(text.contains(&format!("stage Emergency #{big} from buffer")));
    h.ok(&format!(
        "approve --ms emergency --index {big} --key @emg2 --execute"
    ));
    let ve: VenueExtras = read::zc(
        &h.env().svm,
        &shortwin_ops::ixs::pdas::venue_extras(VenueKind::RaydiumCpmm as u8),
    )
    .unwrap();
    for set in &ve.sets {
        assert_eq!((set.current.count, set.current.version), (4, 1));
    }

    //    An interrupted --force-buffer staging (its buffer create landed, the run did not
    //    finish): the rerun resumes the buffer with an extend instead of refusing it.
    let emg3 = h.keypair("emg3");
    let off = sq_compile(
        &emg_vault,
        &[shortwin_ops::ixs::set_pause(&emg_vault, None, false)],
    );
    let create = |index: u8, final_bytes: &[u8], chunk: &[u8]| {
        squads_v4_ix::ix::transaction_buffer_create(
            &emg,
            &emg3.pubkey(),
            &emg3.pubkey(),
            index,
            0,
            Sha256::digest(final_bytes).into(),
            final_bytes.len() as u16,
            chunk,
        )
    };
    h.env()
        .send(&[create(0, &off, &off[..40])], &[&emg3])
        .unwrap();
    let r = h.ok("stage set-pause global off --key @emg3 --force-buffer");
    let k = staged_index(&r);
    assert!(
        r.contains(&format!(
            "resuming transaction buffer 0: 40 of {} B already written",
            off.len()
        )),
        "{r}"
    );
    assert!(
        r.contains(&format!(
            "buffer Emergency #{k} extend ({} B)",
            off.len() - 40
        )),
        "{r}"
    );
    assert!(!r.contains("create ("), "no second create:\n{r}");
    let buffer0 = squads_v4_ix::pda::transaction_buffer(&emg, &emg3.pubkey(), 0);
    assert!(
        h.account(&buffer0).is_none(),
        "closed by create_from_buffer"
    );
    // ...and a further rerun is the duplicate of #k.
    let e = h.err("stage set-pause global off --key @emg3 --force-buffer");
    assert!(format!("{e:#}").contains(&format!("Emergency #{k} already holds this message")));
    h.ok(&format!(
        "approve --ms emergency --index {k} --key @emg0 --execute"
    ));
    assert!(!h.env().config().paused_opens());

    //    A buffer holding another message is refused, and close-buffer returns its rent.
    let on = sq_compile(
        &emg_vault,
        &[shortwin_ops::ixs::set_pause(&emg_vault, None, true)],
    );
    h.env().send(&[create(2, &on, &on)], &[&emg3]).unwrap();
    let buffer2 = squads_v4_ix::pda::transaction_buffer(&emg, &emg3.pubkey(), 2);
    let rent = h.account(&buffer2).unwrap().lamports;
    let e = h.err(
        "stage set-route-broken global raydium_v4 on --key @emg3 --force-buffer --buffer-index 2",
    );
    assert!(
        format!("{e:#}").contains(&format!(
            "transaction buffer 2 ({buffer2}) holds another message ({} of {} B written",
            on.len(),
            on.len()
        )),
        "{e:#}"
    );
    let e = h.err("close-buffer --ms emergency --key @emg3 --buffer-index 3");
    assert!(format!("{e:#}").contains("has no transaction buffer 3"));
    let lamports = h.account(&emg3.pubkey()).unwrap().lamports;
    let r = h.ok("close-buffer --ms emergency --key @emg3 --buffer-index 2");
    assert!(r.contains(&format!("{rent} lamports back to")), "{r}");
    assert!(h.account(&buffer2).is_none());
    assert!(h.account(&emg3.pubkey()).unwrap().lamports > lamports + rent / 2);

    //    The overview: both multisigs' recent transactions, no pending config change.
    let st = h.ok("status");
    assert!(
        st.contains(&format!("Emergency: transaction index {k}"))
            && st.contains("no pending config change"),
        "{st}"
    );

    // 7. verify passes; the report covers every derived address.
    let (passed, text) = h.verify();
    assert!(passed, "{text}");
    for line in [
        format!("ok   Governance multisig {gov}"),
        format!("ok   Emergency multisig {emg}"),
        "ok   Governance threshold: 3".to_string(),
        "ok   Governance time lock (s): 172800".to_string(),
        "ok   Emergency threshold: 2".to_string(),
        "ok   Emergency time lock (s): 0".to_string(),
        format!("ok   config.governance: {vault}"),
        format!("ok   config.pause_authority: {}", l.pause_authority()),
        format!("ok   config.coverage_authority: {new_cov}"),
        format!("ok   config.fee_recipient: {}", l.fee_recipient()),
        "ok   authorities pairwise distinct (SQ-G8): true".to_string(),
        format!("ok   shortwin upgrade authority: {vault} (Governance vault 0)"),
    ] {
        assert!(text.contains(&line), "{line}\n{text}");
    }
    assert!(
        !text.contains("complete_curve"),
        "the retired helper is not checked:\n{text}"
    );
    assert!(!text.contains("WARN"), "nothing left pending:\n{text}");
    std::fs::write(h.dir.join("verify.txt"), &text).unwrap();
    // Every transaction the tool sent fit the packet limit; the largest:
    let max = *h.chain.sizes.borrow().iter().max().unwrap();
    println!(
        "T-OPS-2: {} transactions through Env::send, largest {max} B",
        h.chain.sizes.borrow().len()
    );

    // 8. A drifted layout fails verify: a member the chain does not have, a wrong threshold.
    let text = std::fs::read_to_string(&h.config).unwrap();
    let gov4 = h.pubkey("gov4");
    let stranger = Keypair::new().pubkey();
    std::fs::write(
        &h.config,
        text.replacen(&gov4.to_string(), &stranger.to_string(), 1)
            .replacen("threshold = 2", "threshold = 3", 1),
    )
    .unwrap();
    let (passed, text) = h.verify();
    assert!(!passed);
    assert!(
        text.contains(&format!(
            "FAIL Governance member: {gov4} is not in the layout"
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("FAIL Governance member: {stranger} is missing")),
        "{text}"
    );
    assert!(
        text.contains("FAIL Emergency threshold: 2, expected 3"),
        "{text}"
    );
}
