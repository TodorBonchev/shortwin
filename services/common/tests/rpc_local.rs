//! The JSON-RPC backend against a live local cluster (ignored by default: it needs one).
//!
//! ```sh
//! solana-test-validator --reset --quiet &
//! SHORTWIN_RPC_URL=http://127.0.0.1:8899 \
//!   cargo test --manifest-path services/Cargo.toml -p shortwin-services-common --test rpc_local -- --ignored
//! ```
//!
//! Clock, accounts, account headers (`dataSlice`, also as of a minimum context slot), the
//! finalized slot, the slots of a simulation and a landed transaction, program accounts, recent
//! fees, and a system transfer simulated and sent in both formats: v1 where the cluster accepts it (else the refusal must come back as
//! `TxError::Rejected`, which the keeper's v0 fallback keys on), and v0.

use std::time::Duration;

use shortwin_services_common::rpc::RpcChain;
use shortwin_services_common::tx::{self, ComputeBudget, TxFormat};
use shortwin_services_common::{Chain, Memcmp, TxError};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

fn transfer(from: &Pubkey, to: &Pubkey, lamports: u64) -> Instruction {
    // System program `Transfer` (instruction 2, bincode: u32 tag, u64 lamports).
    let mut data = 2u32.to_le_bytes().to_vec();
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: Pubkey::default(),
        accounts: vec![AccountMeta::new(*from, true), AccountMeta::new(*to, false)],
        data,
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "needs a local validator: SHORTWIN_RPC_URL=http://127.0.0.1:8899"]
async fn the_rpc_backend_reads_simulates_and_sends() {
    let url = std::env::var("SHORTWIN_RPC_URL").expect("SHORTWIN_RPC_URL");
    let chain = RpcChain::new(url);
    let clock = chain.clock().await.unwrap();
    assert!(
        clock.slot > 0 && clock.unix_timestamp > 1_700_000_000,
        "{clock:?}"
    );

    let payer = Keypair::new();
    let sig = chain
        .client()
        .request_airdrop(&payer.pubkey(), 10_000_000_000)
        .await
        .unwrap();
    for _ in 0..60 {
        if chain
            .client()
            .confirm_transaction(&sig)
            .await
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let to = Keypair::new().pubkey();
    let mut expected = 0u64;
    let mut formats = Vec::new();
    for (i, format) in [TxFormat::V1, TxFormat::V0].into_iter().enumerate() {
        let lamports = 1_000_000 + i as u64;
        let ixs = [transfer(&payer.pubkey(), &to, lamports)];
        let bh = chain.latest_blockhash().await.unwrap();
        // Simulate with the price the send uses: v0 then carries the same ComputeBudget
        // instructions, which consume compute units too.
        let price = 1_000;
        let sim_budget = ComputeBudget {
            unit_price: price,
            ..ComputeBudget::SIMULATION
        };
        let t = tx::build(format, &payer, &[], &ixs, &sim_budget, bh, &[]).unwrap();
        let sim = chain.simulate(&t).await.unwrap();
        if let Some(TxError::Rejected(r)) = &sim.err {
            assert_eq!(format, TxFormat::V1, "v0 refused: {r}");
            formats.push((format, false));
            continue;
        }
        assert!(sim.ok(), "{sim:?}");
        assert!(sim.units_consumed > 0 && !sim.logs.is_empty());
        // The bank it ran on (AIR-18 dates a refusal by it).
        assert!(sim.slot > 0, "{sim:?}");
        let budget = ComputeBudget {
            unit_limit: tx::cu_limit(sim.units_consumed),
            unit_price: price,
        };
        let t = tx::build(format, &payer, &[], &ixs, &budget, bh, &[]).unwrap();
        let out = chain.send(&t).await.unwrap();
        assert!(out.ok(), "{out:?}");
        assert!(out.fee >= 5_000, "{out:?}");
        // The block it landed in.
        assert!(out.slot >= sim.slot, "{out:?}");
        expected += lamports;
        formats.push((format, true));
    }
    println!("formats accepted by this cluster: {formats:?}");
    assert!(formats.contains(&(TxFormat::V0, true)));
    let acc = chain.accounts(&[to]).await.unwrap();
    assert_eq!(acc[0].as_ref().map(|a| a.lamports), Some(expected));
    let none = chain
        .program_accounts(
            &Pubkey::new_unique(),
            &[Memcmp {
                offset: 0,
                bytes: vec![1, 2, 3],
            }],
        )
        .await
        .unwrap();
    assert!(none.is_empty());
    chain.recent_priority_fees(&[to]).await.unwrap();

    // Account headers (`dataSlice`): the first bytes of each account, as a whole read has them.
    let clock_sysvar: Pubkey = "SysvarC1ock11111111111111111111111111111111"
        .parse()
        .unwrap();
    let missing = Keypair::new().pubkey();
    let keys = [clock_sysvar, to, missing];
    let full = chain.accounts(&keys).await.unwrap();
    let heads = chain.account_headers(&keys, 12).await.unwrap();
    let whole = full[0].as_ref().unwrap();
    let head = heads[0].as_ref().unwrap();
    assert_eq!(head.data.len(), 12);
    assert!(whole.data.len() > 12);
    assert_eq!(head.owner, whole.owner);
    // The Clock's slot (the first 8 bytes) may have moved between the two reads; the owner and
    // the empty data of a System account may not.
    assert_eq!(heads[1].as_ref().map(|a| a.data.len()), Some(0));
    assert_eq!(
        heads[1].as_ref().map(|a| a.owner),
        full[1].as_ref().map(|a| a.owner)
    );
    assert!(heads[2].is_none());
    assert!(chain.finalized_slot().await.unwrap() <= chain.clock().await.unwrap().slot);

    // AIR-18: a header read as of a slot the node has reached is answered; as of one it has not,
    // refused rather than answered from the older state.
    let now = chain.clock().await.unwrap().slot;
    let heads = chain.account_headers_at(&keys, 12, now).await.unwrap();
    assert_eq!(heads[1].as_ref().map(|a| a.data.len()), Some(0));
    assert!(
        chain
            .account_headers_at(&keys, 12, now + 1_000_000)
            .await
            .is_err()
    );
}
