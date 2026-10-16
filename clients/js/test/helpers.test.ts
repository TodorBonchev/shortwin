/**
 * The hand-written helpers (`src/helpers.ts`; M1 review M1-FID-11) against the program source:
 * the `ShareEpoch` codec follows the Rust struct field by field and has its asserted size and
 * Anchor discriminator, `findShareEpochPda` is the program's PDA, the remaining-account metas
 * list the records a lender is behind on, and the pod conversions round-trip. Also the Borsh
 * `LenderPosition` decoder's field order against the Rust struct (the zero-copy types are
 * `layout.test.ts`'s).
 */
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

import { AccountRole, address, getAddressEncoder, getProgramDerivedAddress } from '@solana/kit';
import { camelCase } from 'codama';

import * as client from '../src/index.ts';

const read = (path: string): string => readFileSync(new URL(`../../../${path}`, import.meta.url), 'utf8');

/** Field names of `pub struct <name>` in a Rust source, in order. */
function rustFields(src: string, name: string): string[] {
    const start = src.indexOf(`pub struct ${name} {`);
    assert.ok(start >= 0, `struct ${name}`);
    const body = src.slice(start, src.indexOf('\n}', start));
    return [...body.matchAll(/^\s+pub (\w+):/gm)].map((m) => camelCase(m[1]));
}

const anchorDiscriminator = (preimage: string): Uint8Array =>
    Uint8Array.from(createHash('sha256').update(preimage).digest().subarray(0, 8));

const MARKET = address('So11111111111111111111111111111111111111112');

test('ShareEpoch: field order, size 143 and discriminator follow epoch.rs', () => {
    const fields = rustFields(read('programs/shortwin/src/state/epoch.rs'), 'ShareEpoch');
    const record = {
        market: MARKET,
        epoch: 7,
        accEnd: -(1n << 100n),
        accREnd: (1n << 127n) + 5n,
        sOld: 1_000_000_000n,
        carry: -3n,
        carryR: 2n,
        num: 1,
        denLog1000: 2,
        bump: 254,
        convRem: 999_999n,
    };
    const bytes = client.getShareEpochEncoder().encode(record);
    const space = /assert!\(ShareEpoch::SPACE == ([\d_]+)\)/.exec(read('programs/shortwin/src/state/layout_asserts.rs'))?.[1];
    assert.equal(bytes.length, Number(space?.replaceAll('_', '')));
    assert.equal(client.SHARE_EPOCH_SIZE, bytes.length);
    assert.deepEqual(client.SHARE_EPOCH_DISCRIMINATOR, anchorDiscriminator('account:ShareEpoch'));
    assert.deepEqual(bytes.subarray(0, 8), client.SHARE_EPOCH_DISCRIMINATOR);
    const decoded = client.getShareEpochDecoder().decode(bytes);
    assert.deepEqual(Object.keys(decoded).slice(1), fields);
    assert.deepEqual({ ...decoded, discriminator: undefined }, { ...record, discriminator: undefined });
    // Borsh: the epoch right after the market key, then five 16-byte integers.
    assert.equal(new DataView(bytes.buffer, bytes.byteOffset).getUint32(40, true), 7);
    assert.equal(bytes[124], 1);
    assert.equal(bytes[126], 254);
    // KI-65: conv_rem is the last field, at ShareEpoch::CONV_REM_OFFSET, rewritten in place.
    const offset = /pub const CONV_REM_OFFSET: usize = ([\d_]+);/.exec(read('programs/shortwin/src/state/epoch.rs'))?.[1];
    assert.equal(Number(offset), 127);
    assert.equal(new DataView(bytes.buffer, bytes.byteOffset).getBigUint64(127, true), 999_999n);
    assert.equal(bytes.length - 16, 127);
});

test('findShareEpochPda and the remaining-account metas', async () => {
    const [pda] = await client.findShareEpochPda({ market: MARKET, epoch: 258 });
    const [expected] = await getProgramDerivedAddress({
        programAddress: client.SHORTWIN_PROGRAM_ADDRESS,
        seeds: ['share_epoch', getAddressEncoder().encode(MARKET), new Uint8Array([2, 1, 0, 0])],
    });
    assert.equal(pda, expected);
    const metas = await client.getShareEpochAccountMetas({ market: MARKET, fromEpoch: 3, shareEpoch: 6 });
    assert.equal(metas.length, 3);
    // Writable: a conversion through a rebase record writes its conv_rem (KI-65).
    assert.ok(metas.every((m) => m.role === AccountRole.WRITABLE));
    assert.equal(metas[0].address, (await client.findShareEpochPda({ market: MARKET, epoch: 3 }))[0]);
    assert.equal((await client.getShareEpochAccountMetas({ market: MARKET, fromEpoch: 0, shareEpoch: 40 })).length, 16);
});

test('PodU128 / PodI128 conversions round-trip', () => {
    for (const v of [0n, 1n, 1n << 64n, (1n << 128n) - 1n]) {
        assert.equal(client.podU128ToBigInt(client.bigIntToPodU128(v)), v);
    }
    for (const v of [0n, -1n, -(1n << 127n), (1n << 127n) - 1n, 12_345n]) {
        assert.equal(client.podI128ToBigInt(client.bigIntToPodI128(v)), v);
    }
    assert.throws(() => client.bigIntToPodU128(-1n));
    assert.throws(() => client.bigIntToPodU128(1n << 128n));
    assert.throws(() => client.bigIntToPodI128(1n << 127n));
    // Little-endian, as the program stores them.
    assert.deepEqual(client.bigIntToPodU128(258n)[0].subarray(0, 3), new Uint8Array([2, 1, 0]));
    const market = client.getMarketDecoder().decode(new Uint8Array(client.getMarketSize()));
    assert.equal(client.podU128ToBigInt(market.totalShares), 0n);
});

test('LenderPosition (Borsh, 241 B): the decoder follows lender.rs field by field', () => {
    const fields = rustFields(read('programs/shortwin/src/state/lender.rs'), 'LenderPosition');
    const zero = new Uint8Array(241);
    zero.set(client.LENDER_POSITION_DISCRIMINATOR, 0);
    const decoded = client.getLenderPositionDecoder().decode(zero);
    assert.deepEqual(Object.keys(decoded).slice(1), fields);
    assert.equal(client.getLenderPositionSize(), 241);
    // shares @72 (u128), pending @104 (i64), epoch @144 (u32), lock_until @200, bump @208.
    const data = Uint8Array.from(zero);
    const view = new DataView(data.buffer);
    view.setBigUint64(72, 5n, true);
    view.setBigInt64(104, -2n, true);
    view.setUint32(144, 9, true);
    view.setBigInt64(200, 86_400n, true);
    data[208] = 253;
    const l = client.getLenderPositionDecoder().decode(data);
    assert.deepEqual([l.shares, l.pending, l.epoch, l.lockUntil, l.bump], [5n, -2n, 9, 86_400n, 253]);
});
