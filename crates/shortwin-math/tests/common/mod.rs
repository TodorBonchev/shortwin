//! Shared helpers for the integration tests (conventions: `tests/vectors/README.md`).
//!
//! Include with `mod common;` from any `tests/<track>_*.rs`.
#![allow(dead_code)]

use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;

use proptest::test_runner::Config;
use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer, Error as _};

/// Minimum cases per property (ARCHITECTURE §8.1: 2,000 in CI). `PROPTEST_CASES`
/// can raise it, never lower it.
pub const MIN_CASES: u32 = 2_000;

/// Proptest configuration for every property in this crate.
pub fn proptest_config() -> Config {
    let default = Config::default();
    Config {
        cases: default.cases.max(MIN_CASES),
        ..default
    }
}

/// `crates/shortwin-math/tests/vectors`.
pub fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
}

/// Loads `tests/vectors/<track>/<file>` into `T`, panicking with the path on failure.
pub fn load_vectors<T: DeserializeOwned>(track: &str, file: &str) -> T {
    let path = vectors_dir().join(track).join(file);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("cannot parse {}: {e}", path.display()))
}

/// An integer written as a decimal string in a vector file (any `FromStr` type:
/// `u64`, `u128`, `i128`, `U256`, ...). Works inside `Vec`, `Option` and maps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dec<T>(pub T);

impl<'de, T> Deserialize<'de> for Dec<T>
where
    T: FromStr,
    T::Err: Display,
{
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        dec(d).map(Dec)
    }
}

/// Field-level form of [`Dec`]: `#[serde(deserialize_with = "common::dec")]`.
pub fn dec<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    T::Err: Display,
{
    let s = String::deserialize(d)?;
    s.parse().map_err(D::Error::custom)
}
