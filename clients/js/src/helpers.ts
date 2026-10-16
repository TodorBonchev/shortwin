/**
 * Hand-written helpers next to the generated client (never inside `src/generated/`), for what
 * the IDL cannot describe yet (M1 review, M1-FID-11):
 *
 * - `ShareEpoch` records (ARCH §2.3.6, A39): the five lender instructions take the records a
 *   lender is behind on as remaining accounts, but no M1 instruction names the account, so the
 *   IDL (and hence Codama) has no type, decoder or PDA for it. `findShareEpochPda`,
 *   `getShareEpochCodec` and `getShareEpochAccountMetas` fill the gap until the type reaches the
 *   IDL with `sol_settle` (M2); `test/helpers.test.ts` holds them to the Rust struct.
 * - `PodU128` / `PodI128` (ARCH §2.3, A67): the IDL describes them as 16-byte arrays, so the
 *   generated types are `readonly [ReadonlyUint8Array]`; `podU128ToBigInt` & co. convert.
 */
import {
    AccountRole,
    type AccountMeta,
    type Address,
    combineCodec,
    type FixedSizeCodec,
    type FixedSizeDecoder,
    type FixedSizeEncoder,
    fixDecoderSize,
    fixEncoderSize,
    getAddressDecoder,
    getAddressEncoder,
    getBytesDecoder,
    getBytesEncoder,
    getI128Decoder,
    getI128Encoder,
    getProgramDerivedAddress,
    getStructDecoder,
    getStructEncoder,
    getU128Decoder,
    getU128Encoder,
    getU32Decoder,
    getU32Encoder,
    getU8Decoder,
    getU8Encoder,
    type ProgramDerivedAddress,
    type ReadonlyUint8Array,
    transformEncoder,
} from '@solana/kit';

import { SHORTWIN_PROGRAM_ADDRESS } from './generated/programs/index.ts';
import type { PodI128, PodU128 } from './generated/types/index.ts';

// ---- PodU128 / PodI128 --------------------------------------------------------------------

const TWO_128 = 1n << 128n;

/** The value of a `PodU128` (16 little-endian bytes). */
export function podU128ToBigInt(pod: PodU128): bigint {
    return getU128Decoder().decode(pod[0]);
}

/** A `PodU128` holding `value` (0 ≤ value < 2¹²⁸). */
export function bigIntToPodU128(value: bigint): PodU128 {
    if (value < 0n || value >= TWO_128) throw new RangeError(`${value} is not a u128`);
    return [getU128Encoder().encode(value)];
}

/** The value of a `PodI128` (16 little-endian bytes, two's complement). */
export function podI128ToBigInt(pod: PodI128): bigint {
    return getI128Decoder().decode(pod[0]);
}

/** A `PodI128` holding `value` (−2¹²⁷ ≤ value < 2¹²⁷). */
export function bigIntToPodI128(value: bigint): PodI128 {
    if (value < -(TWO_128 >> 1n) || value >= TWO_128 >> 1n) throw new RangeError(`${value} is not an i128`);
    return [getI128Encoder().encode(value)];
}

// ---- ShareEpoch ---------------------------------------------------------------------------

/** Anchor's `sha256("account:ShareEpoch")[..8]`. */
export const SHARE_EPOCH_DISCRIMINATOR = new Uint8Array([0xcb, 0x67, 0xdd, 0x98, 0x1c, 0x10, 0xc9, 0x37]);

/** `ShareEpoch::SPACE`: the account size including the discriminator. */
export const SHARE_EPOCH_SIZE = 143;

/** A share-epoch record (`programs/shortwin/src/state/epoch.rs`; ARCH §2.3.6). */
export type ShareEpoch = {
    discriminator: ReadonlyUint8Array;
    market: Address;
    /** The epoch this record closes. */
    epoch: number;
    /** `acc` at the roll (topped up while it is a live full wipe). */
    accEnd: bigint;
    /** `acc_r` at the roll. */
    accREnd: bigint;
    /** Shares at the roll. */
    sOld: bigint;
    carry: bigint;
    carryR: bigint;
    /** 0 (full wipe) or 1 (rebase). */
    num: number;
    /** `k` of the denominator `1,000^k`. */
    denLog1000: number;
    bump: number;
    /**
     * Conversion remainder (KI-65): the old-epoch shares converted through this record so far,
     * modulo `1,000^k`. A lender behind the record converts to `⌊(convRem + shares)/1,000^k⌋`
     * shares and the lender instructions write it back, so a preview must use the value read.
     */
    convRem: bigint;
};

export type ShareEpochArgs = Omit<ShareEpoch, 'discriminator'>;

export function getShareEpochEncoder(): FixedSizeEncoder<ShareEpochArgs> {
    return transformEncoder(
        getStructEncoder([
            ['discriminator', fixEncoderSize(getBytesEncoder(), 8)],
            ['market', getAddressEncoder()],
            ['epoch', getU32Encoder()],
            ['accEnd', getI128Encoder()],
            ['accREnd', getU128Encoder()],
            ['sOld', getU128Encoder()],
            ['carry', getI128Encoder()],
            ['carryR', getU128Encoder()],
            ['num', getU8Encoder()],
            ['denLog1000', getU8Encoder()],
            ['bump', getU8Encoder()],
            ['convRem', getU128Encoder()],
        ]),
        (value: ShareEpochArgs) => ({ ...value, discriminator: SHARE_EPOCH_DISCRIMINATOR }),
    );
}

export function getShareEpochDecoder(): FixedSizeDecoder<ShareEpoch> {
    return getStructDecoder([
        ['discriminator', fixDecoderSize(getBytesDecoder(), 8)],
        ['market', getAddressDecoder()],
        ['epoch', getU32Decoder()],
        ['accEnd', getI128Decoder()],
        ['accREnd', getU128Decoder()],
        ['sOld', getU128Decoder()],
        ['carry', getI128Decoder()],
        ['carryR', getU128Decoder()],
        ['num', getU8Decoder()],
        ['denLog1000', getU8Decoder()],
        ['bump', getU8Decoder()],
        ['convRem', getU128Decoder()],
    ]);
}

export function getShareEpochCodec(): FixedSizeCodec<ShareEpochArgs, ShareEpoch> {
    return combineCodec(getShareEpochEncoder(), getShareEpochDecoder());
}

/** `["share_epoch", market, epoch u32 LE]` under short.win. */
export async function findShareEpochPda(
    seeds: { market: Address; epoch: number },
    config: { programAddress?: Address } = {},
): Promise<ProgramDerivedAddress> {
    const { programAddress = SHORTWIN_PROGRAM_ADDRESS } = config;
    return await getProgramDerivedAddress({
        programAddress,
        seeds: ['share_epoch', getAddressEncoder().encode(seeds.market), getU32Encoder().encode(seeds.epoch)],
    });
}

/**
 * The remaining accounts a lender instruction (`deposit`, `withdraw`, `claim_sol`,
 * `cover_deficit`, `sync_lender`) takes for a lender at `fromEpoch` in a market at
 * `shareEpoch`: the records `fromEpoch..shareEpoch`, in order, at most `limit` of them (ARCH
 * §3.3: a lender further behind is advanced with `sync_lender` first). Writable: the lender's
 * conversion through a rebase record writes the record's `conv_rem` (KI-65), and the program
 * refuses such a record passed read-only (`EpochRecordReadOnly`).
 */
export async function getShareEpochAccountMetas(
    input: { market: Address; fromEpoch: number; shareEpoch: number; limit?: number },
    config: { programAddress?: Address } = {},
): Promise<AccountMeta[]> {
    const { market, fromEpoch, shareEpoch, limit = 16 } = input;
    const metas: AccountMeta[] = [];
    for (let e = fromEpoch; e < shareEpoch && metas.length < limit; e++) {
        const [address] = await findShareEpochPda({ market, epoch: e }, config);
        metas.push({ address, role: AccountRole.WRITABLE });
    }
    return metas;
}
