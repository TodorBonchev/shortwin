/**
 * T-LAYOUT, Codama leg (ARCHITECTURE §2.3, §8.2): the generated decoders read every field of
 * every zero-copy type at the offset `programs/shortwin/src/state/layout_asserts.rs` asserts
 * at compile time on sbpfv3, x86_64 and wasm32, and every account has the asserted size.
 *
 * The test parses the `layout!(acct|nested Type, SIZE, { field @ offset, … })` blocks and the
 * `Type::SPACE == N` asserts of that file, so a field added there is checked here with no
 * edit. For each field it encodes the all-zero value, perturbs only that field and asserts
 * that the first byte that changes is the field's offset (and that the value round-trips).
 */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

import { getAddressDecoder } from '@solana/kit';
import { camelCase } from 'codama';

import * as client from '../src/index.ts';

const LAYOUT_ASSERTS = new URL('../../../programs/shortwin/src/state/layout_asserts.rs', import.meta.url);

/** Types `layout_asserts.rs` sizes that are not in the IDL yet: no instruction names them as an
 * account (they appear with their first named use). `ShortPosition` and `WalletExposure` arrived
 * with M2's `open_short`; `ShareEpoch` arrives with `sol_settle`'s record (today a remaining
 * account or an unchecked one). */
const NOT_IN_IDL_YET = new Set(['ShareEpoch']);

type Layout = { kind: 'acct' | 'nested'; name: string; size: number; fields: Array<[string, number]> };
type Codec = {
    encode(value: unknown): Uint8Array;
    decode(bytes: Uint8Array): Record<string, unknown>;
    fixedSize?: number;
};

const num = (s: string): number => Number(s.replaceAll('_', ''));

function parseLayouts(src: string): Layout[] {
    const out: Layout[] = [];
    for (const m of src.matchAll(/layout!\((acct|nested)\s+(\w+),\s*([\d_]+),\s*\{([^}]*)\}\);/g)) {
        const body = m[4].replace(/\/\/[^\n]*/g, '');
        const fields = [...body.matchAll(/(\w+)\s*@\s*([\d_]+)/g)].map((f): [string, number] => [f[1], num(f[2])]);
        out.push({ kind: m[1] as Layout['kind'], name: m[2], size: num(m[3]), fields });
    }
    return out;
}

function parseSpaces(src: string): Map<string, number> {
    return new Map([...src.matchAll(/assert!\((\w+)::SPACE == ([\d_]+)\)/g)].map((m) => [m[1], num(m[2])]));
}

const OTHER_ADDRESS = getAddressDecoder().decode(new Uint8Array(32).fill(0xab));

/** The same value with its first encoded byte changed (numbers +1, bytes ^1, recursively). */
function perturb(v: unknown): unknown {
    if (typeof v === 'bigint') return v + 1n;
    if (typeof v === 'number') return v + 1;
    if (typeof v === 'boolean') return !v;
    if (typeof v === 'string') return OTHER_ADDRESS;
    if (v instanceof Uint8Array) {
        const out = Uint8Array.from(v);
        out[0] ^= 1;
        return out;
    }
    if (Array.isArray(v)) return [perturb(v[0]), ...v.slice(1)];
    if (v !== null && typeof v === 'object') {
        const [first] = Object.keys(v);
        return { ...(v as Record<string, unknown>), [first]: perturb((v as Record<string, unknown>)[first]) };
    }
    throw new Error(`cannot perturb ${String(v)}`);
}

function codecOf(name: string): Codec | undefined {
    const factory = (client as Record<string, unknown>)[`get${name}Codec`];
    return typeof factory === 'function' ? (factory() as Codec) : undefined;
}

function firstDifference(a: Uint8Array, b: Uint8Array): number {
    assert.equal(a.length, b.length);
    return a.findIndex((byte, i) => byte !== b[i]);
}

const source = readFileSync(LAYOUT_ASSERTS, 'utf8');
const layouts = parseLayouts(source);
const spaces = parseSpaces(source);

test('layout_asserts.rs parses', () => {
    assert.ok(layouts.length >= 15, `only ${layouts.length} layout! blocks found`);
    for (const name of ['GlobalConfig', 'VenueGuard', 'VenueExtras', 'Market', 'MarketVault', 'Params']) {
        assert.ok(layouts.some((l) => l.name === name), `${name} missing from layout_asserts.rs`);
    }
    assert.equal(spaces.get('OracleCache'), 45);
});

for (const layout of layouts) {
    test(`${layout.name}: size ${layout.size} and ${layout.fields.length} field offsets`, () => {
        const codec = codecOf(layout.name);
        assert.ok(codec, `the client has no codec for ${layout.name}`);
        assert.equal(codec.fixedSize, layout.size, `${layout.name} size`);
        if (layout.kind === 'acct') {
            const size = (client as Record<string, unknown>)[`get${layout.name}Size`];
            assert.equal(typeof size === 'function' ? size() : undefined, layout.size);
        }
        const zero = codec.decode(new Uint8Array(layout.size));
        const base = codec.encode(zero);
        assert.equal(base.length, layout.size);
        for (const [rustName, offset] of layout.fields) {
            const field = camelCase(rustName);
            assert.ok(field in zero, `${layout.name}.${rustName} (${field}) is not a decoded field`);
            const changed = { ...zero, [field]: perturb(zero[field]) };
            const bytes = codec.encode(changed);
            assert.equal(firstDifference(base, bytes), offset, `${layout.name}.${rustName} offset`);
            assert.deepEqual(codec.decode(bytes)[field], changed[field], `${layout.name}.${rustName} round trip`);
        }
    });
}

test('every asserted account size matches the client', () => {
    for (const [name, space] of spaces) {
        const size = (client as Record<string, unknown>)[`get${name}Size`];
        if (typeof size !== 'function') {
            assert.ok(NOT_IN_IDL_YET.has(name), `the client has no account ${name}`);
            continue;
        }
        assert.equal(size(), space, `${name} size`);
    }
});
