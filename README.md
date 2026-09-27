# NearPaid

**Launch a coin. Fees get paid.**

NearPaid is a token launchpad on [NEAR](https://near.org). Every coin launches with its full
1,000,000,000 supply in one locked [Rhea](https://rhea.finance) liquidity pool from the first
block. There is no bonding curve and no migration: the coin trades on Rhea, DexScreener and
GeckoTerminal from the moment it is created.

- Site: https://nearpaid.com
- X: https://x.com/Nearpaid
- Telegram: https://t.me/nearpaid

## What this repository contains

| Path | What it is |
|---|---|
| `contracts/token/` | Source of the coin contract every NearPaid launch deploys (a lean NEP-141 / NEP-145 fungible token with burn, optional holder rewards and the optional creator tax). Published on mainnet as a NEAR global contract. |
| `docs/` | Fee rules and integration notes for wallets, indexers and apps. |

The factory, locker, website and tooling are not part of this repository. This code is
published so that wallets, indexers and auditors can verify what a NearPaid coin does.

## Verify the on-chain coin code

Every NearPaid coin runs the same global contract. Its code hash on mainnet is
`ALZF4JurwxqkfB8aktDiSkFxggh4Ec5EpdaU9CfVKTNg`. Building `contracts/token` with the toolchain in
`contracts/token/BUILD.md` produces a wasm with that hash.

## Contracts (NEAR mainnet)

| Role | Account |
|---|---|
| Factory | `nearpaid.near` |
| Locker (zero access keys, holds every pool position) | `lock.nearpaid.near` |
| Coins | `<symbol>-<hex6>.nearpaid.near` |
| Treasury | `nearpaidprotocol.near` |
| Rhea DCL | `dclv2.ref-labs.near` |

## Fees, in short

Every buy and sell pays the pool's 1% fee: 0.2% of the trade to Rhea, 0.2% to NearPaid, and 0.6%
to the creator, a social handle or the holders on buys (paid in the pair asset) or burned on
sells. A creator can add an optional tax of up to 4% on top, split between burn, creator and
holders as set at launch. See `docs/FEES.md`.

## Risk notice

The NearPaid contracts have not been professionally audited. Coins launched on NearPaid are
created by their launchers, not by NearPaid. Nothing here is financial advice.

## License

See `LICENSE`. The source is published for verification and integration. It may not be copied
to run another launchpad.
