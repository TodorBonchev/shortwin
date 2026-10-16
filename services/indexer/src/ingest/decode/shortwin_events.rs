//! short.win events and instruction arguments (INDEXER.md §5.3, §5.4). Owner: INGEST. (The
//! retired straddle helper `complete_curve`'s two events are no longer decoded: design
//! pump-final §9, owner decision D2.)
//!
//! Every payload is checked twice: with the program's own type (`shortwin::events::*`,
//! `shortwin::state::ConfigChange`) and with the IDL walker
//! ([`super::idl`]), which renders the `events.data` / `invocations.args` JSON (§4.2); both must
//! read the same number of bytes. A payload either check refuses (or a short one) is quarantined
//! (§3.6): a short.win event the indexer cannot read means the program and the indexer disagree
//! on a layout. A short.win event **longer** than the layout (a program upgrade that appended
//! fields, as M2 did to `Close` / `Liquidate` / `Expire`) is decoded on its known prefix and
//! applied; [`EventPayload::trailing`] counts the bytes left, and the decoder records the payload
//! in `ingest_quarantine` with `applied = TRUE` (an alert; M8-SEC-13).
//!
//! `propose_config`'s `ConfigChange` is re-encoded with the program's own `encode` and hashed
//! with its `change_hash` (`sha256(kind ‖ payload)`); [`config_change`] reports whether that
//! equals `ConfigProposed.hash`.

use anchor_lang::AnchorDeserialize;
use anyhow::{Context, anyhow, bail};
use serde_json::Value;
use solana_pubkey::Pubkey;

use super::idl;
use crate::model::{EventSpec, SHORTWIN_EVENTS};
use crate::numeric::hex;

/// A decoded event payload.
#[derive(Clone, Debug, PartialEq)]
pub struct EventPayload {
    /// The event's Rust name (`events.kind`).
    pub kind: &'static str,
    /// Every field (§4.2 JSON).
    pub data: Value,
    /// Bytes after the known layout (a newer program version appended fields): the known
    /// prefix was decoded. 0 for an exact payload.
    pub trailing: usize,
}

/// The short.win event whose discriminator prefixes `payload` (discriminator ‖ Borsh).
pub fn shortwin_spec(payload: &[u8]) -> Option<&'static EventSpec> {
    SHORTWIN_EVENTS
        .iter()
        .find(|s| payload.starts_with(s.discriminator))
}

/// Reads `body` with the program's own type of event `kind`; returns the bytes its layout
/// takes (a longer body has trailing bytes, a shorter one fails).
fn check_program_layout(kind: &str, body: &[u8]) -> anyhow::Result<usize> {
    use shortwin::events as e;
    macro_rules! check {
        ($($t:ident),* $(,)?) => {
            match kind {
                $(stringify!($t) => {
                    let mut rest = body;
                    e::$t::deserialize(&mut rest)
                        .map(|_| body.len() - rest.len())
                        .map_err(|err| anyhow!("{kind}: {err}"))
                })*
                other => Err(anyhow!("{other}: not a short.win event")),
            }
        };
    }
    check!(
        MarketCreated,
        Deposit,
        Withdraw,
        ClaimSol,
        DeficitCovered,
        LenderSynced,
        Open,
        Close,
        Liquidate,
        Expire,
        SolSettle,
        SettleArmed,
        SettleDisarmed,
        EpochRolled,
        ProtocolFeesSwept,
        MarginAdded,
        PriceCranked,
        VenueStateChanged,
        RouteChainChanged,
        PoolProposed,
        PoolSwitched,
        PoolSwitchCancelled,
        TraderSwept,
        VenueRewardsClaimed,
        MintSafetyTripped,
        VenueUpgradeFlagged,
        VenueRevalidated,
        CoverageGap,
        CoveragePauseSet,
        ConfigProposed,
        ConfigApplied,
        ConfigCancelled,
        PauseSet,
        VenueDisabled,
        RouteBrokenSet,
        VenueExtrasSet,
    )
}

/// Decodes a short.win event payload (discriminator ‖ Borsh): exactly, or on its known prefix
/// when the payload is longer (module docs; `trailing` > 0).
pub fn decode_shortwin_event(payload: &[u8]) -> anyhow::Result<EventPayload> {
    let spec = shortwin_spec(payload).context("unknown short.win event discriminator")?;
    let body = &payload[spec.discriminator.len()..];
    let used = check_program_layout(spec.name, body)?;
    let mut rest = body;
    let data = idl::idl()
        .decode_struct(spec.name, &mut rest)
        .with_context(|| format!("{}: the IDL layout", spec.name))?;
    let idl_used = body.len() - rest.len();
    if idl_used != used {
        bail!(
            "{}: the IDL reads {idl_used} bytes, the program's type {used}",
            spec.name
        );
    }
    Ok(EventPayload {
        kind: spec.name,
        data,
        trailing: body.len() - used,
    })
}

/// The market a short.win event payload names (every event's first field), when the payload
/// is long enough; `None` for `Pubkey::default()`.
pub fn payload_market(payload: &[u8]) -> Option<Pubkey> {
    let bytes: [u8; 32] = payload.get(8..40)?.try_into().ok()?;
    let k = Pubkey::new_from_array(bytes);
    (k != Pubkey::default()).then_some(k)
}

// ---- field access on the §4.2 JSON ---------------------------------------------------------

/// A key field (`null` = `Pubkey::default()`).
pub fn key_field(data: &Value, name: &str) -> Option<Pubkey> {
    data.get(name)?.as_str()?.parse().ok()
}

/// A u64 field (decimal string or number).
pub fn u64_field(data: &Value, name: &str) -> u64 {
    match data.get(name) {
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(v) => v.as_u64().unwrap_or(0),
        None => 0,
    }
}

/// An i64 field (decimal string or number).
pub fn i64_field(data: &Value, name: &str) -> i64 {
    match data.get(name) {
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(v) => v.as_i64().unwrap_or(0),
        None => 0,
    }
}

/// A u128 field (decimal string).
pub fn u128_field(data: &Value, name: &str) -> u128 {
    match data.get(name) {
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(v) => v.as_u64().map(u128::from).unwrap_or(0),
        None => 0,
    }
}

/// An i128 field (decimal string).
pub fn i128_field(data: &Value, name: &str) -> i128 {
    match data.get(name) {
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(v) => v.as_i64().map(i128::from).unwrap_or(0),
        None => 0,
    }
}

/// A small unsigned field (u8/u16/u32).
pub fn u32_field(data: &Value, name: &str) -> u32 {
    data.get(name)
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0)
}

/// A u8 field.
pub fn u8_field(data: &Value, name: &str) -> u8 {
    u8::try_from(u32_field(data, name)).unwrap_or(0)
}

/// A bool field.
pub fn bool_field(data: &Value, name: &str) -> bool {
    data.get(name).and_then(Value::as_bool).unwrap_or(false)
}

/// `events.position`: the event's `position` field.
pub fn position_of(data: &Value) -> Option<Pubkey> {
    key_field(data, "position")
}

/// `events.actor` (§5.2 rule 4): `owner` for the lender events, `borrower` for `Open`/`Close`,
/// `keeper` for `Liquidate`/`Expire`/`SolSettle`, `by` for `CoveragePauseSet`/`PauseSet`/
/// `ConfigCancelled`, the fee payer for `PriceCranked`, else none.
pub fn actor_of(kind: &str, data: &Value, fee_payer: &Pubkey) -> Option<Pubkey> {
    match kind {
        "Deposit" | "Withdraw" | "ClaimSol" | "DeficitCovered" | "LenderSynced" => {
            key_field(data, "owner")
        }
        "Open" | "Close" => key_field(data, "borrower"),
        "Liquidate" | "Expire" | "SolSettle" => key_field(data, "keeper"),
        "CoveragePauseSet" | "PauseSet" | "ConfigCancelled" => key_field(data, "by"),
        "PriceCranked" => Some(*fee_payer),
        _ => None,
    }
}

// ---- instructions --------------------------------------------------------------------------

/// A decoded short.win instruction.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedIx {
    /// snake_case name (`unknown` for an unknown discriminator).
    pub name: String,
    /// The arguments (§4.2 JSON); `None` without arguments or when they failed to decode.
    pub args: Option<Value>,
    /// Why the arguments failed to decode (quarantined with `ev_source = 2`).
    pub error: Option<String>,
    /// The `market` account, when the instruction names one (an optional account passed as
    /// the program id — Anchor's "none" — is no market).
    pub market: Option<Pubkey>,
}

/// Decodes a short.win instruction's data and resolves its `market` account.
pub fn decode_instruction(program: &Pubkey, data: &[u8], accounts: &[Pubkey]) -> DecodedIx {
    let idl = idl::idl();
    let Some(def) = idl.instruction_for(data) else {
        return DecodedIx {
            name: "unknown".into(),
            args: None,
            error: Some("unknown short.win instruction discriminator".into()),
            market: None,
        };
    };
    let market = def
        .account_index("market")
        .and_then(|i| accounts.get(i))
        .filter(|k| *k != program && **k != Pubkey::default())
        .copied();
    let body = &data[def.discriminator.len()..];
    let (args, error) = match idl.decode_args(def, body) {
        Ok(a) => (a, None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    DecodedIx {
        name: def.name.clone(),
        args,
        error,
        market,
    }
}

/// The account of `ix_name` named `account`, from an invocation's account list.
pub fn account_of(ix_name: &str, account: &str, accounts: &[Pubkey]) -> Option<Pubkey> {
    idl::idl()
        .instruction(ix_name)?
        .account_index(account)
        .and_then(|i| accounts.get(i))
        .copied()
}

/// `propose_config`'s `ConfigChange` from the instruction data: `(kind, payload JSON, hash hex)`
/// with the hash the program computes (`change_hash(kind, encode(change))`).
pub fn config_change(data: &[u8]) -> anyhow::Result<(u8, Value, String)> {
    let idl = idl::idl();
    let def = idl
        .instruction("propose_config")
        .context("propose_config in the IDL")?;
    let body = data
        .strip_prefix(def.discriminator.as_slice())
        .context("not propose_config")?;
    let change = shortwin::state::ConfigChange::try_from_slice(body)
        .map_err(|e| anyhow!("ConfigChange: {e}"))?;
    let (kind, payload) = change
        .encode()
        .map_err(|e| anyhow!("ConfigChange::encode: {e:?}"))?;
    let hash = shortwin::state::change_hash(kind, &payload);
    let args = idl
        .decode_args(def, body)?
        .context("propose_config has an argument")?;
    let change_json = args.get("change").cloned().unwrap_or(Value::Null);
    Ok((kind, change_json, hex(&hash)))
}

/// `ConfigChange` kind names (variant index + 1).
pub fn config_kind_name(kind: u8) -> &'static str {
    match kind {
        1 => "Params",
        2 => "EmaParams",
        3 => "Kink",
        4 => "Authorities",
        5 => "Timelock",
        6 => "EnableVenues",
        7 => "OracleFeed",
        _ => "Unknown",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use anchor_lang::{AnchorSerialize, Discriminator};
    use shortwin::events as e;

    use super::*;

    fn k(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    /// One sample of every short.win event, Borsh-encoded with its discriminator (the decoder
    /// tests' and the writer tests' synthetic payloads).
    pub(crate) fn every_event(market: Pubkey) -> Vec<(&'static str, Vec<u8>)> {
        fn enc<T: AnchorSerialize + Discriminator>(
            name: &'static str,
            ev: T,
        ) -> (&'static str, Vec<u8>) {
            let mut v = T::DISCRIMINATOR.to_vec();
            v.extend(crate::ingest::decode::borsh_bytes(&ev));
            (name, v)
        }
        macro_rules! lender {
            ($t:ident) => {
                enc(
                    stringify!($t),
                    e::$t {
                        market,
                        slot: 10,
                        ts: 20,
                        owner: k(2),
                        amount: 3,
                        shares: 4,
                        t_after: 5,
                        s_after: 6,
                        sol_paid: 7,
                        pot_draw: 8,
                        lock: 9,
                        lock_until: 10,
                        epoch: 11,
                    },
                )
            };
        }
        macro_rules! pool {
            ($t:ident) => {
                enc(
                    stringify!($t),
                    e::$t {
                        market,
                        slot: 10,
                        ts: 20,
                        candidate: k(3),
                        kind: 3,
                        d_cand: 4,
                        d_inc: 5,
                        window_ends: 6,
                    },
                )
            };
        }
        vec![
            enc(
                "MarketCreated",
                e::MarketCreated {
                    market,
                    slot: 10,
                    ts: 20,
                    mint: k(4),
                    token_program: k(5),
                    venue: 6,
                    creator: k(6),
                    bonding_curve: Pubkey::default(),
                    canonical_pool: k(7),
                    ray_pool: Pubkey::default(),
                    tier: 3,
                    creation_fee: 1_000,
                },
            ),
            lender!(Deposit),
            lender!(Withdraw),
            lender!(ClaimSol),
            lender!(DeficitCovered),
            lender!(LenderSynced),
            enc(
                "Open",
                e::Open {
                    market,
                    slot: 10,
                    ts: 20,
                    position: k(8),
                    borrower: k(9),
                    nonce: 1,
                    q: 100,
                    s: 90,
                    m: 50,
                    v: 140,
                    n_open: 100,
                    fee_open: 1,
                    tier: 3,
                    entry_price: 2,
                    spot_wad: 3,
                    ema_wad: 4,
                    b_spot: 5,
                    b_ema: 6,
                    deadline: 7,
                    venue: 6,
                    venue_rent: 0,
                },
            ),
            enc(
                "Close",
                e::Close {
                    market,
                    slot: 10,
                    ts: 20,
                    position: k(8),
                    borrower: k(9),
                    q: 100,
                    buyback_cost: 95,
                    rent: 3,
                    rent_protocol: 1,
                    borrower_payout: 40,
                    b_live: 95,
                    token_surplus: 0,
                },
            ),
            enc(
                "Liquidate",
                e::Liquidate {
                    market,
                    slot: 10,
                    ts: 20,
                    position: k(8),
                    keeper: k(10),
                    cr_spot_bps: 1,
                    cr_ema_bps: 2,
                    b_live: 3,
                    b_ref: 4,
                    p_ref: 5,
                    delta_bps: 6,
                    buyback_cost: 7,
                    rent: 8,
                    penalty: 9,
                    penalty_lender: 10,
                    penalty_protocol: 11,
                    penalty_keeper: 12,
                    keeper_topup: 13,
                    borrower_payout: 14,
                    expired: true,
                    token_surplus: 15,
                    b_live_risk: 16,
                    max_in: 17,
                },
            ),
            enc(
                "Expire",
                e::Expire {
                    market,
                    slot: 10,
                    ts: 20,
                    position: k(8),
                    keeper: k(10),
                    b_live: 1,
                    b_ref: 2,
                    pi_ref: 3,
                    buyback_cost: 4,
                    rent: 5,
                    expiry_fee: 6,
                    keeper_reward: 7,
                    borrower_payout: 8,
                    b_live_risk: 9,
                    max_in: 10,
                    token_surplus: 11,
                },
            ),
            enc(
                "SolSettle",
                e::SolSettle {
                    market,
                    slot: 10,
                    ts: 20,
                    position: k(8),
                    keeper: k(10),
                    condition: 1,
                    b_live: 1,
                    b_rs: 2,
                    b_ema: 3,
                    p_lo: 4,
                    p_hi: 5,
                    l_lo: 6,
                    l_hi: 7,
                    x_lo: 8,
                    x_hi: 9,
                    pprime_lo: 10,
                    pprime_hi: 11,
                    fv: 12,
                    k_s: 13,
                    lender_amount: 14,
                    lump: 15,
                    borrower_amount: 16,
                    ema_age_s: 17,
                    vest_flushed: 18,
                },
            ),
            enc(
                "SettleArmed",
                e::SettleArmed {
                    market,
                    slot: 10,
                    ts: 20,
                    position: k(8),
                    s_count: 1,
                    s_since: 2,
                    reason: 1,
                },
            ),
            enc(
                "SettleDisarmed",
                e::SettleDisarmed {
                    market,
                    slot: 10,
                    ts: 20,
                    position: k(8),
                    s_count: 1,
                    s_since: 2,
                    reason: 2,
                },
            ),
            enc(
                "EpochRolled",
                e::EpochRolled {
                    market,
                    slot: 10,
                    ts: 20,
                    epoch: 0,
                    acc_end: -5,
                    num: 1,
                    den: 1_000,
                    vest_flushed: 0,
                },
            ),
            enc(
                "ProtocolFeesSwept",
                e::ProtocolFeesSwept {
                    market,
                    slot: 10,
                    ts: 20,
                    amount: 5,
                    recipient: k(11),
                },
            ),
            enc(
                "MarginAdded",
                e::MarginAdded {
                    market,
                    slot: 10,
                    ts: 20,
                    position: k(8),
                    amount: 5,
                },
            ),
            enc(
                "PriceCranked",
                e::PriceCranked {
                    market,
                    slot: 10,
                    ts: 20,
                    spot_wad: 1,
                    ema_price: 2,
                    ema_liq: 3,
                    liq_slow: 4,
                    ema_count: 5,
                    lb_lo: 6,
                    lb_hi: 7,
                    tier: 2,
                    util_bps: 8,
                    rent_basis: [9, 10, 11],
                    vest_bal: 12,
                },
            ),
            enc(
                "VenueStateChanged",
                e::VenueStateChanged {
                    market,
                    slot: 10,
                    ts: 20,
                    from: 1,
                    to: 2,
                    pool: k(12),
                    graduated_at: 0,
                },
            ),
            enc(
                "RouteChainChanged",
                e::RouteChainChanged {
                    market,
                    slot: 10,
                    ts: 20,
                    no_route_since: 1,
                    last_no_route_obs: 2,
                    pool_fail_since: 3,
                    last_pool_fail_obs: 4,
                },
            ),
            pool!(PoolProposed),
            pool!(PoolSwitched),
            pool!(PoolSwitchCancelled),
            enc(
                "TraderSwept",
                e::TraderSwept {
                    market,
                    slot: 10,
                    ts: 20,
                    lamports: 5,
                    tokens: 0,
                    to: 0,
                    wipe_bits: 0,
                },
            ),
            enc(
                "VenueRewardsClaimed",
                e::VenueRewardsClaimed {
                    market,
                    slot: 10,
                    ts: 20,
                    lamports: 5,
                    to: 2,
                    wipe_bits: 0,
                },
            ),
            enc(
                "MintSafetyTripped",
                e::MintSafetyTripped {
                    market,
                    slot: 10,
                    ts: 20,
                    mint: k(4),
                },
            ),
            enc(
                "VenueUpgradeFlagged",
                e::VenueUpgradeFlagged {
                    market: Pubkey::default(),
                    slot: 10,
                    ts: 20,
                    venue: 3,
                    guard_slot: 1,
                    programdata_slot: 2,
                    flagged_at: 3,
                },
            ),
            enc(
                "VenueRevalidated",
                e::VenueRevalidated {
                    market: Pubkey::default(),
                    slot: 10,
                    ts: 20,
                    venue: 3,
                    guard_slot: 1,
                    programdata_slot: 2,
                    flagged_at: 3,
                    expected_slot: 2,
                },
            ),
            enc(
                "CoverageGap",
                e::CoverageGap {
                    market,
                    slot: 10,
                    ts: 20,
                    gap_s: 41,
                    open_positions: 1,
                },
            ),
            enc(
                "CoveragePauseSet",
                e::CoveragePauseSet {
                    market,
                    slot: 10,
                    ts: 20,
                    paused: true,
                    reason: 1,
                    by: k(13),
                },
            ),
            enc(
                "ConfigProposed",
                e::ConfigProposed {
                    market: Pubkey::default(),
                    slot: 10,
                    ts: 20,
                    kind: 5,
                    hash: [1; 32],
                    not_before: 30,
                },
            ),
            enc(
                "ConfigApplied",
                e::ConfigApplied {
                    market: Pubkey::default(),
                    slot: 10,
                    ts: 20,
                    kind: 5,
                    hash: [1; 32],
                },
            ),
            enc(
                "ConfigCancelled",
                e::ConfigCancelled {
                    market: Pubkey::default(),
                    slot: 10,
                    ts: 20,
                    kind: 5,
                    hash: [1; 32],
                    by: k(13),
                },
            ),
            enc(
                "PauseSet",
                e::PauseSet {
                    market,
                    slot: 10,
                    ts: 20,
                    scope: 1,
                    paused: true,
                    by: k(13),
                },
            ),
            enc(
                "VenueDisabled",
                e::VenueDisabled {
                    market: Pubkey::default(),
                    slot: 10,
                    ts: 20,
                    bit: 1,
                    venue_enabled: 0b1101,
                },
            ),
            enc(
                "RouteBrokenSet",
                e::RouteBrokenSet {
                    market,
                    slot: 10,
                    ts: 20,
                    scope: 1,
                    venue_bit: 0,
                    broken: true,
                },
            ),
            enc(
                "VenueExtrasSet",
                e::VenueExtrasSet {
                    market: Pubkey::default(),
                    slot: 10,
                    ts: 20,
                    venue: 3,
                    ix: 0,
                    version: 1,
                    keys: [
                        k(14),
                        Pubkey::default(),
                        Pubkey::default(),
                        Pubkey::default(),
                    ],
                    stable_version: 1,
                },
            ),
        ]
    }

    #[test]
    fn every_catalog_event_decodes_with_both_layouts() {
        let market = k(1);
        let all = every_event(market);
        assert_eq!(all.len(), SHORTWIN_EVENTS.len());
        for (name, payload) in &all {
            let ev = decode_shortwin_event(payload).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert_eq!(ev.kind, *name);
            if !matches!(
                *name,
                "VenueUpgradeFlagged"
                    | "VenueRevalidated"
                    | "ConfigProposed"
                    | "ConfigApplied"
                    | "ConfigCancelled"
                    | "VenueDisabled"
                    | "VenueExtrasSet"
            ) {
                assert_eq!(key_field(&ev.data, "market"), Some(market), "{name}");
            } else {
                assert_eq!(ev.data["market"], Value::Null, "{name}");
            }
            assert_eq!(u64_field(&ev.data, "slot"), 10, "{name}");
            assert_eq!(i64_field(&ev.data, "ts"), 20, "{name}");
            assert_eq!(ev.trailing, 0, "{name}");
            // A truncated payload never decodes.
            assert!(decode_shortwin_event(&payload[..payload.len() - 1]).is_err());
            // One with appended bytes (a newer program version) decodes on its known prefix
            // (M8-SEC-13): the same fields, the extra bytes counted.
            let mut long = payload.clone();
            long.extend([0u8; 9]);
            let ext = decode_shortwin_event(&long).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert_eq!(
                (ext.kind, &ext.data, ext.trailing),
                (ev.kind, &ev.data, 9),
                "{name}"
            );
            assert_eq!(
                payload_market(payload),
                key_field(&ev.data, "market"),
                "{name}"
            );
        }
        let (_, open) = &all[6];
        let ev = decode_shortwin_event(open).expect("Open");
        assert_eq!(ev.data["q"], "100");
        assert_eq!(ev.data["tier"], 3);
        assert_eq!(actor_of("Open", &ev.data, &k(99)), Some(k(9)));
        assert_eq!(position_of(&ev.data), Some(k(8)));
    }

    #[test]
    fn instructions_resolve_names_markets_and_the_config_hash() {
        let ix = idl::idl().instruction("set_pause").expect("set_pause");
        let program = shortwin::ID;
        let mut data = ix.discriminator.clone();
        data.extend([0u8, 1u8]);
        // Global scope: the optional market is the program id (Anchor's none).
        let accounts = vec![k(1), k(2), program, k(4), program];
        let d = decode_instruction(&program, &data, &accounts);
        assert_eq!(d.name, "set_pause");
        assert_eq!(d.market, None);
        assert_eq!(d.args.expect("args")["scope"], "Global");
        let accounts = vec![k(1), k(2), k(3), k(4), program];
        assert_eq!(
            decode_instruction(&program, &data, &accounts).market,
            Some(k(3))
        );
        let bad = decode_instruction(&program, &[1, 2, 3, 4, 5, 6, 7, 8], &[]);
        assert_eq!(bad.name, "unknown");

        let change = shortwin::state::ConfigChange::Timelock { timelock_s: 90_000 };
        let pc = idl::idl()
            .instruction("propose_config")
            .expect("propose_config");
        let mut data = pc.discriminator.clone();
        data.extend(crate::ingest::decode::borsh_bytes(&change));
        let (kind, payload, hash) = config_change(&data).expect("change");
        assert_eq!(kind, 5);
        assert_eq!(payload["Timelock"]["timelock_s"], 90_000);
        let (kind2, enc) = change.encode().expect("encode");
        assert_eq!(kind2, 5);
        assert_eq!(hash, hex(&shortwin::state::change_hash(kind, &enc)));
        assert_eq!(config_kind_name(kind), "Timelock");
    }
}
