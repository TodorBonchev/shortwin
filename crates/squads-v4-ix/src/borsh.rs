//! The few Borsh shapes Squads v4's instructions and accounts use, written and read by hand:
//! little-endian integers, `bool` as one byte, `Option<T>` as a 0/1 tag, `Vec<T>` / `bytes` /
//! `String` with a `u32` length, fixed arrays and public keys as raw bytes.

use solana_pubkey::Pubkey;

/// An append-only Borsh writer.
#[derive(Default, Debug)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    /// A writer starting with `prefix` (an instruction discriminator).
    pub fn with(prefix: &[u8]) -> Self {
        Self(prefix.to_vec())
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }

    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(u8::from(v))
    }

    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn raw(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }

    pub fn pubkey(&mut self, k: &Pubkey) -> &mut Self {
        self.raw(k.as_ref())
    }

    /// A `u32` length prefix (Borsh `Vec` / `bytes` / `String`).
    ///
    /// # Panics
    /// If `len` exceeds `u32::MAX` (no Squads argument comes near it).
    pub fn len(&mut self, len: usize) -> &mut Self {
        self.u32(u32::try_from(len).expect("Borsh length exceeds u32"))
    }

    /// `bytes` / `Vec<u8>`.
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.len(v.len()).raw(v)
    }

    pub fn option_pubkey(&mut self, v: Option<&Pubkey>) -> &mut Self {
        match v {
            Some(k) => self.u8(1).pubkey(k),
            None => self.u8(0),
        }
    }

    pub fn option_string(&mut self, v: Option<&str>) -> &mut Self {
        match v {
            Some(s) => self.u8(1).bytes(s.as_bytes()),
            None => self.u8(0),
        }
    }

    pub fn pubkeys(&mut self, v: &[Pubkey]) -> &mut Self {
        self.len(v.len());
        for k in v {
            self.pubkey(k);
        }
        self
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }
}

/// A Borsh reader over an account's data; every read fails (`None`) past the end.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    /// Bytes consumed so far.
    pub fn position(&self) -> usize {
        self.at
    }

    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.data.get(self.at..end)?;
        self.at = end;
        Some(s)
    }

    pub fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    pub fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    pub fn bool(&mut self) -> Option<bool> {
        match self.u8()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    pub fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.array()?))
    }

    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.array()?))
    }

    pub fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.array()?))
    }

    pub fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.array()?))
    }

    pub fn pubkey(&mut self) -> Option<Pubkey> {
        Some(Pubkey::new_from_array(self.array()?))
    }

    pub fn option_pubkey(&mut self) -> Option<Option<Pubkey>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.pubkey()?)),
            _ => None,
        }
    }

    /// A `u32`-prefixed length, refused when it cannot fit in what is left (`min_item` bytes
    /// per element).
    pub fn len(&mut self, min_item: usize) -> Option<usize> {
        let n = usize::try_from(self.u32()?).ok()?;
        let left = self.data.len().checked_sub(self.at)?;
        (n.checked_mul(min_item)? <= left).then_some(n)
    }

    pub fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.len(1)?;
        self.take(n)
    }

    pub fn pubkeys(&mut self) -> Option<Vec<Pubkey>> {
        let n = self.len(32)?;
        (0..n).map(|_| self.pubkey()).collect()
    }
}
