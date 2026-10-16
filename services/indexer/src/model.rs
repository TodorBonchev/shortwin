//! The shared vocabulary of the schema, the writer and the API (INDEXER.md §5, §6). FOUNDATION:
//! every string here is a value a migration's CHECK constraint or the OpenAPI document also
//! names; change them together.

use anchor_lang::Discriminator;

/// Where an event was found (`events.ev_source`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i16)]
pub enum EvSource {
    /// An `emit_cpi!` self-CPI inner instruction (`EVENT_IX_TAG_LE ‖ discriminator ‖ Borsh`).
    Cpi = 0,
    /// An `emit!` "Program data: <base64>" log line of the emitting program.
    Log = 1,
    /// An instruction's arguments (quarantine only).
    Instruction = 2,
}

/// How an event is emitted (INDEXER.md §5.1 catalog).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Emission {
    /// `emit_cpi!` only.
    Cpi,
    /// `emit!` only (a log event: advisory, may be lost to log truncation).
    Log,
    /// `emit_cpi!` from one instruction, `emit!` from others (`MintSafetyTripped`).
    Both,
}

/// One entry of the event catalog.
#[derive(Clone, Copy, Debug)]
pub struct EventSpec {
    /// The Rust type name, stored in `events.kind`.
    pub name: &'static str,
    pub discriminator: &'static [u8],
    pub emission: Emission,
}

macro_rules! spec {
    ($path:path, $name:literal, $em:ident) => {
        EventSpec {
            name: $name,
            discriminator: <$path as Discriminator>::DISCRIMINATOR,
            emission: Emission::$em,
        }
    };
}

/// Every short.win event (`programs/shortwin/src/events.rs`, the IDL's 36 events), in the
/// order of INDEXER.md §5.3.
pub const SHORTWIN_EVENTS: [EventSpec; 36] = {
    use shortwin::events as e;
    [
        spec!(e::MarketCreated, "MarketCreated", Cpi),
        spec!(e::Deposit, "Deposit", Cpi),
        spec!(e::Withdraw, "Withdraw", Cpi),
        spec!(e::ClaimSol, "ClaimSol", Cpi),
        spec!(e::DeficitCovered, "DeficitCovered", Cpi),
        spec!(e::LenderSynced, "LenderSynced", Cpi),
        spec!(e::Open, "Open", Cpi),
        spec!(e::Close, "Close", Cpi),
        spec!(e::Liquidate, "Liquidate", Cpi),
        spec!(e::Expire, "Expire", Cpi),
        spec!(e::SolSettle, "SolSettle", Cpi),
        spec!(e::SettleArmed, "SettleArmed", Cpi),
        spec!(e::SettleDisarmed, "SettleDisarmed", Cpi),
        spec!(e::EpochRolled, "EpochRolled", Cpi),
        spec!(e::ProtocolFeesSwept, "ProtocolFeesSwept", Cpi),
        spec!(e::MarginAdded, "MarginAdded", Cpi),
        spec!(e::PriceCranked, "PriceCranked", Cpi),
        spec!(e::VenueStateChanged, "VenueStateChanged", Cpi),
        spec!(e::RouteChainChanged, "RouteChainChanged", Cpi),
        spec!(e::PoolProposed, "PoolProposed", Cpi),
        spec!(e::PoolSwitched, "PoolSwitched", Cpi),
        spec!(e::PoolSwitchCancelled, "PoolSwitchCancelled", Cpi),
        spec!(e::TraderSwept, "TraderSwept", Cpi),
        spec!(e::VenueRewardsClaimed, "VenueRewardsClaimed", Cpi),
        spec!(e::MintSafetyTripped, "MintSafetyTripped", Both),
        spec!(e::VenueUpgradeFlagged, "VenueUpgradeFlagged", Cpi),
        spec!(e::VenueRevalidated, "VenueRevalidated", Cpi),
        spec!(e::CoverageGap, "CoverageGap", Log),
        spec!(e::CoveragePauseSet, "CoveragePauseSet", Cpi),
        spec!(e::ConfigProposed, "ConfigProposed", Cpi),
        spec!(e::ConfigApplied, "ConfigApplied", Cpi),
        spec!(e::ConfigCancelled, "ConfigCancelled", Cpi),
        spec!(e::PauseSet, "PauseSet", Cpi),
        spec!(e::VenueDisabled, "VenueDisabled", Cpi),
        spec!(e::RouteBrokenSet, "RouteBrokenSet", Cpi),
        spec!(e::VenueExtrasSet, "VenueExtrasSet", Cpi),
    ]
};

/// `Market.venue` / `VenueKind` names (API and `venue_*` tables).
pub fn venue_name(kind: u8) -> &'static str {
    match kind {
        0 => "none",
        1 => "pump_curve",
        2 => "pump_migrating",
        3 => "pump_swap",
        4 => "raydium_v4",
        5 => "raydium_cpmm",
        6 => "mock",
        _ => "unknown",
    }
}

/// `VenueGuard` index names (`GUARD_*`).
pub fn guard_name(index: u8) -> &'static str {
    match index {
        0 => "pump",
        1 => "pump_amm",
        2 => "pump_fees",
        3 => "raydium_v4",
        4 => "raydium_cpmm",
        5 => "jupiter",
        _ => "unknown",
    }
}

/// Tier names (`Market.tier`: 0 A … 3 D).
pub fn tier_name(tier: u8) -> &'static str {
    match tier {
        0 => "A",
        1 => "B",
        2 => "C",
        3 => "D",
        _ => "unknown",
    }
}

/// `positions.status`.
pub const POSITION_STATUSES: [&str; 5] = ["open", "closed", "liquidated", "expired", "settled"];

/// `fees.fee_type` and its recipient (INDEXER.md §8.2).
pub const FEE_TYPES: [(&str, &str); 18] = [
    ("creation_fee", "protocol"),
    ("open_fee", "protocol"),
    ("venue_rent", "venue"),
    ("rent_lender", "lenders"),
    ("rent_protocol", "protocol"),
    ("liq_penalty_lender", "lenders"),
    ("liq_penalty_protocol", "protocol"),
    ("liq_penalty_keeper", "keeper"),
    ("liq_keeper_topup", "keeper"),
    ("expiry_fee", "lenders"),
    ("expiry_keeper_reward", "keeper"),
    ("settle_keeper_reward", "keeper"),
    ("trader_sweep_lenders", "lenders"),
    ("trader_sweep_wipe", "lenders"),
    ("trader_sweep_protocol", "protocol"),
    ("venue_rewards_lenders", "lenders"),
    ("venue_rewards_wipe", "lenders"),
    ("venue_rewards_protocol", "protocol"),
];

/// `ingest_txs.source`.
pub const SOURCES: [&str; 4] = ["rpc_backfill", "ws_live", "yellowstone", "replay"];

/// Instructions whose prologue records an observation (keeper coverage, §8.5).
pub const OBSERVING_INSTRUCTIONS: [&str; 8] = [
    "crank_price",
    "open_short",
    "close_short",
    "liquidate",
    "expire",
    "sol_settle",
    "propose_pool",
    "execute_pool_switch",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalog is the IDL's event list, with the IDL's discriminators (INDEXER.md §5.3).
    #[test]
    fn catalog_matches_the_idl() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../clients/js/idl/shortwin.json"
        );
        let idl: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("the IDL")).expect("json");
        let events = idl["events"].as_array().expect("events");
        assert_eq!(events.len(), SHORTWIN_EVENTS.len());
        for ev in events {
            let name = ev["name"].as_str().expect("name");
            let disc: Vec<u8> = ev["discriminator"]
                .as_array()
                .expect("discriminator")
                .iter()
                .map(|b| u8::try_from(b.as_u64().expect("byte")).expect("u8"))
                .collect();
            let spec = SHORTWIN_EVENTS
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{name} missing from the catalog"));
            assert_eq!(spec.discriminator, disc.as_slice(), "{name}");
        }
    }

    #[test]
    fn discriminators_are_distinct() {
        let mut all: Vec<&[u8]> = SHORTWIN_EVENTS.iter().map(|s| s.discriminator).collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), SHORTWIN_EVENTS.len());
    }
}
