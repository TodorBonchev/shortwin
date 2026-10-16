/**
 * The generated client agrees with the program source: program id, one builder per
 * instruction, the error codes in declaration order (ARCH §3.11), Anchor discriminators, PDA
 * defaults, and the Borsh `OracleCache` layout (ARCH §2.3.6) that `crank_sol_usd` writes.
 */
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

import { AccountRole, address, createNoopSigner, getAddressEncoder, getProgramDerivedAddress } from '@solana/kit';
import { camelCase, pascalCase, snakeCase } from 'codama';

import * as client from '../src/index.ts';

const read = (path: string): string => readFileSync(new URL(`../../../${path}`, import.meta.url), 'utf8');
const LIB_RS = read('programs/shortwin/src/lib.rs');
const ERRORS_RS = read('programs/shortwin/src/errors.rs');

const anchorDiscriminator = (preimage: string): Uint8Array =>
    Uint8Array.from(createHash('sha256').update(preimage).digest().subarray(0, 8));

const exported = (name: string): unknown => (client as Record<string, unknown>)[name];

test('program address is declare_id!', () => {
    const id = /declare_id!\("(\w+)"\)/.exec(LIB_RS)?.[1];
    assert.ok(id);
    assert.equal(client.SHORTWIN_PROGRAM_ADDRESS, id);
});

test('every #[program] instruction has builders and its Anchor discriminator', () => {
    const program = LIB_RS.slice(LIB_RS.indexOf('#[program]'));
    const names = [...program.matchAll(/pub fn (\w+)(?:<'info>)?\(\s*ctx: Context/g)].map((m) => m[1]);
    assert.ok(names.length >= 17, `found ${names.length} instructions in lib.rs`);
    for (const name of names) {
        const pascal = pascalCase(name);
        assert.equal(typeof exported(`get${pascal}Instruction`), 'function', `get${pascal}Instruction`);
        assert.equal(typeof exported(`parse${pascal}Instruction`), 'function', `parse${pascal}Instruction`);
        const constant = `${snakeCase(name).toUpperCase()}_DISCRIMINATOR`;
        assert.deepEqual(exported(constant), anchorDiscriminator(`global:${name}`), constant);
    }
});

test('error codes follow errors.rs declaration order from 6000', () => {
    const body = ERRORS_RS.slice(ERRORS_RS.indexOf('pub enum ShortwinError {'));
    const variants = [...body.slice(0, body.indexOf('\n}\n')).matchAll(/^ {4}([A-Z]\w*),$/gm)].map((m) => m[1]);
    assert.ok(variants.length >= 91, `found ${variants.length} variants`);
    variants.forEach((variant, i) => {
        const constant = `SHORTWIN_ERROR__${snakeCase(variant).toUpperCase()}`;
        assert.equal(exported(constant), 6000 + i, constant);
    });
    assert.equal(client.SHORTWIN_ERROR__ORACLE_STALE, 6087);
    assert.equal(client.getShortwinErrorMessage(client.SHORTWIN_ERROR__ORACLE_STEP), 'Oracle price step too large');
});

test('crank_sol_usd: PDA defaults, roles and data', async () => {
    const priceUpdate = address('7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE');
    const ix = await client.getCrankSolUsdInstructionAsync({ priceUpdate });
    const pda = async (seed: string) =>
        (await getProgramDerivedAddress({ programAddress: client.SHORTWIN_PROGRAM_ADDRESS, seeds: [seed] }))[0];
    assert.deepEqual(
        ix.accounts.map((a) => [a.address, a.role]),
        [
            [await pda('config'), AccountRole.READONLY],
            [await pda('sol_usd'), AccountRole.WRITABLE],
            [priceUpdate, AccountRole.READONLY],
        ],
    );
    assert.deepEqual(ix.data, anchorDiscriminator('global:crank_sol_usd'));
    assert.equal(client.identifyShortwinInstruction(ix.data), client.ShortwinInstruction.CrankSolUsd);
});

test('sync_lender (KI65-REV-SEC1): the owner is an either-signer, the lender PDA defaults from it', async () => {
    const market = address('So11111111111111111111111111111111111111112');
    const owner = createNoopSigner(address('7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE'));
    const [lender] = await client.findLenderPda({ market, owner: owner.address });
    const [marketVault] = await client.findMarketVaultPda({ market });
    const eventAuthority = (
        await getProgramDerivedAddress({ programAddress: client.SHORTWIN_PROGRAM_ADDRESS, seeds: ['__event_authority'] })
    )[0];
    const build = (o: typeof owner | typeof owner.address) =>
        client.getSyncLenderInstructionAsync({
            market,
            owner: o,
            eventAuthority,
            program: client.SHORTWIN_PROGRAM_ADDRESS,
            maxEpochs: 8,
        });
    const roles = async (o: typeof owner | typeof owner.address) =>
        (await build(o)).accounts.map((a) => [a.address, a.role]);
    const rest = [
        [eventAuthority, AccountRole.READONLY],
        [client.SHORTWIN_PROGRAM_ADDRESS, AccountRole.READONLY],
    ];
    const head = [
        [market, AccountRole.READONLY],
        [marketVault, AccountRole.WRITABLE],
        [lender, AccountRole.WRITABLE],
    ];
    // A third party (the owner's address): a plain catch-up; the owner's signer: any conversion.
    assert.deepEqual(await roles(owner.address), [...head, [owner.address, AccountRole.READONLY], ...rest]);
    assert.deepEqual(await roles(owner), [...head, [owner.address, AccountRole.READONLY_SIGNER], ...rest]);
    assert.equal(client.parseSyncLenderInstruction(await build(owner)).accounts.owner.address, owner.address);
    assert.equal(client.SHORTWIN_ERROR__REBASE_NEEDS_OWNER, 6107);
});

test('set_coverage_pause (O17): the keeper key signs, the config PDA defaults, the market is writable', async () => {
    const coverageAuthority = createNoopSigner(address('7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE'));
    const market = address('So11111111111111111111111111111111111111112');
    const pda = async (seed: string) =>
        (await getProgramDerivedAddress({ programAddress: client.SHORTWIN_PROGRAM_ADDRESS, seeds: [seed] }))[0];
    const eventAuthority = await pda('__event_authority');
    const ix = await client.getSetCoveragePauseInstructionAsync({
        coverageAuthority,
        market,
        eventAuthority,
        program: client.SHORTWIN_PROGRAM_ADDRESS,
        paused: true,
        reason: 1,
    });
    assert.deepEqual(
        ix.accounts.map((a) => [a.address, a.role]),
        [
            [coverageAuthority.address, AccountRole.READONLY_SIGNER],
            [await pda('config'), AccountRole.READONLY],
            [market, AccountRole.WRITABLE],
            [eventAuthority, AccountRole.READONLY],
            [client.SHORTWIN_PROGRAM_ADDRESS, AccountRole.READONLY],
        ],
    );
    assert.deepEqual(ix.data, Uint8Array.from([...anchorDiscriminator('global:set_coverage_pause'), 1, 1]));
    assert.equal(client.identifyShortwinInstruction(ix.data), client.ShortwinInstruction.SetCoveragePause);
    const parsed = client.parseSetCoveragePauseInstruction(ix);
    assert.deepEqual([parsed.data.paused, parsed.data.reason], [true, 1]);
    // Inert while unset (ARCH §3.1): the existing Admin-group code.
    assert.equal(client.SHORTWIN_ERROR__COVERAGE_AUTHORITY_DISABLED, 6015);
    assert.deepEqual(client.COVERAGE_PAUSE_SET_EVENT_DISCRIMINATOR, anchorDiscriminator('event:CoveragePauseSet'));
    // The event names its signer (`by`, as `PauseSet`).
    const ev = { market, slot: 1n, ts: 2n, paused: true, reason: 1, by: coverageAuthority.address };
    const bytes = client.getCoveragePauseSetEventEncoder().encode(ev);
    assert.equal(bytes.length, 8 + 32 + 8 + 8 + 1 + 1 + 32);
    assert.deepEqual(bytes.slice(0, 8), anchorDiscriminator('event:CoveragePauseSet'));
    assert.deepEqual(client.getCoveragePauseSetEventDecoder().decode(bytes), ev);
});

test('OracleCache (Borsh, 45 B): price @8, conf @16, expo @24, publish_time @28, updated_slot @36, bump @44', () => {
    const data = new Uint8Array(45);
    const view = new DataView(data.buffer);
    data.set(anchorDiscriminator('account:OracleCache'), 0);
    view.setBigInt64(8, 12_179_606_511n, true);
    view.setBigUint64(16, 1_399_451n, true);
    view.setInt32(24, -8, true);
    view.setBigInt64(28, 1_790_523_057n, true);
    view.setBigUint64(36, 451_037_803n, true);
    data[44] = 254;
    const cache = client.getOracleCacheDecoder().decode(data);
    assert.deepEqual(
        { ...cache, discriminator: undefined },
        {
            discriminator: undefined,
            price: 12_179_606_511n,
            conf: 1_399_451n,
            expo: -8,
            publishTime: 1_790_523_057n,
            updatedSlot: 451_037_803n,
            bump: 254,
        },
    );
    assert.deepEqual(client.getOracleCacheEncoder().encode(cache), data);
    assert.deepEqual(client.ORACLE_CACHE_DISCRIMINATOR, anchorDiscriminator('account:OracleCache'));
});

test('account discriminators are Anchor’s', () => {
    for (const name of ['GlobalConfig', 'VenueGuard', 'VenueExtras', 'Market', 'MarketVault', 'LenderPosition', 'OracleCache']) {
        const constant = `${snakeCase(name).toUpperCase()}_DISCRIMINATOR`;
        assert.deepEqual(exported(constant), anchorDiscriminator(`account:${name}`), constant);
    }
    // PDA seeds with a key: the market of a mint.
    const mint = address('So11111111111111111111111111111111111111112');
    return client.findMarketPda({ mint }).then(async ([market]) => {
        const [expected] = await getProgramDerivedAddress({
            programAddress: client.SHORTWIN_PROGRAM_ADDRESS,
            seeds: ['market', getAddressEncoder().encode(mint)],
        });
        assert.equal(market, expected);
        assert.equal(camelCase('market_vault'), 'marketVault');
    });
});
