---
sidebar_position: 52
title: Routstr — Wallet & Top-up
sidebar_label: Routstr (wallet)
description: Top up the Cashu wallet that pays for Routstr LLM requests
---

# Routstr Wallet — Top-up Guide

The [Routstr](./routstr.md) provider bills LLM usage in Bitcoin sats via
[Cashu](https://cashu.space/) ecash tokens. goose ships a `goose wallet`
subcommand that holds the sats Routstr spends — you fund it with a Cashu
token from any external wallet, and goose keeps the balance encoded as a
single token in `ROUTSTR_API_KEY` so the chat path can spend from it.

This guide walks through the wallet end-to-end:

- where the wallet lives and what it stores
- how `goose wallet topup` claims a Cashu token into your Routstr balance
- how `balance` and `withdraw` work, and what they do to `ROUTSTR_API_KEY`
- the refund-on-top-up flow that reclaims unspent sats from Routstr
- common errors and how to recover

For provider config (host switching, model listing) see the
[Routstr setup guide](./routstr.md). For the QA / regression matrix see the
[test scenario doc](./routstr-test-scenario.md).

## Where the wallet lives

| Path                          | What it is                                                |
| ----------------------------- | --------------------------------------------------------- |
| `~/.cdk-gooose/seed`          | BIP-39 mnemonic (12 words). **Back this up.**             |
| `~/.cdk-gooose/cdk-goose.redb` | Local Cashu proof store ([redb](https://github.com/cberner/redb)). |
| `~/.config/goose/config.yaml` | Holds `ROUTSTR_API_KEY` (the encoded ecash token).        |

On first use, `initialize_wallet` (`crates/goose-cli/src/commands/wallet.rs:204`)
creates `~/.cdk-gooose/`, generates a fresh BIP-39 seed, writes it to disk,
and opens the redb store. Every subsequent run loads the same seed — so
**deleting `~/.cdk-gooose/seed` is destructive**, the proofs in redb become
unrecoverable. If you need to migrate machines, copy the whole `.cdk-gooose/`
directory.

## Defaults

| Knob              | Default                              | How to change                            |
| ----------------- | ------------------------------------ | ---------------------------------------- |
| Mint              | `https://mint.minibits.cash/Bitcoin` | Currently a constant in `wallet.rs`. Topping up against a different mint requires a code change for now. |
| Currency          | `sat`                                | Hard-coded.                              |
| Wallet directory  | `~/.cdk-gooose/`                     | Hard-coded; uses the OS home dir.        |
| Routstr host      | `https://api.routstr.com`            | `ROUTSTR_HOST` (see [provider guide](./routstr.md)). |

The mint goose talks to needs to be the same mint your Routstr host trusts —
the public `api.routstr.com` accepts Minibits, but a self-hosted Routstr can
restrict to its own mint. If you switch hosts, double-check the mint match
before topping up; tokens minted at one mint are not redeemable at another.

## Topping up

### 1. Get a Cashu token from somewhere

You need an existing Cashu balance against the same mint goose uses
(`https://mint.minibits.cash/Bitcoin` by default). Easiest paths:

- [Minibits](https://www.minibits.cash/) (mobile, the same mint by default).
- [Cashu.me](https://wallet.cashu.me/) in a browser, pointed at the
  Minibits mint URL.
- A Lightning wallet that supports paying-to-mint (mint a Cashu token from a
  paid Lightning invoice, then export it).

Whatever you use, end up with a `cashuB...` (or `cashuA...`) token string.
That's your top-up token.

### 2. Run `goose wallet topup`

```sh
goose wallet topup cashuBpUiTHRzdGRJZA...
```

What this does, step by step (`handle_wallet_topup` in `wallet.rs:133`):

1. **Initializes the wallet.** Loads/creates the seed at `~/.cdk-gooose/seed`,
   opens redb, calls `recover_incomplete_sagas()` to release any proofs left
   `Reserved` from a crashed previous run.
2. **Refunds the previous Routstr token.** If `ROUTSTR_API_KEY` is set, goose
   POSTs the *current* token to `<ROUTSTR_HOST>/v1/wallet/refund`. Any sats
   the proxy hasn't yet debited come back as a fresh Cashu token, which goose
   immediately calls `wallet.receive` on. Then `ROUTSTR_API_KEY` is cleared
   so the same token can't double-spend.
3. **Receives your new top-up token.** `wallet.receive` validates the token
   with the mint and adds the proofs to redb.
4. **Consolidates.** `consolidate_proofs` calls `wallet.swap(balance, ...)`
   to repack the entire balance into one optimal set of denominations. (If
   the proofs are already optimal, swap returns `None` and goose falls back
   to the unspent set — same balance, fewer trips to the mint.)
5. **Writes a new `ROUTSTR_API_KEY`.** The consolidated proofs are encoded
   into a single Cashu token via `Token::new(...).to_string()` and saved as
   `ROUTSTR_API_KEY` in `~/.config/goose/config.yaml`.

After this finishes, `goose session start` (with `GOOSE_PROVIDER=routstr`)
spends from the new balance until either you withdraw or the proxy debits
the whole token.

### Empty / invalid input

```sh
goose wallet topup ""
# → "No token provided. Operation cancelled."
```

`topup` with an empty string is a no-op — the wallet isn't touched. A
malformed token surfaces as `Failed to claim change: <error>` from `cdk` and
the balance stays exactly where it was. Already-spent tokens behave the
same way.

## Checking the balance

```sh
goose wallet balance
```

`handle_wallet_balance` (`wallet.rs:20`) looks superficially like a read but
it actually performs the same refund + consolidate cycle as `topup`:

1. Open the wallet.
2. **Refund the existing `ROUTSTR_API_KEY`** against
   `<ROUTSTR_HOST>/v1/wallet/refund`, claim the change, clear the key.
3. Print `sats: <total balance>`.
4. Consolidate proofs to optimal denominations.
5. Write a fresh `ROUTSTR_API_KEY` encoding the new balance.

The reason every `balance` rotates the API key is so the next chat request
is guaranteed to spend a token that matches the actual on-chain proof set
(routstr requires the auth token to be a valid Cashu token, not just a
balance read-out). This is intentional, not a bug.

If you want a raw read-only check that doesn't rotate the key, query the
mint directly with `cdk-cli` against `~/.cdk-gooose/cdk-goose.redb` — but
that's a developer escape hatch, not a supported flow.

## Withdrawing

```sh
# drain everything
goose wallet withdraw

# withdraw exactly 250 sats and keep the rest
goose wallet withdraw 250
```

`handle_wallet_withdraw` (`wallet.rs:177`):

1. Print the *current* `ROUTSTR_API_KEY` to stdout (handy as a manual
   backup before the refund consumes it).
2. Refund + clear the API key, just like `balance`/`topup`.
3. Call `wallet.prepare_send(amount, SendOptions::default())` followed by
   `prep_send.confirm(None).await?` to mint a Cashu token of the requested
   amount.
4. Print the resulting `cashuB...` token to stdout.

Receive that printed token in any external Cashu wallet. If the wallet was
already empty before the withdraw call, you get `Wallet is empty.` and no
token — no panic, no partial state.

After `withdraw`, `ROUTSTR_API_KEY` is left **unset** until you next call
`balance` or `topup` — the chat path will fail with a
`Required ROUTSTR_API_KEY is not set` configure error, which is the
intended signal that you need to refill before sending another request.

## Refund-on-top-up explained

The single most surprising behavior in this wallet is that **every `topup`
and every `balance` first hits Routstr's `/v1/wallet/refund` endpoint to
reclaim any unspent sats from the previous token**. That's the
`handle_refund` function (`wallet.rs:91`).

Why: when goose sends a chat request, it hands the *whole* current
`ROUTSTR_API_KEY` token to Routstr as the auth header. Routstr debits the
cost of the request from that token and is supposed to leave the change
untouched on the proxy side until the next call. By POSTing the token to
`/v1/wallet/refund` before any wallet mutation, goose pulls those unspent
sats back into the redb store as a fresh Cashu token — so the new
balance/top-up is computed against the actual remaining sats, not stale
state.

If the refund fails (network, proxy down, token already redeemed), you get
`Failed to claim change: …` in tracing logs and goose continues. The
subsequent `wallet.receive` of your new top-up token still works, you just
forfeit whatever was left of the previous one. Since the previous balance
typically only persists across a single session, the loss is small.

To verify the refund worked, run with `RUST_LOG=debug`:

```sh
RUST_LOG=debug goose wallet topup cashuB...
# look for: "Claimed change from mint: <N> sats."
```

## Round-trip: top up, chat, refill

The expected day-to-day pattern:

```sh
# 1. Fund the wallet with sats from an external Cashu wallet.
goose wallet topup cashuBfromMinibits...

# 2. Start a session.
GOOSE_PROVIDER=routstr GOOSE_MODEL=anthropic/claude-sonnet-4 goose session start

# 3. Chat. Routstr debits sats per request. ROUTSTR_API_KEY stays the same
#    throughout the session — the proxy holds the change.

# 4. When you want a fresh balance read OR before the proxy spends the whole
#    token, refill / consolidate:
goose wallet balance        # refunds change, consolidates, rotates the key
# - or -
goose wallet topup cashuBfromMinibits...  # refunds + adds new sats
```

Routstr's billing is per-request, so a long session can drain the token
mid-conversation. If that happens you'll see
`ProviderError::InsufficientBalance(<sats>)` from goose with the missing
amount in the message — top up, then the next request goes through.

## Errors and recovery

| Symptom                                                             | Cause                                                                                                   | Fix                                                                                                                  |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| `Required ROUTSTR_API_KEY is not set`                               | Wallet is empty / never topped up; or you just ran `withdraw` to drain it.                              | `goose wallet topup <token>`.                                                                                        |
| `ProviderError::InsufficientBalance(<sats>)` during a chat          | Proxy debited the full token mid-session.                                                               | `goose wallet topup <token>`, retry the chat.                                                                        |
| `Failed to claim change: …` in logs                                 | `/v1/wallet/refund` was unreachable, or the previous token was already redeemed.                        | Continue if logs say `Claimed change from mint: 0 sats.` (nothing to claim). Otherwise check `ROUTSTR_HOST` and connectivity. |
| Topup with `cashuB...` returns the same balance with no error       | Token was already received once (Cashu proofs are single-spend).                                        | Use a fresh token from your external wallet.                                                                         |
| `401 / 403` from Routstr after a host switch                        | The new host doesn't trust the mint your wallet uses.                                                   | Top up against the host's expected mint; for now this requires editing `DEFAULT_MINT_URL` in `wallet.rs`.            |
| Wallet directory exists but `seed` is missing                       | Manual deletion or sync conflict.                                                                       | The redb proofs are no longer recoverable. Move the directory aside and start fresh.                                 |
| `goose wallet balance` shows a number but next chat says no balance | You ran `balance`/`topup`, then deleted `~/.config/goose/config.yaml` (or `ROUTSTR_API_KEY` from it).   | Re-run `goose wallet balance` to rewrite the key.                                                                    |

## Security notes

- The mnemonic at `~/.cdk-gooose/seed` controls every sat in the wallet.
  Treat it like any other seed phrase: back it up, don't paste it into
  config-shipping tools, and don't commit `.cdk-gooose/` to a repo.
- `ROUTSTR_API_KEY` is **not** stored in your OS keychain. It's a Cashu
  token in `~/.config/goose/config.yaml`, because the wallet has to read
  and rewrite it on every consolidate. If multiple users share a machine,
  the file permissions on `~/.config/goose/` are your only barrier.
- The Routstr proxy sees the full token on every request. If you don't
  trust the proxy, run a self-hosted Routstr instance and point
  `ROUTSTR_HOST` at it.
- `goose wallet withdraw` prints the resulting Cashu token to stdout. If
  you're piping the output anywhere, anyone reading the pipe can claim the
  sats — Cashu tokens are bearer instruments.
