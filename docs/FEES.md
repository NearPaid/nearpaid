# NearPaid fees

All shares below are a percentage **of the trade**.

## Pool fee (every coin, every trade)

Each NearPaid coin trades in a Rhea DCL pool with a 1% fee tier.

| Share of the trade | On buys | On sells |
|---|---|---|
| 0.2% | Rhea | Rhea |
| 0.6% | creator, social handle or holders (paid in the pair asset) | burned in the coin |
| 0.2% | NearPaid protocol | NearPaid protocol (converted to the pair asset by the factory) |

The split is fixed in the factory contract and cannot be changed.

## Creator tax (optional)

At launch the creator may set one tax rate, 0% to 4%, charged on every buy and sell on top of
the pool fee. Wallet-to-wallet transfers are never taxed. The collected tax is split as chosen at
launch between:

- **Burn** - destroyed immediately (`ft_burn` event, memo `tax`).
- **Creator** - converted by the factory into the pair asset and credited to the creator or the
  named social handle.
- **Holders** - converted the same way and shared across holders pro-rata.

NearPaid takes nothing from the tax.

Apps that quote a trade from the pool alone (Rhea, aggregators) show the amount **before** the
coin's tax. A coin with a 2% tax delivers about 2% less than such a quote. Read the coin's
`tax_info` view to show the exact amount.

## Launch cost

A 0.5 NEAR launch fee plus the storage the launch uses (about 0.1 to 0.3 NEAR, depending on the
logo). The site shows the exact total before signing.

## Where fees move

Fees sit in the pool position until `collect` runs on the factory. Anyone may call it. The site
runs it together with every trade and with the "Burn now" button, so no bot is needed.
