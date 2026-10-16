# Keeper test data

Mainnet accounts read by `shortwin-build-alt` (`src/alt.rs`), dumped with `getAccountInfo`
(base64) from `https://api.mainnet-beta.solana.com`:

| File | Account | Owner | Bytes | Slot |
|---|---|---|---|---|
| `pump_global.bin` | pump `Global` `4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf` | pump `6EF8rr…` | 1,087 | 451,215,530 |
| `pamm_global_config.bin` | PumpSwap `GlobalConfig` `ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw` | pAMM `pAMMBay6…` | 949 | 451,215,534 |

The unit tests decode the fee and buyback recipient lists (pump `fee_recipient` @41,
`fee_recipients[7]` @162, `buyback_fee_recipients[8]` @741; PumpSwap `protocol_fee_recipients[8]`
@57, `buyback_fee_recipients[8]` @643; `docs/research/pumpfun-bonding-curve.md` §2.2,
`docs/research/graduated-venues.md` §2) and compare them with the lists those documents record.
`sha256sum -c SHA256SUMS` checks the files.
