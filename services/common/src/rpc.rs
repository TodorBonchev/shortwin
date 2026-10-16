//! The JSON-RPC backend of [`Chain`] (a real cluster; ARCHITECTURE §7.1 "RPC websocket
//! fallback" — the Yellowstone account stream of §7.1 "Inputs" is a later milestone, so v0
//! polls `getMultipleAccounts` / `getProgramAccounts` each tick).
//!
//! - Reads use the configured commitment (default `confirmed`).
//! - `simulate` asks for inner instructions (event decoding) and replaces nothing: the keeper
//!   simulates the exact transaction it will send (signatures are not verified).
//! - `send` submits with `skipPreflight` (the keeper simulated already), then polls the
//!   signature until it is confirmed, fails, or `confirm_timeout` passes, and fetches the landed
//!   transaction's meta (logs, fee, compute units, inner instructions).
//! - A cluster that refuses a transaction before running it (e.g. a v1 transaction on a
//!   cluster without SIMD-0385, an oversized transaction) is reported as
//!   [`TxError::Rejected`], which the keeper's v1 → v0 fallback keys on.
//! - `send_bundle` (U22; no keeper duty sends one since the straddle bundle's retirement,
//!   pump-final §7) goes to a **Jito block engine** when one is configured
//!   ([`RpcChain::with_block_engine`]): `sendBundle` with the base64 transactions
//!   (docs.jito.wtf, "sendBundle": `POST <engine>/api/v1/bundles`, params `[[tx…], {"encoding":
//!   "base64"}]`, result the bundle id). Whether it landed is read from the **cluster**, not the
//!   engine: every transaction's signature confirmed (a bundle is all-or-nothing) or, at the
//!   confirmation timeout, none — so the engine's status endpoints are not relied on.
//!   **UNVERIFIED** against a live block engine (T-STRADDLE ran the bundle in-process until the
//!   straddle's retirement).

use std::time::Duration;

use anyhow::{Context, anyhow};
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_commitment_config::CommitmentConfig;
use solana_hash::Hash;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::client_error::{Error as ClientError, ErrorKind};
use solana_rpc_client_api::config::{
    RpcAccountInfoConfig, RpcProgramAccountsConfig, RpcSendTransactionConfig,
    RpcSimulateTransactionConfig, RpcTransactionConfig,
};
use solana_rpc_client_api::filter::{Memcmp as RpcMemcmp, RpcFilterType};
use solana_rpc_client_api::request::{RpcError, RpcRequest};
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_status_client_types::{
    UiInnerInstructions, UiInstruction, UiTransactionEncoding,
};

use crate::chain::{
    AccountData, BundleOutcome, Chain, ChainClock, InnerIx, Memcmp, TxError, TxOutcome,
};

/// Largest `getMultipleAccounts` request.
const MULTIPLE_ACCOUNTS_MAX: usize = 100;

/// `getMultipleAccounts` requests of one read in flight at once (M7-SEC-8: a read of `n` keys
/// takes ⌈n/100⌉ requests; awaited one after another they cost ⌈n/100⌉ round trips, in parallel
/// about one). Results keep the keys' order.
pub const MULTIPLE_ACCOUNTS_CONCURRENCY: usize = 8;

/// The path of a Jito block engine's `sendBundle` endpoint (docs.jito.wtf).
pub const JITO_BUNDLES_PATH: &str = "/api/v1/bundles";

/// Most transactions one Jito bundle holds (U22).
pub const JITO_BUNDLE_MAX_TXS: usize = 5;

/// A JSON-RPC cluster (module docs).
pub struct RpcChain {
    client: RpcClient,
    commitment: CommitmentConfig,
    confirm_timeout: Duration,
    poll_interval: Duration,
    /// The Jito block engine bundles go to (`None`: no bundles).
    block_engine: Option<RpcClient>,
}

/// A block engine URL with the `sendBundle` path: a bare engine URL (no `/api/` in it) gets
/// [`JITO_BUNDLES_PATH`]; a full endpoint is kept.
pub fn bundle_endpoint(url: &str) -> String {
    if url.contains("/api/") {
        url.to_string()
    } else {
        format!("{}{JITO_BUNDLES_PATH}", url.trim_end_matches('/'))
    }
}

/// `sendBundle`'s params: `[[base64(tx), …], {"encoding": "base64"}]` (the wire bytes of each
/// transaction, in bundle order).
pub fn send_bundle_params(txs: &[VersionedTransaction]) -> anyhow::Result<serde_json::Value> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let encoded = txs
        .iter()
        .map(|t| {
            wincode::serialize(t)
                .map(|b| engine.encode(b))
                .map_err(|e| anyhow!("serializing a bundle transaction: {e:?}"))
        })
        .collect::<anyhow::Result<Vec<String>>>()?;
    Ok(serde_json::json!([encoded, { "encoding": "base64" }]))
}

impl RpcChain {
    /// A client for `url` at `confirmed` commitment, waiting up to 8 s for a confirmation.
    pub fn new(url: impl Into<String>) -> Self {
        let commitment = CommitmentConfig::confirmed();
        Self {
            client: RpcClient::new_with_commitment(url.into(), commitment),
            commitment,
            // About 20 slots: a keeper transaction that has not landed by then is retried
            // with an escalated priority price next tick rather than waited for.
            confirm_timeout: Duration::from_secs(8),
            poll_interval: Duration::from_millis(400),
            block_engine: None,
        }
    }

    /// Sends bundles to the Jito block engine at `url` (a bare engine URL such as
    /// `https://mainnet.block-engine.jito.wtf`, or its full `sendBundle` endpoint).
    pub fn with_block_engine(mut self, url: &str) -> Self {
        self.block_engine = Some(RpcClient::new_with_commitment(
            bundle_endpoint(url),
            self.commitment,
        ));
        self
    }

    /// Whether bundles can be sent.
    pub fn has_block_engine(&self) -> bool {
        self.block_engine.is_some()
    }

    /// Overrides the confirmation timeout.
    pub fn with_confirm_timeout(mut self, timeout: Duration) -> Self {
        self.confirm_timeout = timeout;
        self
    }

    /// The underlying client.
    pub fn client(&self) -> &RpcClient {
        &self.client
    }

    /// The latest finalized slot: a slot every bank's `SlotHashes` still holds for the next
    /// ~512 slots (the `recent_slot` of an address-lookup-table creation).
    pub async fn finalized_slot(&self) -> anyhow::Result<u64> {
        self.client
            .get_slot_with_commitment(CommitmentConfig::finalized())
            .await
            .context("getSlot (finalized)")
    }

    fn account_of(ui: solana_account_decoder_client_types::UiAccount) -> Option<AccountData> {
        let data = ui.data.decode()?;
        let owner = ui.owner.parse().ok()?;
        Some(AccountData {
            lamports: ui.lamports,
            owner,
            executable: ui.executable,
            data,
        })
    }

    /// `getProgramAccounts` of `program` with a `dataSize` filter (if any) and `filters`.
    async fn get_program_accounts(
        &self,
        program: &Pubkey,
        data_size: Option<usize>,
        filters: &[Memcmp],
    ) -> anyhow::Result<Vec<(Pubkey, AccountData)>> {
        let mut rpc_filters: Vec<RpcFilterType> = data_size
            .map(|n| RpcFilterType::DataSize(n as u64))
            .into_iter()
            .collect();
        rpc_filters.extend(
            filters
                .iter()
                .map(|f| RpcFilterType::Memcmp(RpcMemcmp::new_base58_encoded(f.offset, &f.bytes))),
        );
        let config = RpcProgramAccountsConfig {
            filters: Some(rpc_filters),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(self.commitment),
                ..RpcAccountInfoConfig::default()
            },
            with_context: None,
            sort_results: None,
        };
        let res = self
            .client
            .get_program_ui_accounts_with_config(program, config)
            .await
            .context("getProgramAccounts")?;
        Ok(res
            .into_iter()
            .filter_map(|(k, a)| Self::account_of(a).map(|a| (k, a)))
            .collect())
    }
}

/// `true` if an RPC error means the cluster refused the transaction before running it.
fn is_rejection(e: &ClientError) -> Option<String> {
    match e.kind() {
        ErrorKind::RpcError(RpcError::RpcResponseError { code, message, .. }) => {
            // -32602 invalid params (unsupported version, too large, undecodable),
            // -32003 signature verification, -32013 transaction length.
            matches!(code, -32602 | -32003 | -32013).then(|| message.clone())
        }
        _ => None,
    }
}

fn inner_of(
    inner: Option<Vec<UiInnerInstructions>>,
    keys: &[Pubkey],
    n_top: usize,
) -> Vec<Vec<InnerIx>> {
    let mut out = vec![Vec::new(); n_top];
    for group in inner.unwrap_or_default() {
        let Some(slot) = out.get_mut(usize::from(group.index)) else {
            continue;
        };
        for ix in group.instructions {
            if let UiInstruction::Compiled(c) = ix {
                let Some(program_id) = keys.get(usize::from(c.program_id_index)).copied() else {
                    continue;
                };
                let Ok(data) = bs58::decode(&c.data).into_vec() else {
                    continue;
                };
                slot.push(InnerIx { program_id, data });
            }
        }
    }
    out
}

impl Chain for RpcChain {
    async fn clock(&self) -> anyhow::Result<ChainClock> {
        let slot = self
            .client
            .get_slot_with_commitment(self.commitment)
            .await
            .context("getSlot")?;
        let clock = self
            .client
            .get_account_with_commitment(&solana_sdk_ids_clock(), self.commitment)
            .await
            .context("Clock sysvar")?
            .value
            .context("no Clock sysvar")?;
        // Clock sysvar: slot u64, epoch_start_timestamp i64, epoch u64, leader_schedule_epoch
        // u64, unix_timestamp i64.
        let ts = clock
            .data
            .get(32..40)
            .map(|b| i64::from_le_bytes(b.try_into().unwrap_or([0; 8])))
            .context("short Clock sysvar")?;
        Ok(ChainClock {
            slot,
            unix_timestamp: ts,
        })
    }

    async fn accounts(&self, keys: &[Pubkey]) -> anyhow::Result<Vec<Option<AccountData>>> {
        use futures::stream::{self, StreamExt, TryStreamExt};
        // M7-SEC-8: the chunks in parallel (at most `MULTIPLE_ACCOUNTS_CONCURRENCY`), in order.
        let chunks: Vec<Vec<Option<AccountData>>> =
            stream::iter(keys.chunks(MULTIPLE_ACCOUNTS_MAX))
                .map(|chunk| async move {
                    let res = self
                        .client
                        .get_multiple_accounts_with_commitment(chunk, self.commitment)
                        .await
                        .context("getMultipleAccounts")?;
                    anyhow::Ok(
                        res.value
                            .into_iter()
                            .map(|a| {
                                a.map(|a| AccountData {
                                    lamports: a.lamports,
                                    owner: a.owner,
                                    executable: a.executable,
                                    data: a.data,
                                })
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .buffered(MULTIPLE_ACCOUNTS_CONCURRENCY)
                .try_collect()
                .await?;
        Ok(chunks.into_iter().flatten().collect())
    }

    async fn account_headers(
        &self,
        keys: &[Pubkey],
        len: usize,
    ) -> anyhow::Result<Vec<Option<AccountData>>> {
        self.headers(keys, len, None).await
    }

    async fn account_headers_at(
        &self,
        keys: &[Pubkey],
        len: usize,
        min_context_slot: u64,
    ) -> anyhow::Result<Vec<Option<AccountData>>> {
        self.headers(keys, len, Some(min_context_slot)).await
    }

    async fn program_accounts(
        &self,
        program: &Pubkey,
        filters: &[Memcmp],
    ) -> anyhow::Result<Vec<(Pubkey, AccountData)>> {
        self.get_program_accounts(program, None, filters).await
    }

    async fn program_accounts_sized(
        &self,
        program: &Pubkey,
        data_size: usize,
        filters: &[Memcmp],
    ) -> anyhow::Result<Vec<(Pubkey, AccountData)>> {
        let mut out = self
            .get_program_accounts(program, Some(data_size), filters)
            .await?;
        // A node that ignored the filter must not hand the caller another layout.
        out.retain(|(_, a)| a.data.len() == data_size);
        Ok(out)
    }

    async fn latest_blockhash(&self) -> anyhow::Result<Hash> {
        self.client
            .get_latest_blockhash()
            .await
            .context("getLatestBlockhash")
    }

    async fn simulate(&self, tx: &VersionedTransaction) -> anyhow::Result<TxOutcome> {
        let config = RpcSimulateTransactionConfig {
            sig_verify: false,
            replace_recent_blockhash: false,
            commitment: Some(self.commitment),
            encoding: Some(UiTransactionEncoding::Base64),
            accounts: None,
            min_context_slot: None,
            inner_instructions: true,
        };
        let (slot, res) = match self
            .client
            .simulate_transaction_with_config(tx, config)
            .await
        {
            Ok(r) => (r.context.slot, r.value),
            Err(e) => {
                if let Some(msg) = is_rejection(&e) {
                    return Ok(TxOutcome {
                        err: Some(TxError::Rejected(msg)),
                        ..TxOutcome::default()
                    });
                }
                return Err(anyhow!(e).context("simulateTransaction"));
            }
        };
        let keys = tx.message.static_account_keys().to_vec();
        let n_top = tx.message.instructions().len();
        Ok(TxOutcome {
            signature: Default::default(),
            err: res.err.map(|e| TxError::from_transaction_error(e.into())),
            logs: res.logs.unwrap_or_default(),
            units_consumed: res.units_consumed.unwrap_or(0),
            fee: res.fee.unwrap_or(0),
            inner: inner_of(res.inner_instructions, &keys, n_top),
            slot,
        })
    }

    async fn send(&self, tx: &VersionedTransaction) -> anyhow::Result<TxOutcome> {
        let config = RpcSendTransactionConfig {
            skip_preflight: true,
            preflight_commitment: Some(self.commitment.commitment),
            encoding: Some(UiTransactionEncoding::Base64),
            max_retries: Some(0),
            min_context_slot: None,
        };
        let signature = match self.client.send_transaction_with_config(tx, config).await {
            Ok(s) => s,
            Err(e) => {
                if let Some(msg) = is_rejection(&e) {
                    return Ok(TxOutcome {
                        err: Some(TxError::Rejected(msg)),
                        ..TxOutcome::default()
                    });
                }
                return Err(anyhow!(e).context("sendTransaction"));
            }
        };
        let deadline = tokio::time::Instant::now() + self.confirm_timeout;
        let mut out = self.await_landed(&[(signature, tx)], deadline).await?;
        Ok(out.pop().unwrap_or_default())
    }

    async fn recent_priority_fees(&self, writable: &[Pubkey]) -> anyhow::Result<Vec<u64>> {
        let fees = self
            .client
            .get_recent_prioritization_fees(writable)
            .await
            .context("getRecentPrioritizationFees")?;
        Ok(fees.into_iter().map(|f| f.prioritization_fee).collect())
    }

    async fn send_bundle(&self, txs: &[VersionedTransaction]) -> anyhow::Result<BundleOutcome> {
        let Some(engine) = &self.block_engine else {
            anyhow::bail!("no block engine configured (--block-engine): bundles cannot be sent");
        };
        if txs.is_empty() || txs.len() > JITO_BUNDLE_MAX_TXS {
            anyhow::bail!(
                "a bundle holds 1 to {JITO_BUNDLE_MAX_TXS} transactions, not {}",
                txs.len()
            );
        }
        let params = send_bundle_params(txs)?;
        let id: String = match engine
            .send(
                RpcRequest::Custom {
                    method: "sendBundle",
                },
                params,
            )
            .await
        {
            Ok(id) => id,
            Err(e) => {
                return Ok(BundleOutcome {
                    detail: format!("sendBundle refused: {e}"),
                    ..BundleOutcome::default()
                });
            }
        };
        let deadline = tokio::time::Instant::now() + self.confirm_timeout;
        let pairs: Vec<(solana_signature::Signature, &VersionedTransaction)> = txs
            .iter()
            .map(|t| (t.signatures.first().copied().unwrap_or_default(), t))
            .collect();
        let outs = self.await_landed(&pairs, deadline).await?;
        let landed = outs.iter().all(TxOutcome::ok);
        let detail = if landed {
            String::new()
        } else {
            outs.iter()
                .enumerate()
                .find_map(|(i, o)| o.err.as_ref().map(|e| format!("tx{}: {e:?}", i + 1)))
                .unwrap_or_default()
        };
        Ok(BundleOutcome {
            id,
            landed,
            txs: if landed { outs } else { Vec::new() },
            detail,
        })
    }
}

impl RpcChain {
    /// `getMultipleAccounts` of `keys` sliced to their first `len` bytes (`dataSlice`), at the
    /// chain's commitment and, with `min_context_slot`, from a node that has reached that slot
    /// (one behind refuses: an error).
    async fn headers(
        &self,
        keys: &[Pubkey],
        len: usize,
        min_context_slot: Option<u64>,
    ) -> anyhow::Result<Vec<Option<AccountData>>> {
        let config = RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            data_slice: Some(solana_account_decoder_client_types::UiDataSliceConfig {
                offset: 0,
                length: len,
            }),
            commitment: Some(self.commitment),
            min_context_slot,
        };
        let mut out = Vec::with_capacity(keys.len());
        for chunk in keys.chunks(MULTIPLE_ACCOUNTS_MAX) {
            let res = self
                .client
                .get_multiple_ui_accounts_with_config(chunk, config.clone())
                .await
                .context("getMultipleAccounts (dataSlice)")?;
            out.extend(res.value.into_iter().map(|a| a.and_then(Self::account_of)));
        }
        Ok(out)
    }

    /// Polls the signatures of `txs` until every one is confirmed at the chain's commitment or
    /// `deadline` passes (the ones still missing are `Dropped`), then fetches each confirmed
    /// transaction's meta (logs, fee, compute units, inner instructions).
    async fn await_landed(
        &self,
        txs: &[(solana_signature::Signature, &VersionedTransaction)],
        deadline: tokio::time::Instant,
    ) -> anyhow::Result<Vec<TxOutcome>> {
        let sigs: Vec<solana_signature::Signature> = txs.iter().map(|(s, _)| *s).collect();
        let mut statuses = vec![None; sigs.len()];
        loop {
            let st = self
                .client
                .get_signature_statuses(&sigs)
                .await
                .context("getSignatureStatuses")?;
            for (slot, s) in statuses.iter_mut().zip(st.value) {
                if let Some(s) = s
                    && s.satisfies_commitment(self.commitment)
                {
                    *slot = Some(s);
                }
            }
            if statuses.iter().all(Option::is_some) || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(self.poll_interval).await;
        }
        let mut out = Vec::with_capacity(txs.len());
        for ((signature, tx), status) in txs.iter().zip(statuses) {
            let Some(status) = status else {
                out.push(TxOutcome {
                    signature: *signature,
                    err: Some(TxError::Dropped("not confirmed before the timeout".into())),
                    ..TxOutcome::default()
                });
                continue;
            };
            let mut outcome = TxOutcome {
                signature: *signature,
                err: status.err.map(TxError::from_transaction_error),
                slot: status.slot,
                ..TxOutcome::default()
            };
            let cfg = RpcTransactionConfig {
                encoding: Some(UiTransactionEncoding::Base64),
                commitment: Some(self.commitment),
                max_supported_transaction_version: Some(1),
            };
            if let Ok(landed) = self
                .client
                .get_transaction_with_config(signature, cfg)
                .await
                && let Some(meta) = landed.transaction.meta
            {
                let keys = tx.message.static_account_keys().to_vec();
                let n_top = tx.message.instructions().len();
                outcome.fee = meta.fee;
                outcome.logs = Option::<Vec<String>>::from(meta.log_messages).unwrap_or_default();
                outcome.units_consumed =
                    Option::<u64>::from(meta.compute_units_consumed).unwrap_or(0);
                outcome.inner = inner_of(
                    Option::<Vec<UiInnerInstructions>>::from(meta.inner_instructions),
                    &keys,
                    n_top,
                );
            }
            out.push(outcome);
        }
        Ok(out)
    }
}

/// The Clock sysvar's address.
fn solana_sdk_ids_clock() -> Pubkey {
    solana_pubkey::pubkey!("SysvarC1ock11111111111111111111111111111111")
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_transaction_status_client_types::UiCompiledInstruction;

    #[test]
    fn inner_instructions_resolve_programs_and_decode_data() {
        let keys = [Pubkey::new_unique(), Pubkey::new_unique()];
        let ui = vec![UiInnerInstructions {
            index: 1,
            instructions: vec![UiInstruction::Compiled(UiCompiledInstruction {
                program_id_index: 1,
                accounts: vec![0],
                data: bs58::encode([1u8, 2, 3]).into_string(),
                stack_height: Some(2),
            })],
        }];
        let inner = inner_of(Some(ui), &keys, 3);
        assert_eq!(inner.len(), 3);
        assert!(inner[0].is_empty() && inner[2].is_empty());
        assert_eq!(
            inner[1],
            vec![InnerIx {
                program_id: keys[1],
                data: vec![1, 2, 3]
            }]
        );
        assert_eq!(inner_of(None, &keys, 1), vec![Vec::new()]);
    }

    /// `sendBundle`'s params are `[[base64(wire bytes)…], {"encoding": "base64"}]`, in bundle
    /// order, and a bare engine URL gets the bundles path.
    #[test]
    fn send_bundle_params_are_jitos() {
        use base64::Engine;
        use solana_hash::Hash;
        use solana_keypair::Keypair;
        use solana_signer::Signer;
        let kp = Keypair::new();
        let mk = |lamports: u64| {
            let ix = solana_instruction::Instruction {
                program_id: Pubkey::new_unique(),
                accounts: vec![solana_instruction::AccountMeta::new(kp.pubkey(), true)],
                data: lamports.to_le_bytes().to_vec(),
            };
            crate::tx::build(
                crate::tx::TxFormat::V0,
                &kp,
                &[],
                &[ix],
                &crate::tx::ComputeBudget::SIMULATION,
                Hash::default(),
                &[],
            )
            .unwrap()
        };
        let txs = [mk(1), mk(2)];
        let p = send_bundle_params(&txs).unwrap();
        let arr = p.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1], serde_json::json!({ "encoding": "base64" }));
        let list = arr[0].as_array().unwrap();
        assert_eq!(list.len(), 2);
        for (enc, tx) in list.iter().zip(&txs) {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(enc.as_str().unwrap())
                .unwrap();
            assert_eq!(bytes.len(), crate::tx::size(tx));
            assert_eq!(bytes, wincode::serialize(tx).unwrap());
        }
        assert_eq!(
            bundle_endpoint("https://mainnet.block-engine.jito.wtf/"),
            "https://mainnet.block-engine.jito.wtf/api/v1/bundles"
        );
        assert_eq!(
            bundle_endpoint("https://ny.mainnet.block-engine.jito.wtf/api/v1/bundles"),
            "https://ny.mainnet.block-engine.jito.wtf/api/v1/bundles"
        );
    }
}
