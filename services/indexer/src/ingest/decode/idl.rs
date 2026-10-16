//! IDL-driven Borsh → JSON (INDEXER.md §4.2). Owner: INGEST.
//!
//! The program's Anchor IDL (`clients/js/idl/shortwin.json`, embedded at compile time) names every
//! field of every event, instruction argument and account; the typed decoders
//! ([`super::shortwin_events`]) use the program's own Rust types for the projections, and this
//! module renders the same bytes as `events.data` / `invocations.args` / `account_snapshots.data`
//! JSON with the §4.2 encoding:
//!
//! - `u8/u16/u32/i8/i16/i32` → JSON numbers, `bool` → booleans;
//! - `u64/i64/u128/i128` (and the zero-copy `PodU128` / `PodI128`) → decimal strings;
//! - `pubkey` → base58, `Pubkey::default()` → `null`;
//! - `[u8; N]` and `bytes` → lowercase hex; other arrays and vectors → JSON arrays;
//! - enums → `{"<Variant>": {fields}}` (unit variants → `"<Variant>"`);
//! - fields whose name starts with `_` (padding, reserved space) are consumed and omitted.
//!
//! Zero-copy (bytemuck) accounts decode with the same walker: their layouts have no implicit
//! padding (`Pod`), so reading the declared fields in order is reading the memory layout.

use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::{Context, anyhow, bail};
use serde_json::{Map, Value};
use solana_pubkey::Pubkey;

use crate::numeric::hex;

/// The program's IDL, as committed with the program (the catalog test pins it to the program's
/// discriminators).
const IDL_JSON: &str = include_str!("../../../../../clients/js/idl/shortwin.json");

/// A field or element type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    Bool,
    U8,
    U16,
    U32,
    U64,
    U128,
    I8,
    I16,
    I32,
    I64,
    I128,
    Pubkey,
    String,
    Bytes,
    Array(Box<Ty>, usize),
    Vec(Box<Ty>),
    Option(Box<Ty>),
    Defined(String),
}

/// A struct's or a variant's fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fields {
    Named(Vec<(String, Ty)>),
    Tuple(Vec<Ty>),
    Unit,
}

/// A defined type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeDef {
    Struct(Fields),
    Enum(Vec<(String, Fields)>),
}

/// An instruction of the IDL.
#[derive(Clone, Debug)]
pub struct IxDef {
    pub name: String,
    pub discriminator: Vec<u8>,
    /// Account names, flattened in order (optional accounts keep their slot).
    pub accounts: Vec<String>,
    pub args: Vec<(String, Ty)>,
}

impl IxDef {
    /// Index of the account named `name`.
    pub fn account_index(&self, name: &str) -> Option<usize> {
        self.accounts.iter().position(|a| a == name)
    }
}

/// The parsed IDL.
#[derive(Debug)]
pub struct Idl {
    pub types: HashMap<String, TypeDef>,
    pub instructions: Vec<IxDef>,
    /// Account type name → discriminator.
    pub accounts: Vec<(String, Vec<u8>)>,
    /// Event name → discriminator.
    pub events: Vec<(String, Vec<u8>)>,
}

/// The embedded IDL (parsed once).
pub fn idl() -> &'static Idl {
    static IDL: OnceLock<Idl> = OnceLock::new();
    IDL.get_or_init(|| Idl::parse(IDL_JSON).unwrap_or_else(|e| panic!("the embedded IDL: {e:#}")))
}

fn parse_ty(v: &Value) -> anyhow::Result<Ty> {
    if let Some(s) = v.as_str() {
        return Ok(match s {
            "bool" => Ty::Bool,
            "u8" => Ty::U8,
            "u16" => Ty::U16,
            "u32" => Ty::U32,
            "u64" => Ty::U64,
            "u128" => Ty::U128,
            "i8" => Ty::I8,
            "i16" => Ty::I16,
            "i32" => Ty::I32,
            "i64" => Ty::I64,
            "i128" => Ty::I128,
            "pubkey" | "publicKey" => Ty::Pubkey,
            "string" => Ty::String,
            "bytes" => Ty::Bytes,
            other => bail!("unsupported IDL type {other}"),
        });
    }
    let o = v.as_object().context("an IDL type")?;
    if let Some(a) = o.get("array").and_then(Value::as_array) {
        let elem = parse_ty(a.first().context("array element")?)?;
        let n = a
            .get(1)
            .and_then(Value::as_u64)
            .context("array length (generic lengths are not used)")?;
        return Ok(Ty::Array(Box::new(elem), usize::try_from(n)?));
    }
    if let Some(e) = o.get("vec") {
        return Ok(Ty::Vec(Box::new(parse_ty(e)?)));
    }
    if let Some(e) = o.get("option") {
        return Ok(Ty::Option(Box::new(parse_ty(e)?)));
    }
    if let Some(d) = o.get("defined") {
        let name = d
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| d.as_str())
            .context("defined type name")?;
        return Ok(Ty::Defined(name.to_string()));
    }
    bail!("unsupported IDL type {v}")
}

fn parse_fields(v: Option<&Value>) -> anyhow::Result<Fields> {
    let Some(arr) = v.and_then(Value::as_array) else {
        return Ok(Fields::Unit);
    };
    if arr.is_empty() {
        return Ok(Fields::Unit);
    }
    if arr
        .iter()
        .all(|f| f.get("name").is_some() && f.get("type").is_some())
    {
        let mut out = Vec::with_capacity(arr.len());
        for f in arr {
            let name = f["name"].as_str().context("field name")?.to_string();
            out.push((name, parse_ty(&f["type"])?));
        }
        return Ok(Fields::Named(out));
    }
    Ok(Fields::Tuple(
        arr.iter().map(parse_ty).collect::<anyhow::Result<_>>()?,
    ))
}

fn flatten_accounts(v: &Value, out: &mut Vec<String>) {
    if let Some(arr) = v.as_array() {
        for a in arr {
            if let Some(inner) = a.get("accounts") {
                flatten_accounts(inner, out);
            } else if let Some(n) = a.get("name").and_then(Value::as_str) {
                out.push(n.to_string());
            }
        }
    }
}

fn bytes_of(v: &Value) -> anyhow::Result<Vec<u8>> {
    v.as_array()
        .context("a discriminator")?
        .iter()
        .map(|b| {
            b.as_u64()
                .and_then(|b| u8::try_from(b).ok())
                .context("a discriminator byte")
        })
        .collect()
}

impl Idl {
    /// Parses an Anchor (spec 0.1.0) IDL document.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let doc: Value = serde_json::from_str(text).context("IDL JSON")?;
        let mut types = HashMap::new();
        for t in doc["types"].as_array().context("types")? {
            let name = t["name"].as_str().context("type name")?.to_string();
            let ty = &t["type"];
            let def = match ty["kind"].as_str() {
                Some("struct") => TypeDef::Struct(parse_fields(ty.get("fields"))?),
                Some("enum") => {
                    let mut variants = Vec::new();
                    for v in ty["variants"].as_array().context("variants")? {
                        variants.push((
                            v["name"].as_str().context("variant name")?.to_string(),
                            parse_fields(v.get("fields"))?,
                        ));
                    }
                    TypeDef::Enum(variants)
                }
                other => bail!("type {name}: unsupported kind {other:?}"),
            };
            types.insert(name, def);
        }
        let mut instructions = Vec::new();
        for ix in doc["instructions"].as_array().context("instructions")? {
            let mut accounts = Vec::new();
            flatten_accounts(&ix["accounts"], &mut accounts);
            let mut args = Vec::new();
            for a in ix["args"].as_array().context("args")? {
                args.push((
                    a["name"].as_str().context("arg name")?.to_string(),
                    parse_ty(&a["type"])?,
                ));
            }
            instructions.push(IxDef {
                name: ix["name"].as_str().context("ix name")?.to_string(),
                discriminator: bytes_of(&ix["discriminator"])?,
                accounts,
                args,
            });
        }
        let named = |key: &str| -> anyhow::Result<Vec<(String, Vec<u8>)>> {
            doc[key]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|a| {
                            Ok((
                                a["name"].as_str().context("name")?.to_string(),
                                bytes_of(&a["discriminator"])?,
                            ))
                        })
                        .collect()
                })
                .unwrap_or_else(|| Ok(Vec::new()))
        };
        Ok(Self {
            types,
            instructions,
            accounts: named("accounts")?,
            events: named("events")?,
        })
    }

    /// The instruction whose discriminator prefixes `data`.
    pub fn instruction_for(&self, data: &[u8]) -> Option<&IxDef> {
        self.instructions
            .iter()
            .find(|ix| !ix.discriminator.is_empty() && data.starts_with(&ix.discriminator))
    }

    /// The instruction named `name`.
    pub fn instruction(&self, name: &str) -> Option<&IxDef> {
        self.instructions.iter().find(|ix| ix.name == name)
    }

    /// The account type whose discriminator prefixes `data`.
    pub fn account_for(&self, data: &[u8]) -> Option<&str> {
        self.accounts
            .iter()
            .find(|(_, d)| data.starts_with(d))
            .map(|(n, _)| n.as_str())
    }

    /// Decodes a value of `ty` from `input` (advancing it).
    pub fn decode(&self, ty: &Ty, input: &mut &[u8]) -> anyhow::Result<Value> {
        decode_ty(self, ty, input, 0)
    }

    /// Decodes the named fields of struct `name` from `input` into a JSON object.
    pub fn decode_struct(&self, name: &str, input: &mut &[u8]) -> anyhow::Result<Value> {
        self.decode(&Ty::Defined(name.to_string()), input)
    }

    /// Decodes an instruction's arguments (after its discriminator) into an object; `Ok(None)`
    /// for an instruction without arguments. Trailing bytes are an error (a layout mismatch).
    pub fn decode_args(&self, ix: &IxDef, mut body: &[u8]) -> anyhow::Result<Option<Value>> {
        if ix.args.is_empty() {
            return Ok(None);
        }
        let mut m = Map::new();
        for (name, ty) in &ix.args {
            let v =
                decode_ty(self, ty, &mut body, 0).with_context(|| format!("{}.{name}", ix.name))?;
            if !name.starts_with('_') {
                m.insert(name.clone(), v);
            }
        }
        if !body.is_empty() {
            bail!("{}: {} trailing argument bytes", ix.name, body.len());
        }
        Ok(Some(Value::Object(m)))
    }
}

fn take<'a>(input: &mut &'a [u8], n: usize) -> anyhow::Result<&'a [u8]> {
    if input.len() < n {
        bail!("unexpected end of data ({} < {n} bytes)", input.len());
    }
    let (head, rest) = input.split_at(n);
    *input = rest;
    Ok(head)
}

fn arr<const N: usize>(input: &mut &[u8]) -> anyhow::Result<[u8; N]> {
    let b = take(input, N)?;
    let mut out = [0u8; N];
    out.copy_from_slice(b);
    Ok(out)
}

/// The base58 of a key, or JSON `null` for `Pubkey::default()` (§4.2).
pub fn key_json(k: &Pubkey) -> Value {
    if *k == Pubkey::default() {
        Value::Null
    } else {
        Value::String(k.to_string())
    }
}

fn decode_ty(idl: &Idl, ty: &Ty, input: &mut &[u8], depth: u32) -> anyhow::Result<Value> {
    if depth > 32 {
        bail!("type nesting too deep");
    }
    Ok(match ty {
        Ty::Bool => match take(input, 1)?[0] {
            0 => Value::Bool(false),
            1 => Value::Bool(true),
            b => bail!("invalid bool byte {b}"),
        },
        Ty::U8 => Value::from(take(input, 1)?[0]),
        Ty::I8 => Value::from(i8::from_le_bytes(arr::<1>(input)?)),
        Ty::U16 => Value::from(u16::from_le_bytes(arr(input)?)),
        Ty::I16 => Value::from(i16::from_le_bytes(arr(input)?)),
        Ty::U32 => Value::from(u32::from_le_bytes(arr(input)?)),
        Ty::I32 => Value::from(i32::from_le_bytes(arr(input)?)),
        Ty::U64 => Value::String(u64::from_le_bytes(arr(input)?).to_string()),
        Ty::I64 => Value::String(i64::from_le_bytes(arr(input)?).to_string()),
        Ty::U128 => Value::String(u128::from_le_bytes(arr(input)?).to_string()),
        Ty::I128 => Value::String(i128::from_le_bytes(arr(input)?).to_string()),
        Ty::Pubkey => key_json(&Pubkey::new_from_array(arr(input)?)),
        Ty::String => {
            let n = u32::from_le_bytes(arr(input)?);
            let b = take(input, usize::try_from(n)?)?;
            Value::String(String::from_utf8_lossy(b).into_owned())
        }
        Ty::Bytes => {
            let n = u32::from_le_bytes(arr(input)?);
            Value::String(hex(take(input, usize::try_from(n)?)?))
        }
        Ty::Array(elem, n) => {
            if **elem == Ty::U8 {
                Value::String(hex(take(input, *n)?))
            } else {
                let mut out = Vec::with_capacity(*n);
                for _ in 0..*n {
                    out.push(decode_ty(idl, elem, input, depth + 1)?);
                }
                Value::Array(out)
            }
        }
        Ty::Vec(elem) => {
            let n = usize::try_from(u32::from_le_bytes(arr(input)?))?;
            if **elem == Ty::U8 {
                Value::String(hex(take(input, n)?))
            } else {
                if n > input.len() {
                    bail!("vector of {n} elements in {} bytes", input.len());
                }
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(decode_ty(idl, elem, input, depth + 1)?);
                }
                Value::Array(out)
            }
        }
        Ty::Option(elem) => match take(input, 1)?[0] {
            0 => Value::Null,
            1 => decode_ty(idl, elem, input, depth + 1)?,
            b => bail!("invalid option tag {b}"),
        },
        Ty::Defined(name) => match name.as_str() {
            // The zero-copy wide integers (`state::pod`): 16 little-endian bytes.
            "PodU128" => Value::String(u128::from_le_bytes(arr(input)?).to_string()),
            "PodI128" => Value::String(i128::from_le_bytes(arr(input)?).to_string()),
            _ => {
                let def = idl
                    .types
                    .get(name)
                    .ok_or_else(|| anyhow!("type {name} not in the IDL"))?;
                match def {
                    TypeDef::Struct(fields) => decode_fields(idl, fields, input, depth + 1)
                        .with_context(|| name.clone())?,
                    TypeDef::Enum(variants) => {
                        let tag = usize::from(take(input, 1)?[0]);
                        let (vname, fields) = variants
                            .get(tag)
                            .ok_or_else(|| anyhow!("{name}: variant {tag} out of range"))?;
                        match fields {
                            Fields::Unit => Value::String(vname.clone()),
                            f => {
                                let mut m = Map::new();
                                m.insert(vname.clone(), decode_fields(idl, f, input, depth + 1)?);
                                Value::Object(m)
                            }
                        }
                    }
                }
            }
        },
    })
}

fn decode_fields(
    idl: &Idl,
    fields: &Fields,
    input: &mut &[u8],
    depth: u32,
) -> anyhow::Result<Value> {
    Ok(match fields {
        Fields::Unit => Value::Object(Map::new()),
        Fields::Named(fs) => {
            let mut m = Map::new();
            for (name, ty) in fs {
                let v = decode_ty(idl, ty, input, depth).with_context(|| name.clone())?;
                if !name.starts_with('_') {
                    m.insert(name.clone(), v);
                }
            }
            Value::Object(m)
        }
        Fields::Tuple(ts) => {
            if ts.len() == 1 {
                decode_ty(idl, &ts[0], input, depth)?
            } else {
                Value::Array(
                    ts.iter()
                        .map(|t| decode_ty(idl, t, input, depth))
                        .collect::<anyhow::Result<_>>()?,
                )
            }
        }
    })
}

/// Decodes a whole short.win account (discriminator ‖ data) into `(type name, JSON)`.
/// Trailing bytes after the declared fields (Anchor allocates `INIT_SPACE`, which may exceed the
/// Borsh length) are ignored.
pub fn decode_account(data: &[u8]) -> anyhow::Result<(String, Value)> {
    let idl = idl();
    let name = idl
        .account_for(data)
        .ok_or_else(|| anyhow!("not a short.win account (unknown discriminator)"))?;
    let mut body = data.get(8..).unwrap_or_default();
    let v = idl.decode_struct(name, &mut body)?;
    Ok((name.to_string(), v))
}

#[cfg(test)]
mod tests {
    use anchor_lang::Discriminator;

    use super::*;

    #[test]
    fn the_embedded_idl_parses_with_every_instruction_and_event() {
        let idl = idl();
        assert_eq!(idl.events.len(), crate::model::SHORTWIN_EVENTS.len());
        for ix in [
            "open_short",
            "propose_config",
            "initialize_config",
            "sol_settle",
            "set_venue_extras",
        ] {
            assert!(idl.instruction(ix).is_some(), "{ix}");
        }
        // `sol_settle`'s record account (the §6.4 donation transfer source).
        let settle = idl.instruction("sol_settle").expect("sol_settle");
        assert_eq!(settle.account_index("share_epoch_rec"), Some(8));
        assert_eq!(settle.account_index("market_vault"), Some(6));
    }

    #[test]
    fn events_render_with_the_section_4_2_encoding() {
        let ev = shortwin::events::SolSettle {
            market: Pubkey::new_from_array([7; 32]),
            slot: u64::MAX,
            ts: -5,
            position: Pubkey::default(),
            keeper: Pubkey::new_from_array([9; 32]),
            condition: 2,
            b_live: 1,
            b_rs: 2,
            b_ema: 3,
            p_lo: u128::MAX,
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
        };
        let bytes = crate::ingest::decode::borsh_bytes(&ev);
        let mut body = bytes.as_slice();
        let v = idl().decode_struct("SolSettle", &mut body).expect("decode");
        assert!(body.is_empty());
        assert_eq!(v["slot"], u64::MAX.to_string());
        assert_eq!(v["ts"], "-5");
        assert_eq!(v["position"], Value::Null);
        assert_eq!(v["condition"], 2);
        assert_eq!(v["p_lo"], u128::MAX.to_string());
        assert_eq!(v["pprime_hi"], 11);
        assert_eq!(v["vest_flushed"], "18");
        assert_eq!(v["keeper"], Pubkey::new_from_array([9; 32]).to_string());
        assert_eq!(
            <shortwin::events::SolSettle as Discriminator>::DISCRIMINATOR,
            idl()
                .events
                .iter()
                .find(|(n, _)| n == "SolSettle")
                .map(|(_, d)| d.as_slice())
                .expect("SolSettle")
        );
    }

    #[test]
    fn enum_arguments_and_byte_arrays() {
        let change = shortwin::state::ConfigChange::OracleFeed {
            account: Pubkey::new_from_array([3; 32]),
            feed_id: [0xab; 32],
        };
        let bytes = crate::ingest::decode::borsh_bytes(&change);
        let ix = idl().instruction("propose_config").expect("propose_config");
        let mut data = ix.discriminator.clone();
        data.extend_from_slice(&bytes);
        let args = idl()
            .decode_args(ix, &data[8..])
            .expect("args")
            .expect("some");
        assert_eq!(
            args["change"]["OracleFeed"]["feed_id"],
            "ab".repeat(32).as_str()
        );
        let set_pause = idl().instruction("set_pause").expect("set_pause");
        let mut body = vec![1u8, 1u8];
        let args = idl()
            .decode_args(set_pause, &body)
            .expect("args")
            .expect("some");
        assert_eq!(args["scope"], "Market");
        assert_eq!(args["paused"], true);
        body.push(0);
        assert!(
            idl().decode_args(set_pause, &body).is_err(),
            "trailing bytes"
        );
    }
}
