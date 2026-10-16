//! `shortwin-gov`: the Squads v4 governance tool against a JSON-RPC cluster (`--rpc`; the
//! commands are `shortwin_ops::cli`'s, `--help` lists them).
//!
//! ```text
//! shortwin-gov verify --config governance.toml --rpc https://api.mainnet-beta.solana.com
//! shortwin-gov stage-authorities --config governance.toml --rpc <url> --key member.json --dry-run
//! ```
//!
//! Exit status: 0 on success, 1 when `verify` finds a failed check, 2 on an error.

use std::time::Duration;

use shortwin_services_common::rpc::RpcChain;

/// How long a sent governance transaction is awaited by default (`--confirm-timeout`).
const CONFIRM_TIMEOUT_S: u64 = 90;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() || argv.iter().any(|a| a == "-h" || a == "--help") {
        println!("{}", shortwin_ops::cli::USAGE);
        std::process::exit(if argv.is_empty() { 2 } else { 0 });
    }
    let code = match go(&argv).await {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            eprintln!("error: {e:#}");
            2
        }
    };
    std::process::exit(code);
}

async fn go(argv: &[String]) -> anyhow::Result<bool> {
    let args = shortwin_ops::cli::parse(argv)?;
    let url = args.rpc.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "--rpc <url> is required (no default cluster)\n{}",
            shortwin_ops::cli::USAGE
        )
    })?;
    // Past a blockhash's lifetime (150 blocks, about 60 to 80 s): a transaction still not
    // confirmed then has expired and cannot land later.
    let timeout = Duration::from_secs(args.confirm_timeout.unwrap_or(CONFIRM_TIMEOUT_S));
    let chain = RpcChain::new(url).with_confirm_timeout(timeout);
    let mut out = std::io::stdout();
    shortwin_ops::run(&chain, argv, &mut out).await
}
