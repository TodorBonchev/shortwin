//! The create-market mint safety verdict (FRONTEND.md §3.5, FE-D5): the SPEC §3 allowlist
//! filter `shortwin_venue_state::mint::check` (the byte-level copy of the program's
//! `check_mint_safety`, differential-tested against it in the program crate; ARCH §3.2 check
//! 1), run on the mint's raw bytes.
//!
//! [`mint_check`] reports what `create_market` fails with (the first reason, in the program's
//! order) and every other failing rule the bytes show, so the user sees everything wrong with
//! the token at once. Every reason is a [`mint::check`] verdict: after each one the rule it
//! names is lifted on a copy (the native key replaced, the freeze authority cleared, the
//! rejected TLV entry's type set to an allowlisted one) and the filter runs again, until it
//! passes or stops on a rule nothing can lift (a foreign owner, a malformed base, a TLV that
//! does not parse). No rule is restated here.

use shortwin_math::MathError;
use shortwin_venue_state::Key;
use shortwin_venue_state::mint::{
    self, ALLOWED_EXTENSIONS, FREEZE_AUTHORITY_OFFSET, MintBase, MintReject, TLV_OFFSET, ext,
};
use wasm_bindgen::JsError;
use wasm_bindgen::prelude::wasm_bindgen;

/// A 32-byte key from JS bytes; `invalid input` for any other length.
fn key_of(bytes: &[u8]) -> Result<Key, MathError> {
    Key::try_from(bytes).map_err(|_| MathError::InvalidInput)
}

/// The program error a rejection is (ARCH §3.11's mint group).
fn code(r: MintReject) -> &'static str {
    match r {
        MintReject::UnsupportedProgram => "MintUnsupportedProgram",
        MintReject::Native => "MintNative",
        MintReject::Malformed => "MintMalformed",
        MintReject::FreezeAuthority => "MintFreezeAuthority",
        MintReject::DisallowedExtension(_) => "MintDisallowedExtension",
    }
}

/// The TLV entries after [`TLV_OFFSET`] as `(type, offset)`, walked as `mint::check` walks
/// them (an entry ends the walk when fewer than 2 bytes remain or its type is 0); the walk
/// stops after an entry whose length or value runs past the end. Display and lifting only:
/// the decision is [`mint::check`]'s.
fn tlv_entries(tlv: &[u8]) -> Vec<(u16, usize)> {
    let u16_at = |at: usize| -> Option<u16> {
        let b = tlv.get(at..at.checked_add(2)?)?;
        Some(u16::from_le_bytes([*b.first()?, *b.get(1)?]))
    };
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(t) = u16_at(at) {
        if t == ext::UNINITIALIZED {
            break;
        }
        out.push((t, at));
        let Some(len) = at.checked_add(2).and_then(u16_at) else {
            break;
        };
        match at
            .checked_add(4)
            .and_then(|v| v.checked_add(usize::from(len)))
        {
            Some(end) if end <= tlv.len() => at = end,
            _ => break,
        }
    }
    out
}

/// The filter's verdict on one mint account (see the module docs).
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintVerdict {
    reasons: Vec<MintReject>,
    base: Option<MintBase>,
    types: Vec<u16>,
}

#[wasm_bindgen]
impl MintVerdict {
    /// The mint passes the filter (`create_market` accepts it).
    #[wasm_bindgen(getter)]
    pub fn ok(&self) -> bool {
        self.reasons.is_empty()
    }

    /// Every failing rule as its program error name, the first being what `create_market`
    /// returns (`MintUnsupportedProgram`, `MintNative`, `MintMalformed`,
    /// `MintFreezeAuthority`, `MintDisallowedExtension`).
    #[wasm_bindgen(getter)]
    pub fn codes(&self) -> Vec<String> {
        self.reasons.iter().map(|&r| code(r).into()).collect()
    }

    /// Per reason, the rejected extension type of a `MintDisallowedExtension`; `-1` for a TLV
    /// that does not parse (`DisallowedExtension(None)`) and for every other rule.
    #[wasm_bindgen(getter)]
    pub fn extensions(&self) -> Vec<i32> {
        self.reasons
            .iter()
            .map(|r| match r {
                MintReject::DisallowedExtension(Some(t)) => i32::from(*t),
                _ => -1,
            })
            .collect()
    }

    /// Token-2022 extension types in TLV order, allowed and not (empty for an SPL Token mint
    /// and for one whose base is rejected).
    #[wasm_bindgen(getter)]
    pub fn types(&self) -> Vec<u16> {
        self.types.clone()
    }

    /// The base fields unpack (owner a token program, the base well-formed).
    #[wasm_bindgen(getter, js_name = hasBase)]
    pub fn has_base(&self) -> bool {
        self.base.is_some()
    }

    /// `mint_authority`, `undefined` when none (or no base).
    #[wasm_bindgen(getter, js_name = mintAuthority)]
    pub fn mint_authority(&self) -> Option<Vec<u8>> {
        self.base.and_then(|b| b.mint_authority).map(Vec::from)
    }

    /// `freeze_authority`, `undefined` when none (or no base).
    #[wasm_bindgen(getter, js_name = freezeAuthority)]
    pub fn freeze_authority(&self) -> Option<Vec<u8>> {
        self.base.and_then(|b| b.freeze_authority).map(Vec::from)
    }

    /// Raw supply (0 without a base).
    #[wasm_bindgen(getter)]
    pub fn supply(&self) -> u64 {
        self.base.map_or(0, |b| b.supply)
    }

    /// Decimals (0 without a base).
    #[wasm_bindgen(getter)]
    pub fn decimals(&self) -> u8 {
        self.base.map_or(0, |b| b.decimals)
    }
}

/// The filter on `(key, owner, data)` (32-byte keys; throws `invalid input` otherwise). An
/// address with no account is a System-owned one: pass the System program as `owner` and no
/// data (`MintUnsupportedProgram`).
#[wasm_bindgen(js_name = mintCheck)]
pub fn mint_check(key: &[u8], owner: &[u8], data: &[u8]) -> Result<MintVerdict, JsError> {
    Ok(verdict(&key_of(key)?, &key_of(owner)?, data))
}

fn verdict(key: &Key, owner: &Key, data: &[u8]) -> MintVerdict {
    let mut reasons = Vec::new();
    let (mut key, mut lifted) = (*key, data.to_vec());
    // Each pass lifts one rule: the key, the freeze authority, one TLV entry each.
    let passes = data.len().saturating_add(3);
    for _ in 0..passes {
        let Err(r) = mint::check(&key, owner, &lifted) else {
            break;
        };
        reasons.push(r);
        match r {
            MintReject::Native => key = [0; 32],
            MintReject::FreezeAuthority => {
                let tag = FREEZE_AUTHORITY_OFFSET..FREEZE_AUTHORITY_OFFSET.saturating_add(4);
                lifted
                    .get_mut(tag)
                    .into_iter()
                    .flatten()
                    .for_each(|b| *b = 0);
            }
            MintReject::DisallowedExtension(Some(t)) => {
                let tlv = lifted.get(TLV_OFFSET..).unwrap_or(&[]);
                let entry = tlv_entries(tlv).into_iter().find(|&(e, _)| e == t);
                let at = entry.and_then(|(_, at)| at.checked_add(TLV_OFFSET));
                let Some(b) = at.and_then(|at| lifted.get_mut(at..at.saturating_add(2))) else {
                    break;
                };
                b.copy_from_slice(&ext::METADATA_POINTER.to_le_bytes());
            }
            MintReject::UnsupportedProgram
            | MintReject::Malformed
            | MintReject::DisallowedExtension(None) => break,
        }
    }
    // The base and the extension list of the mint as it is (lifting changes neither's layout).
    let unpacks = !reasons
        .iter()
        .any(|r| matches!(r, MintReject::UnsupportedProgram | MintReject::Malformed));
    let (base, types) = if unpacks {
        let tlv = data.get(TLV_OFFSET..).unwrap_or(&[]);
        let types = tlv_entries(tlv).into_iter().map(|(t, _)| t).collect();
        (mint::read_base(owner, data).ok(), types)
    } else {
        (None, Vec::new())
    };
    MintVerdict {
        reasons,
        base,
        types,
    }
}

/// `true` if the Token-2022 extension type is on the v1 allowlist (SPEC §3; R-MINT §4).
#[wasm_bindgen(js_name = isAllowedExtension)]
pub fn is_allowed_extension(ext_type: u16) -> bool {
    mint::is_allowed_extension(ext_type)
}

/// The v1 allowlist itself (display: the types a mint may carry).
#[wasm_bindgen(js_name = allowedExtensions)]
pub fn allowed_extensions() -> Vec<u16> {
    ALLOWED_EXTENSIONS.to_vec()
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::unwrap_used
)]
mod tests {
    use super::*;
    use shortwin_venue_state::mint::{
        ACCOUNT_TYPE_MINT, ACCOUNT_TYPE_OFFSET, MINT_LEN, MULTISIG_LEN, NATIVE_MINT,
        NATIVE_MINT_2022, TOKEN_2022_PROGRAM, TOKEN_PROGRAM,
    };

    const KEY: Key = [7; 32];

    fn base(freeze: bool) -> Vec<u8> {
        let mut d = vec![0u8; MINT_LEN];
        d[0] = 1;
        d[4..36].copy_from_slice(&[9; 32]);
        d[36..44].copy_from_slice(&1_000u64.to_le_bytes());
        d[44] = 6;
        d[45] = 1;
        if freeze {
            d[46] = 1;
            d[50..82].copy_from_slice(&[8; 32]);
        }
        d
    }

    fn extended(entries: &[(u16, u16)], freeze: bool) -> Vec<u8> {
        let mut d = base(freeze);
        d.resize(TLV_OFFSET, 0);
        d[ACCOUNT_TYPE_OFFSET] = ACCOUNT_TYPE_MINT;
        for &(t, len) in entries {
            d.extend_from_slice(&t.to_le_bytes());
            d.extend_from_slice(&len.to_le_bytes());
            d.resize(d.len() + usize::from(len), 0xab);
        }
        d
    }

    fn codes(v: &MintVerdict) -> Vec<String> {
        v.codes()
    }

    #[test]
    fn passing_mints_and_their_fields() {
        let v = verdict(&KEY, &TOKEN_PROGRAM, &base(false));
        assert!(v.ok());
        assert_eq!((v.supply(), v.decimals()), (1_000, 6));
        assert_eq!(v.mint_authority(), Some(vec![9; 32]));
        assert_eq!(v.freeze_authority(), None);
        assert!(v.types().is_empty());
        // The pump create_v2 shape: MetadataPointer then TokenMetadata.
        let v = verdict(
            &KEY,
            &TOKEN_2022_PROGRAM,
            &extended(&[(18, 64), (19, 152)], false),
        );
        assert!(v.ok());
        assert_eq!(v.types(), vec![18, 19]);
        let all: Vec<_> = ALLOWED_EXTENSIONS.iter().map(|&t| (t, 16)).collect();
        assert!(verdict(&KEY, &TOKEN_2022_PROGRAM, &extended(&all, false)).ok());
        assert_eq!(allowed_extensions(), ALLOWED_EXTENSIONS.to_vec());
        assert!(is_allowed_extension(23) && !is_allowed_extension(12));
    }

    #[test]
    fn every_failing_rule_in_the_programs_order() {
        // A foreign owner (or no account) stops at once.
        let v = verdict(&KEY, &[0; 32], &[]);
        assert_eq!(codes(&v), ["MintUnsupportedProgram"]);
        assert!(!v.has_base());
        // The native mint, then its freeze authority (keys lifted, the bytes unchanged).
        let v = verdict(&NATIVE_MINT, &TOKEN_PROGRAM, &base(true));
        assert_eq!(codes(&v), ["MintNative", "MintFreezeAuthority"]);
        assert_eq!(v.freeze_authority(), Some(vec![8; 32]));
        assert_eq!(
            codes(&verdict(
                &NATIVE_MINT_2022,
                &TOKEN_2022_PROGRAM,
                &base(false)
            )),
            ["MintNative"]
        );
        // The freeze authority and each disallowed extension, then the truncated tail.
        let mut d = extended(&[(18, 64), (12, 32), (14, 64), (1, 108)], true);
        d.extend_from_slice(&[19, 0, 9]);
        let v = verdict(&KEY, &TOKEN_2022_PROGRAM, &d);
        assert_eq!(
            codes(&v),
            [
                "MintFreezeAuthority",
                "MintDisallowedExtension",
                "MintDisallowedExtension",
                "MintDisallowedExtension",
                "MintDisallowedExtension"
            ]
        );
        assert_eq!(v.extensions(), vec![-1, 12, 14, 1, -1]);
        assert_eq!(v.types(), vec![18, 12, 14, 1, 19]);
        assert_eq!(v.freeze_authority(), Some(vec![8; 32]));
        // A repeated type is listed once per entry.
        let v = verdict(
            &KEY,
            &TOKEN_2022_PROGRAM,
            &extended(&[(12, 4), (12, 4)], false),
        );
        assert_eq!(v.extensions(), vec![12, 12]);
        // Malformed wins and stops: no base, no extension list.
        let mut d = extended(&[(12, 32)], true);
        d[ACCOUNT_TYPE_OFFSET] = 2;
        let v = verdict(&KEY, &TOKEN_2022_PROGRAM, &d);
        assert_eq!(codes(&v), ["MintMalformed"]);
        assert!(!v.has_base() && v.types().is_empty());
        for len in [0, 81, 83, 165, MULTISIG_LEN] {
            let mut d = extended(&[(18, 300)], false);
            d.truncate(len);
            assert_eq!(
                codes(&verdict(&KEY, &TOKEN_2022_PROGRAM, &d)),
                ["MintMalformed"],
                "{len}"
            );
        }
        // `mintCheck` throws on a key of any other length (a JsError: Err paths are covered by
        // the JS tests).
        assert_eq!(key_of(&KEY[..31]), Err(MathError::InvalidInput));
        assert_eq!(key_of(&KEY), Ok(KEY));
    }

    /// The first reason is `mint::check`'s, on every fixture shape and thousands of byte
    /// mutations of them (xorshift; the program crate's differential test does the same
    /// against `check_mint_safety`).
    #[test]
    fn the_first_reason_is_the_programs() {
        let shapes = [
            base(false),
            base(true),
            extended(&[(18, 64), (19, 152)], false),
            extended(&[(18, 8), (12, 8), (0, 0)], true),
            extended(&[(14, 64), (1, 108), (22, 32)], false),
        ];
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut n = 0;
        for shape in &shapes {
            for owner in [TOKEN_PROGRAM, TOKEN_2022_PROGRAM, [3; 32]] {
                for key in [KEY, NATIVE_MINT, NATIVE_MINT_2022] {
                    for _ in 0..200 {
                        let mut d = shape.clone();
                        for _ in 0..(next() % 4) {
                            let i = (next() as usize) % d.len();
                            d[i] = next() as u8;
                        }
                        if next() % 5 == 0 {
                            d.truncate((next() as usize) % (d.len() + 1));
                        }
                        let v = verdict(&key, &owner, &d);
                        match mint::check(&key, &owner, &d) {
                            Ok(b) => {
                                assert!(v.ok());
                                assert_eq!(v.base, Some(b));
                            }
                            Err(r) => assert_eq!(v.reasons.first(), Some(&r)),
                        }
                        n += 1;
                    }
                }
            }
        }
        assert_eq!(n, 5 * 3 * 3 * 200);
    }
}
