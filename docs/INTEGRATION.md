# Integration notes

For wallets, indexers, aggregators and apps that want to list or read NearPaid coins.

## Discovering coins

Factory `nearpaid.near` (NEAR mainnet) views:

- `list_launches({ from_index, limit })` and `get_launch({ id })` - every launch, with `token`,
  `quote`, `pool_id`, `mode`, `recipient_key`, `tax`, `status`.
- `get_stats({ launch_id })` - fees burned, paid to the recipient, protocol share, tax paid out.
- `get_claimable({ key })` - what a recipient key (`near:<account>`, `x:<handle>`,
  `github:<handle>`, `twitch:<handle>`) can claim, per asset.
- `launches_by_creator({ account_id })`, `launches_by_recipient({ key })`.

Coin accounts are `<symbol>-<hex6>.nearpaid.near`. All coins share one global contract; verify
the code hash listed in the README.

## Pools

Every coin has exactly one Rhea DCL pool: `<token_x>|<token_y>|10000` where `token_x < token_y`
in byte order. DexScreener and GeckoTerminal address the same pool as
`refv2-<token_x>:<token_y>:10000`. The whole supply sits in one position owned by
`lock.nearpaid.near`, an account with zero access keys.

## Coin contract

Standard NEP-141 (`ft_transfer`, `ft_transfer_call`, `ft_total_supply`, `ft_balance_of`,
`ft_metadata`) and NEP-145 storage methods, plus:

- `burn({ amount })` - destroys the caller's own coins.
- `tax_info()` - `null` when the coin has no tax, otherwise the buy/sell rate and split in basis
  points and the tax burned so far.
- `rewards_info()`, `pending_rewards({ account_id })`, `claim_rewards()` - holder rewards, when
  enabled.

Events follow NEP-297 (`ft_mint`, `ft_transfer`, `ft_burn`). Tax burns carry the memo `tax`.

A coin with a tax deducts it from the coins that leave the pool on a buy and from the coins that
enter the pool on a sell. Rhea checks a buyer's minimum output before the tax, so quote the
after-tax amount to users by applying `buy_bps` from `tax_info`.

## Trading

Coins trade on Rhea from their first block. A direct link:
`https://app.rhea.finance/#near|<coin account>` (use the quote token's account instead of `near`
for USDC or stock pairs).
