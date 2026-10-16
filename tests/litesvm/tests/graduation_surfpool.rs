//! **Surfpool** mainnet-fork E2E through a real graduation (ARCH §8.4 "Fork E2E: Surfpool 1.6",
//! §9 M6 row "Surfpool E2E through a real graduation"; U19). `#[ignore]`d: it needs a running
//! Surfpool 1.6 (the release binary, `surfpool-linux-x64.tar.gz` of v1.6.0, sha256 `cb471b00…`)
//! forking mainnet:
//!
//! ```sh
//! surfpool start --rpc-url <mainnet RPC> --no-tui --no-studio --no-deploy \
//!     --port 18899 --ws-port 18900 --airdrop-amount 0
//! SURFPOOL_RPC=127.0.0.1:18899 cargo test -p shortwin-litesvm-tests --test graduation_surfpool \
//!     -- --ignored --nocapture          # SURFPOOL_TRACE=1: every transaction's CU and errors
//! ```
//!
//! The mainnet RPC behind Surfpool must accept its batched `getMultipleAccounts`: on 2026-09-30
//! `solana-rpc.publicnode.com` answered Surfpool's batches with HTTP 403 (the same batches sent by
//! `curl` succeed) and `api.mainnet-beta.solana.com` dropped some under its rate limit; a local
//! forwarder that retries with a `curl` user agent (≈ 40 lines of Python, stdlib only) made every
//! run pass. `getProgramAccounts` is refused by both public endpoints, so the harness mirrors the
//! accounts it watches instead of scanning short.win's.
//!
//! Unlike every LiteSVM binary, **nothing of the venues is dumped**: pump, pump-fees, PumpSwap,
//! Token-2022, the ATA and ALT programs and every venue-state account (pump's `Global` and
//! `FeeConfig`, PumpSwap's `GlobalConfig` and `FeeConfig`, the fee recipients, the Pyth feed, the
//! venues' ProgramData with their live deploy slots) are **mainnet's as of the run**, fetched
//! lazily by Surfpool. What the test plants with `surfnet_setAccount` (the M5 scenario builder's
//! allowance, ARCH §8.2): the release short.win (its ProgramData **before** the program account —
//! Surfpool loads a program when the program account is written, and a program written first
//! runs as a 2-CU no-op), the funded harness keys, ARCH §6.4's protocol ALT (A28) and the `fresh`
//! coin of the fixtures (mint, curve with its 1-lamport creation buy undone as `Sim::d_market`
//! does, the curve's token account, the creator vault) — its mainnet curve never graduated, so its
//! canonical pool does not exist on mainnet either and `migrate_v2` creates it on the fork.
//!
//! The path, all real instructions sent over JSON-RPC: `initialize_config` (the live venue deploy
//! slots must equal `fork::FIXTURE_GUARD_SLOTS`, the 2026-10-02 pump redeploys; 10 accounts and
//! 730 data bytes: 1,294 B with a separate fee payer, over the 1,232-B limit, so the upgrade
//! authority pays its own fee: 1,198 B) → a trader's `buy_v2` to ECON's D@80 % (the live pump lands on `at_progress(0.8)`
//! exactly) → `create_market(Auto)` (PumpCurve) → the trader lends its tokens → a borrower opens
//! ECON's D@80 % position, **`S`, `M`, `V` equal to the dumped ELFs' to the lamport** → the trader
//! completes the curve (`PumpCurve → PumpMigrating`) → the trader's `migrate_v2` on the live pump
//! and PumpSwap → the keeper's crank (`→ PumpSwap`, the ring at `lp_supply`, `(8, 1)`) → the
//! existence requirement's accounts → per-second cranks (`surfnet_timeTravel`), liquidations
//! refused `EmaWarmingUp` until the eighth integration → the position liquidated on the pool,
//! `Q` back to the lenders. Every transaction too large as legacy goes as v0 through the protocol
//! ALT (the open 1,648 → 1,033 B, a PumpSwap liquidation 1,509 → 1,018 B). No boost is spent
//! (pump's boost authority does not run on the fork), unlike `graduation_scenarios`.
//!
//! **Invariants.** After every transaction the harness mirrors Surfpool's accounts (every
//! short.win account the test creates, the market's venue and token accounts, the Clock) into a
//! LiteSVM and runs `invariants::check_all` on the market (the per-market half of the hooks; the
//! per-instruction `check_ix` needs LiteSVM's own transaction metadata and runs only in the
//! LiteSVM binaries).
//!
//! The JSON-RPC client is `std::net::TcpStream` + `serde_json` (no new dependency): HTTP/1.1,
//! the wire transaction built from `Transaction::message_data` (legacy) or the harness's
//! `raydium::v0_message` (v0).

#![allow(clippy::result_large_err)]

#[path = "graduation_support.rs"]
mod support;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::str::FromStr;

use anchor_lang::__private::base64::{self, Engine};
use anchor_lang::{AnchorDeserialize, Discriminator, Event};
use serde_json::{Value, json};
use shortwin::constants::VENUE_PROGRAM_DATA;
use shortwin_litesvm_tests::{invariants, raydium};
use solana_account::Account;
use solana_transaction::Transaction;
use support::*;

const D80_SOLD: u64 = 634_480_000_000_000;
const D80_Q: u64 = 6_529_192_912_424;
/// The D@80 % open on the dumped ELFs (`graduation_scenarios`, `pump_curve_scenarios`).
const D80_OPEN: (u64, u64, u64) = (1_063_457_547, 3_429_906_865, 4_493_364_412);

// ---- a minimal JSON-RPC client ------------------------------------------------------------------

struct Rpc {
    addr: String,
}

impl Rpc {
    fn call(&self, method: &str, params: Value) -> Value {
        self.try_call(method, params)
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    fn try_call(&self, method: &str, params: Value) -> Result<Value, Value> {
        let body =
            json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
        let mut s = TcpStream::connect(&self.addr)
            .unwrap_or_else(|e| panic!("connect {}: {e} (is Surfpool running?)", self.addr));
        write!(
            s,
            "POST / HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.addr,
            body.len()
        )
        .expect("write");
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).expect("read");
        let split = buf
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("an HTTP response");
        let head = String::from_utf8_lossy(&buf[..split]).to_ascii_lowercase();
        let mut rest = buf[split + 4..].to_vec();
        if head.contains("transfer-encoding: chunked") {
            rest = dechunk(&rest);
        }
        let v: Value = serde_json::from_slice(&rest).expect("JSON");
        if let Some(e) = v.get("error") {
            return Err(e.clone());
        }
        Ok(v["result"].clone())
    }
}

fn dechunk(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        let eol = b[i..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .expect("chunk size")
            + i;
        let n = usize::from_str_radix(std::str::from_utf8(&b[i..eol]).unwrap().trim(), 16).unwrap();
        if n == 0 {
            return out;
        }
        out.extend_from_slice(&b[eol + 2..eol + 2 + n]);
        i = eol + 2 + n + 2;
    }
}

fn b58_decode(s: &str) -> Vec<u8> {
    const A: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut out: Vec<u8> = Vec::new();
    for c in s.bytes() {
        let mut carry = A.iter().position(|&x| x == c).expect("base58") as u32;
        for b in out.iter_mut().rev() {
            carry += u32::from(*b) * 58;
            *b = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            out.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let zeros = s.bytes().take_while(|&c| c == b'1').count();
    let mut v = vec![0u8; zeros];
    v.extend(out);
    v
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A landed transaction as `getTransaction` reports it.
struct Landed {
    err: Option<Value>,
    logs: Vec<String>,
    cu: u64,
    /// short.win's `emit_cpi!` payloads (event discriminator ‖ Borsh).
    events: Vec<Vec<u8>>,
}

impl Landed {
    fn code(&self) -> Option<u32> {
        let e = self.err.as_ref()?;
        e["InstructionError"][1]["Custom"]
            .as_u64()
            .map(|c| c as u32)
    }

    fn ok(&self) -> &Self {
        assert!(
            self.err.is_none(),
            "failed: {:?}\n{}",
            self.err,
            self.logs.join("\n")
        );
        self
    }

    fn of<T: Event + Discriminator + AnchorDeserialize>(&self) -> Vec<T> {
        self.events
            .iter()
            .filter_map(|p| T::try_from_slice(p.strip_prefix(T::DISCRIMINATOR)?).ok())
            .collect()
    }

    fn one<T: Event + Discriminator + AnchorDeserialize>(&self) -> T {
        let mut v = self.of::<T>();
        assert_eq!(
            v.len(),
            1,
            "one {}\n{}",
            core::any::type_name::<T>(),
            self.logs.join("\n")
        );
        v.remove(0)
    }
}

/// Surfpool as the chain; an `Env` as the wallet (the harness keys), the instruction builders'
/// account source and the invariant hooks' mirror.
struct Surf {
    rpc: Rpc,
    env: Env,
    watched: Vec<Pubkey>,
    market: Option<Pubkey>,
    /// The lookup table's addresses (planted at [`ALT_KEY`]).
    alt: Vec<Pubkey>,
    /// `(legacy, v0)` sizes of the transactions sent as v0.
    sizes: Vec<(usize, usize)>,
}

/// The lookup table's address on the fork.
const ALT_KEY: Pubkey = Pubkey::new_from_array([0xA2; 32]);

/// ARCH §6.4's protocol ALT (A28): the Raydium part (`raydium::protocol_alt_addresses`), the pump
/// part (the three programs, pump's `Global`, event authority, `FeeConfig` and global volume
/// accumulator, PumpSwap's four statics) and the live pump and PumpSwap fee and buyback
/// recipients with their WSOL ATAs. Per-market accounts stay inline (ARCH §6.4).
fn a28(svm: &litesvm::LiteSVM) -> Vec<Pubkey> {
    use shortwin::constants::*;
    let mut v = raydium::protocol_alt_addresses();
    v.extend([
        PUMP_PROGRAM_ID,
        PUMP_AMM_PROGRAM_ID,
        PUMP_FEES_PROGRAM_ID,
        PUMP_GLOBAL,
        PUMP_EVENT_AUTHORITY,
        PUMP_FEE_CONFIG,
        PUMP_GLOBAL_VOLUME_ACCUMULATOR,
        PUMP_AMM_GLOBAL_CONFIG,
        PUMP_AMM_EVENT_AUTHORITY,
        PUMP_AMM_FEE_CONFIG,
        PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR,
    ]);
    for i in 0..8 {
        for k in [
            fork::pump_fee_recipient(svm, i),
            fork::pump_buyback_fee_recipient(svm, i),
            pamm::protocol_fee_recipient(svm, i),
            pamm::buyback_fee_recipient(svm, i),
        ] {
            if !v.contains(&k) {
                v.push(k);
                v.push(pump::wsol_ata(&k));
            }
        }
    }
    v
}

impl Surf {
    fn set_account(&self, key: &Pubkey, acc: &Account) {
        self.rpc.call(
            "surfnet_setAccount",
            json!([key.to_string(), {
                "lamports": acc.lamports,
                "data": hex(&acc.data),
                "owner": acc.owner.to_string(),
                "executable": acc.executable,
            }]),
        );
    }

    fn fund(&self, key: &Pubkey, lamports: u64) {
        self.set_account(
            key,
            &Account {
                lamports,
                data: vec![],
                owner: anchor_lang::system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        );
    }

    fn get(&self, keys: &[Pubkey]) -> Vec<Option<Account>> {
        let mut out = Vec::new();
        for chunk in keys.chunks(100) {
            let ks: Vec<String> = chunk.iter().map(ToString::to_string).collect();
            // The public RPC behind Surfpool sometimes refuses a batch (HTTP 403): one by one.
            let vals: Vec<Value> = match self
                .rpc
                .try_call("getMultipleAccounts", json!([ks, {"encoding": "base64"}]))
            {
                Ok(r) => r["value"].as_array().expect("value").clone(),
                Err(e) => {
                    eprintln!("getMultipleAccounts refused ({e}); fetching one by one");
                    ks.iter()
                        .map(|k| {
                            self.rpc
                                .call("getAccountInfo", json!([k, {"encoding": "base64"}]))["value"]
                                .clone()
                        })
                        .collect()
                }
            };
            for v in &vals {
                out.push(if v.is_null() {
                    None
                } else {
                    let data = base64::engine::general_purpose::STANDARD
                        .decode(v["data"][0].as_str().unwrap())
                        .unwrap();
                    Some(Account {
                        lamports: v["lamports"].as_u64().unwrap(),
                        data,
                        owner: Pubkey::from_str(v["owner"].as_str().unwrap()).unwrap(),
                        executable: v["executable"].as_bool().unwrap(),
                        rent_epoch: 0,
                    })
                });
            }
        }
        out
    }

    /// Chain time (the Clock sysvar).
    fn clock(&self) -> (i64, u64) {
        let c = self.get(&[anchor_lang::prelude::pubkey!(
            "SysvarC1ock11111111111111111111111111111111"
        )])[0]
            .clone()
            .expect("Clock");
        let slot = u64::from_le_bytes(c.data[0..8].try_into().unwrap());
        let ts = i64::from_le_bytes(c.data[32..40].try_into().unwrap());
        (ts, slot)
    }

    /// `surfnet_timeTravel` to `now + secs`.
    fn warp(&self, secs: i64) {
        let (now, _) = self.clock();
        self.rpc.call(
            "surfnet_timeTravel",
            json!([{"absoluteTimestamp": (now + secs) * 1_000}]),
        );
        let (after, _) = self.clock();
        assert!(after >= now + secs, "time travelled {now} → {after}");
    }

    /// Copies every watched account (the short.win accounts the test creates are watched as
    /// they appear: the public RPC behind Surfpool refuses `getProgramAccounts`) and the Clock
    /// into the mirror; the market's share-epoch records are added as the market names them.
    fn pull(&mut self) {
        if let Some(m) = self.market
            && let Some(mk) = read::zc::<Market>(&self.env.svm, &m)
        {
            let epochs: Vec<Pubkey> = (0..=mk.share_epoch)
                .map(|e| pda::share_epoch(&m, e).0)
                .collect();
            self.watch(&epochs);
        }
        let keys = self.watched.clone();
        for (k, a) in keys.iter().zip(self.get(&keys)) {
            let acc = a.unwrap_or_default();
            self.env.svm.set_account(*k, acc).expect("mirror");
        }
        let (ts, slot) = self.clock();
        self.env.set_clock(ts, slot);
    }

    fn watch(&mut self, keys: &[Pubkey]) {
        for k in keys {
            if !self.watched.contains(k) {
                self.watched.push(*k);
            }
        }
    }

    /// Sends `ixs` (the harness payer pays) and waits for it; then mirrors and runs
    /// `invariants::check_all` on the market.
    fn send(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> Landed {
        let payer = self.env.payer.insecure_clone();
        self.send_with(&payer, ixs, signers)
    }

    /// [`Surf::send`] with `payer` as the fee payer.
    fn send_with(&mut self, payer: &Keypair, ixs: &[Instruction], signers: &[&Keypair]) -> Landed {
        let bh = self.rpc.call("getLatestBlockhash", json!([]));
        let hash =
            solana_message::Hash::from_str(bh["value"]["blockhash"].as_str().unwrap()).unwrap();
        let payer = payer.insecure_clone();
        let needed = |k: &Pubkey| {
            ixs.iter()
                .flat_map(|i| i.accounts.iter())
                .any(|m| m.is_signer && m.pubkey == *k)
        };
        let mut all: Vec<&Keypair> = vec![&payer];
        all.extend(
            signers
                .iter()
                .copied()
                .filter(|s| needed(&s.pubkey()) && s.pubkey() != payer.pubkey()),
        );
        let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, hash);
        let mut wire = vec![u8::try_from(tx.signatures.len()).unwrap()];
        for s in &tx.signatures {
            wire.extend_from_slice(s.as_ref());
        }
        wire.extend(tx.message_data());
        if wire.len() > 1_232 {
            // Too large as legacy (every short.win trade on a pump venue): a v0 transaction
            // through the lookup table (ARCH §6.4, A28).
            let legacy = wire.len();
            let msg = raydium::v0_message(&payer.pubkey(), ixs, &ALT_KEY, &self.alt, hash);
            let vtx = solana_transaction::versioned::VersionedTransaction::try_new(msg, &all)
                .expect("sign v0");
            wire = vec![u8::try_from(vtx.signatures.len()).unwrap()];
            for s in &vtx.signatures {
                wire.extend_from_slice(s.as_ref());
            }
            wire.extend(vtx.message.serialize());
            self.sizes.push((legacy, wire.len()));
            assert!(
                wire.len() <= 1_232,
                "{} B even as v0 through the ALT (legacy {legacy} B)",
                wire.len()
            );
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&wire);
        let sig = self.rpc.call(
            "sendTransaction",
            json!([b64, {"encoding": "base64", "skipPreflight": true}]),
        );
        let sig = sig.as_str().expect("signature").to_string();
        let mut tries = 0;
        let tx = loop {
            let t = self.rpc.call(
                "getTransaction",
                json!([sig, {"encoding": "json", "maxSupportedTransactionVersion": 0, "commitment": "confirmed"}]),
            );
            if !t.is_null() {
                break t;
            }
            tries += 1;
            assert!(tries < 400, "transaction {sig} never landed");
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let meta = &tx["meta"];
        let mut keys: Vec<String> = tx["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap().to_string())
            .collect();
        for part in ["writable", "readonly"] {
            for k in meta["loadedAddresses"][part]
                .as_array()
                .cloned()
                .unwrap_or_default()
            {
                keys.push(k.as_str().unwrap().to_string());
            }
        }
        let me = shortwin::ID.to_string();
        let tag = anchor_lang::event::EVENT_IX_TAG_LE;
        let mut events = Vec::new();
        for group in meta["innerInstructions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            for ix in group["instructions"].as_array().unwrap() {
                let prog = &keys[ix["programIdIndex"].as_u64().unwrap() as usize];
                if *prog != me {
                    continue;
                }
                let d = b58_decode(ix["data"].as_str().unwrap());
                if let Some(p) = d.strip_prefix(tag) {
                    events.push(p.to_vec());
                }
            }
        }
        let landed = Landed {
            err: (!meta["err"].is_null()).then(|| meta["err"].clone()),
            logs: meta["logMessages"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .map(|l| l.as_str().unwrap_or("").to_string())
                        .collect()
                })
                .unwrap_or_default(),
            cu: meta["computeUnitsConsumed"].as_u64().unwrap_or(0),
            events,
        };
        if std::env::var_os("SURFPOOL_TRACE").is_some() {
            eprintln!(
                "tx {sig}: err {:?}, {} CU, {} events, logs {:?}",
                landed.err,
                landed.cu,
                landed.events.len(),
                landed
                    .logs
                    .iter()
                    .filter(|l| l.contains("5yvy") || l.contains("Error"))
                    .take(4)
                    .collect::<Vec<_>>()
            );
        }
        self.pull();
        if let Some(m) = self.market
            && let Err(v) = invariants::check_all(&self.env.svm, &m)
        {
            panic!(
                "invariant on the Surfpool state: {v}\nlogs:\n{}",
                landed.logs.join("\n")
            );
        }
        landed
    }
}

/// The Surfpool mainnet-fork E2E (module docs).
#[test]
#[ignore = "needs a running Surfpool 1.6 mainnet fork (SURFPOOL_RPC=host:port); module docs"]
fn surfpool_mainnet_fork_graduation() {
    let addr = std::env::var("SURFPOOL_RPC").unwrap_or_else(|_| "127.0.0.1:8899".into());
    let mut s = Surf {
        rpc: Rpc { addr },
        env: Env::new(),
        watched: Vec::new(),
        market: None,
        alt: Vec::new(),
        sizes: Vec::new(),
    };
    let v = s.rpc.call("getVersion", json!([]));
    eprintln!("Surfpool {v}");

    // ---- the live venue deploy slots are the VenueGuard baseline ----
    let pds = s.get(&VENUE_PROGRAM_DATA[..3]);
    for (i, pd) in pds.iter().enumerate() {
        let pd = pd.as_ref().expect("a live ProgramData");
        let slot = u64::from_le_bytes(pd.data[4..12].try_into().unwrap());
        eprintln!("venue {i}: live deploy slot {slot}");
        assert_eq!(
            slot,
            shortwin_litesvm_tests::fork::FIXTURE_GUARD_SLOTS[i],
            "no upgrade since 2026-10-02"
        );
    }

    // ---- plant short.win and the harness keys ----
    // ProgramData first: Surfpool loads the program when the program account is written.
    for k in [pda::program_data(), shortwin::ID] {
        let acc = s.env.svm.get_account(&k).expect("the program");
        s.set_account(&k, &acc);
    }
    for k in [
        s.env.payer.pubkey(),
        s.env.upgrade_authority.pubkey(),
        s.env.governance.pubkey(),
        s.env.pause_authority.pubkey(),
        s.env.fee_recipient.pubkey(),
    ] {
        s.fund(&k, 1_000 * SOL);
    }
    // ---- plant the fresh coin (its 1-lamport creation buy undone) ----
    for f in fork::all_pump_fixtures().filter(|f| f.role.as_deref() == Some("fresh")) {
        let mut acc = f.account.clone();
        if f.name == "pump_fresh_curve" {
            let d = &mut acc.data;
            d[shortwin_venue_state::pump::OFF_VIRTUAL_TOKEN_RESERVES..][..8]
                .copy_from_slice(&PUMP_VT0.to_le_bytes());
            d[shortwin_venue_state::pump::OFF_VIRTUAL_QUOTE_RESERVES..][..8]
                .copy_from_slice(&PUMP_VS0.to_le_bytes());
            d[shortwin_venue_state::pump::OFF_REAL_TOKEN_RESERVES..][..8]
                .copy_from_slice(&PUMP_RT0.to_le_bytes());
            d[shortwin_venue_state::pump::OFF_REAL_QUOTE_RESERVES..][..8]
                .copy_from_slice(&0u64.to_le_bytes());
        }
        s.set_account(&f.address, &acc);
    }
    let c = fork::PumpCurve::of(PumpCoin::Fresh);
    let market = pda::market(&c.mint).0;
    let keys = shortwin_litesvm_tests::fixtures::MarketKeys {
        mint: c.mint,
        token_program: c.token_program.id(),
        market,
        market_vault: pda::market_vault(&market).0,
        pool_vault: pda::pool_vault(&market).0,
        trader: pda::trader(&market).0,
    };
    let np = pump::NewPool::of(&c);
    s.watch(&[
        shortwin::constants::PUMP_GLOBAL,
        shortwin::constants::PUMP_FEE_CONFIG,
        shortwin::constants::PUMP_AMM_GLOBAL_CONFIG,
        shortwin::constants::PUMP_AMM_FEE_CONFIG,
        c.mint,
        c.curve,
        c.curve_base_ata,
        keys.pool_vault,
        keys.trader,
        pda::ata(&keys.trader, &c.mint, &keys.token_program),
        pda::trader_wsol(&market).0,
        np.pool,
        np.base_vault,
        np.quote_vault,
        np.lp_mint,
        np.boost_vault_ata,
    ]);
    s.watch(&[
        pda::config(),
        pda::venue_guard(),
        pda::oracle_cache(),
        pda::venue_extras(VenueKind::PumpCurve),
        pda::venue_extras(VenueKind::PumpSwap),
        pda::venue_extras(VenueKind::RaydiumV4),
        pda::venue_extras(VenueKind::RaydiumCpmm),
        market,
        keys.market_vault,
    ]);
    s.pull();
    s.watch(&[c.creator_vault(&s.env.svm)]);
    // The protocol ALT, planted active (extended at slot 0, never deactivated).
    s.alt = a28(&s.env.svm);
    {
        let mut data = vec![0u8; 56];
        data[0..4].copy_from_slice(&1u32.to_le_bytes());
        data[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
        for a in &s.alt {
            data.extend_from_slice(a.as_ref());
        }
        s.set_account(
            &ALT_KEY,
            &Account {
                lamports: 1_000_000_000,
                data,
                owner: raydium::ALT_PROGRAM,
                executable: false,
                rent_epoch: 0,
            },
        );
    }

    // ---- initialize_config with the live deploy slots ----
    // 10 accounts and 730 bytes of data: with a separate fee payer the legacy transaction is
    // 1,294 B (> 1,232, refused); with the upgrade authority paying its own fee, 1,198 B.
    // ECON's sizing (`econ_params`: the wallet cap lifted, as every ECON replay).
    let mut args = s.env.default_init_args();
    econ_params(&mut args);
    let auth = s.env.upgrade_authority.insecure_clone();
    s.send_with(
        &auth,
        &[shortwin_litesvm_tests::ix::initialize_config(
            &auth.pubkey(),
            args,
        )],
        &[&auth],
    )
    .ok();

    // ---- a trader buys to ECON's D@80 % on the live pump program ----
    let trader = Keypair::new();
    s.fund(&trader.pubkey(), 1_000 * SOL);
    let trader_ata = pda::ata(&trader.pubkey(), &c.mint, &keys.token_program);
    s.watch(&[trader_ata]);
    let payer = s.env.payer.pubkey();
    s.send(
        &[
            fork::create_ata_idempotent_ix(&payer, &trader.pubkey(), &c.mint, &keys.token_program),
            fork::pump_init_uva_ix(&payer, &trader.pubkey()),
        ],
        &[],
    )
    .ok();
    let fr = fork::pump_fee_recipient(&s.env.svm, 1);
    let bb = fork::pump_buyback_fee_recipient(&s.env.svm, 1);
    let buy = c.v2_ix(
        &s.env.svm,
        fork::PumpV2::Buy,
        &trader.pubkey(),
        &trader_ata,
        &fr,
        &bb,
        D80_SOLD,
        u64::MAX,
    );
    let l = s.send(&[pump::cu_limit_ix(300_000), buy], &[&trader]);
    l.ok();
    let st = c.curve_state(&s.env.svm);
    eprintln!(
        "the live curve at D@80 %: vT {} vQ {} ({} CU)",
        st.vt, st.vq, l.cu
    );
    assert_eq!(
        (st.vt, st.vq),
        (438_520_000_000_000, 73_406_002_007),
        "at_progress(0.8) on the live program"
    );

    // ---- list, lend, warm up ----
    let lister = Keypair::new();
    s.fund(&lister.pubkey(), 10 * SOL);
    let l = s.send(
        &[pump::create_pump_market_ix(&lister.pubkey(), &c)],
        &[&lister],
    );
    l.ok();
    let created: shortwin::events::MarketCreated = l.one();
    assert_eq!(created.venue, VenueKind::PumpCurve as u8);
    s.market = Some(market);
    let pm = PumpMarket {
        trader_base_ata: pda::ata(&keys.trader, &c.mint, &keys.token_program),
        fee_recipient: fork::pump_fee_recipient(&s.env.svm, 0),
        buyback_recipient: fork::pump_buyback_fee_recipient(&s.env.svm, 0),
        creator_vault: c.creator_vault(&s.env.svm),
        curve: c,
        keys,
        lender: trader.insecure_clone(),
        lender_account: trader_ata,
    };
    s.watch(&[pda::lender(&market, &trader.pubkey()).0]);
    let lend = lend_for_80(D80_Q);
    s.send(
        &[shortwin_litesvm_tests::ix::deposit(
            &pm.lender_keys(),
            lend,
            0,
        )],
        &[&trader],
    )
    .ok();
    s.warp(i64::from(s.env.config().params.market_warmup_s) + 10);
    s.send(&[crank_price_ix(&pm)], &[]).ok();

    // ---- the borrower's D@80 % position ----
    let borrower = Keypair::new();
    s.fund(&borrower.pubkey(), 100 * SOL);
    s.watch(&[
        borrower.pubkey(),
        pda::position(&market, &borrower.pubkey(), 0).0,
        pda::exposure(&market, &borrower.pubkey()).0,
    ]);
    let open = open_short_ix(
        &pm,
        &borrower.pubkey(),
        0,
        D80_Q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    let l = s.send(
        &[pump::cu_limit_ix(pump::CURVE_TRADE_CU), open],
        &[&borrower],
    );
    l.ok();
    let o: Open = l.one();
    eprintln!(
        "open on the live curve: S {} M {} V {} ({} CU)",
        o.s, o.m, o.v, l.cu
    );
    assert_eq!(
        (o.s, o.m, o.v),
        D80_OPEN,
        "the dumped ELFs' figures on the live programs"
    );

    // ---- the trader completes the curve: PumpMigrating ----
    s.warp(5);
    s.send(&[crank_price_ix(&pm)], &[]).ok();
    let rt = u64::try_from(pm.curve.curve_state(&s.env.svm).rt).unwrap();
    let buy = pm.curve.v2_ix(
        &s.env.svm,
        fork::PumpV2::Buy,
        &trader.pubkey(),
        &trader_ata,
        &fr,
        &bb,
        rt,
        u64::MAX,
    );
    s.send(&[pump::cu_limit_ix(300_000), buy], &[&trader]).ok();
    s.warp(1);
    let l = s.send(&[crank_price_ix(&pm)], &[]);
    let vs: VenueStateChanged = l.ok().one();
    assert_eq!(
        (vs.from, vs.to),
        (VenueKind::PumpCurve as u8, VenueKind::PumpMigrating as u8)
    );

    // ---- migrate_v2 on the live pump / PumpSwap, then the keeper's crank → PumpSwap ----
    s.warp(1);
    let mig = pump::migrate_v2_ix(&s.env.svm, &trader.pubkey(), &pm.curve);
    let l = s.send(&[pump::cu_limit_ix(MIGRATE_CU), mig], &[&trader]);
    l.ok();
    eprintln!("migrate_v2 on the live programs: {} CU", l.cu);
    let l = s.send(&[crank_price_ix(&pm)], &[]);
    let vs: VenueStateChanged = l.ok().one();
    assert_eq!(
        (vs.from, vs.to),
        (VenueKind::PumpMigrating as u8, VenueKind::PumpSwap as u8)
    );
    let m = s.env.market(&market);
    let pool = PammPool::of(&s.env.svm, &c.mint);
    assert_eq!((m.ema_warm, m.ema_guard), (8, 1));
    assert_eq!(
        m.ema_liq.get(),
        u128::from(pool.state(&s.env.svm).lp_supply)
    );
    s.watch(&[pool.boost_vault_ata]);
    let pam = PammMarket::graduated(&s.env.svm, &pm);
    let ixs = pamm::venue_account_ixs(&s.env.svm, &payer, &pam);
    s.send(&ixs, &[]).ok();

    // ---- per-second cranks: EmaWarmingUp, then the liquidation on the pool ----
    let keeper = Keypair::new();
    s.fund(&keeper.pubkey(), 10 * SOL);
    let tokens_free0 = s.env.market(&market).tokens_free;
    let mut liquidated = None;
    for sec in 1..=40 {
        s.warp(1);
        s.send(&[crank_price_ix(&pam)], &[]).ok();
        let warm = s.env.market(&market).ema_warm;
        let liq = liquidate_ix(&pam, &keeper.pubkey(), &borrower.pubkey(), 0);
        let l = s.send(&[pump::cu_limit_ix(POOL_KEEPER_EXIT_CU), liq], &[&keeper]);
        match l.code() {
            None => {
                let ev: Liquidate = l.one();
                liquidated = Some((sec, ev, l.cu));
                break;
            }
            Some(c) if warm > 0 => assert_eq!(c, u32::from(E::EmaWarmingUp), "+{sec} s"),
            Some(c) => assert!(
                c == u32::from(E::NotLiquidatable) || c == u32::from(E::ExitBoundNotMet),
                "+{sec} s: {c}"
            ),
        }
    }
    let (sec, ev, cu) = liquidated.expect("liquidated on the pool");
    eprintln!(
        "Surfpool: liquidated on the live PumpSwap {sec} s after the transition: B {:.4} SOL, penalty {:.4}, {cu} CU",
        ev.buyback_cost as f64 / 1e9,
        ev.penalty as f64 / 1e9
    );
    assert!(sec >= 8, "after the warm-up");
    assert_eq!(
        s.env.market(&market).tokens_free,
        tokens_free0 + D80_Q,
        "Q back"
    );
    assert_eq!(s.env.market(&market).open_positions, 0);
    eprintln!(
        "v0 through the protocol ALT (legacy → v0 bytes): {:?}",
        s.sizes
    );
}
