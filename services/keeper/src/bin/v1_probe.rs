//! `shortwin-v1-probe`: does a cluster take SIMD-0385 **v1** transactions (ARCH §12 U25; M7 K6)?
//!
//! ```text
//! shortwin-v1-probe --rpc <url> [--keypair <payer>] [--send] [--priority-price <µlamports/CU>]
//! ```
//!
//! Builds, with the keeper's own builder (`shortwin_services_common::tx`), a **v1** and a **v0**
//! transaction holding one zero-lamport System transfer from the payer to itself (it moves nothing;
//! the fee is the only cost), the compute budget set as the keeper sets it, and:
//!
//! - simulates both (`simulateTransaction`, signature check off): a node that cannot decode v1
//!   answers `-32602` (the keeper's `TxError::Rejected`, the trigger of its v1 → v0 fallback);
//! - with `--send` and a funded `--keypair`, also **sends** both (`sendTransaction`, preflight off)
//!   and waits for them to land — the only evidence that leaders take v1 (a node's simulation says
//!   nothing about the leaders').
//!
//! Prints one line per transaction and exits 0 when v1 was accepted (landed with `--send`,
//! otherwise simulated without a rejection), 1 when it was rejected, 2 on a transport error.
//! Without `--keypair` a throwaway key signs: the simulation then fails for the missing fee payer
//! (`AccountNotFound`), which still shows whether the node decodes v1. Cost with `--send`: two
//! transactions' fees (10,000 lamports at no priority price).

use std::process::ExitCode;

use anyhow::{Context, bail};
use shortwin_services_common::rpc::RpcChain;
use shortwin_services_common::tx::{self, ComputeBudget, TxFormat};
use shortwin_services_common::{Chain, TxError, TxOutcome};
use solana_keypair::{Keypair, read_keypair_file};
use solana_pubkey::Pubkey;
use solana_signer::Signer;

const USAGE: &str = "usage: shortwin-v1-probe --rpc <url> [--keypair <payer>] [--send] \
     [--priority-price <µlamports/CU>]";

struct Args {
    rpc: String,
    keypair: Option<String>,
    send: bool,
    price: u64,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut rpc = None;
    let mut keypair = None;
    let mut send = false;
    let mut price = 0;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || {
            it.next()
                .with_context(|| format!("{flag} needs a value\n{USAGE}"))
        };
        match flag.as_str() {
            "--rpc" => rpc = Some(val()?),
            "--keypair" => keypair = Some(val()?),
            "--priority-price" => price = val()?.parse().context("--priority-price")?,
            "--send" => send = true,
            "-h" | "--help" => bail!("{USAGE}"),
            other => bail!("unknown argument {other}\n{USAGE}"),
        }
    }
    let rpc = rpc.with_context(|| format!("--rpc is required\n{USAGE}"))?;
    if send && keypair.is_none() {
        bail!("--send needs a funded --keypair\n{USAGE}");
    }
    Ok(Args {
        rpc,
        keypair,
        send,
        price,
    })
}

/// A System `transfer` of `lamports` from `from` to `to` (instruction 2).
fn transfer(
    from: &Pubkey,
    to: &Pubkey,
    lamports: u64,
) -> anchor_lang::solana_program::instruction::Instruction {
    use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
    let mut data = 2u32.to_le_bytes().to_vec();
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: anchor_lang::system_program::ID,
        accounts: vec![AccountMeta::new(*from, true), AccountMeta::new(*to, false)],
        data,
    }
}

fn describe(out: &TxOutcome) -> String {
    match &out.err {
        None => format!("ok ({} CU, fee {} lamports)", out.units_consumed, out.fee),
        Some(TxError::Rejected(m)) => format!("REJECTED before execution: {m}"),
        Some(e) => format!("decoded; failed in execution: {e:?}"),
    }
}

async fn run(a: &Args) -> anyhow::Result<bool> {
    let chain = RpcChain::new(a.rpc.clone());
    let payer = match &a.keypair {
        Some(p) => read_keypair_file(p).map_err(|e| anyhow::anyhow!("reading {p}: {e}"))?,
        None => Keypair::new(),
    };
    let me = payer.pubkey();
    let ixs = [transfer(&me, &me, 0)];
    let mut v1_ok = false;
    for format in [TxFormat::V1, TxFormat::V0] {
        let bh = chain.latest_blockhash().await?;
        let sim_budget = ComputeBudget {
            unit_price: a.price,
            ..ComputeBudget::SIMULATION
        };
        let t = tx::build(format, &payer, &[], &ixs, &sim_budget, bh, &[])?;
        let sim = chain.simulate(&t).await?;
        let decoded = !matches!(sim.err, Some(TxError::Rejected(_)));
        println!(
            "{format:?} simulate: {} B — {}",
            tx::size(&t),
            describe(&sim)
        );
        let mut accepted = decoded;
        if a.send {
            let budget = ComputeBudget {
                unit_limit: tx::cu_limit(sim.units_consumed.max(450)),
                unit_price: a.price,
            };
            let t = tx::build(format, &payer, &[], &ixs, &budget, bh, &[])?;
            let out = chain.send(&t).await?;
            println!("{format:?} send:     {}", describe(&out));
            accepted = out.err.is_none();
        }
        if format == TxFormat::V1 {
            v1_ok = accepted;
        }
    }
    println!(
        "v1 {} by {} ({})",
        if v1_ok { "ACCEPTED" } else { "REFUSED" },
        a.rpc,
        if a.send {
            "landed: leaders take v1"
        } else {
            "simulation only: says nothing about the leaders; rerun with --send"
        }
    );
    Ok(v1_ok)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let a = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e:#}");
            return ExitCode::from(2);
        }
    };
    match run(&a).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("{e:#}");
            ExitCode::from(2)
        }
    }
}
