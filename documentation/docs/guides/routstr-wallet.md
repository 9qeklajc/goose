---
sidebar_position: 52
title: Routstr — Wallet & Top-up
sidebar_label: Routstr (wallet)
description: Move sats between the local Cashu wallet and the Routstr proxy
---

# Routstr Wallet — Top-up Guide

`goose wallet …` manages a single shared local
[Cashu](https://cashu.space/) wallet. `goose routstr …` moves sats from
that wallet onto a Routstr proxy and back. This page walks through both
sides.

For provider config (multi-profile setup, host switching, model listing)
see the [Routstr setup guide](./routstr.md). For the QA / regression
matrix see the [test scenario doc](./routstr-test-scenario.md).

## Two wallets, two layers

There are two places sats can live:

1. **Local Cashu wallet** at `~/.cdk-gooose/`. One BIP-39 seed, one redb
   proof store, one mint (Minibits, hardcoded). Funded with `goose wallet
   topup <cashuB...>`. Not visible to any Routstr proxy.
2. **Routstr-tracked balance** behind an `sk-...` API key, one per
   profile, on the Routstr instance you registered. Funded with
   `goose routstr topup` (which drains some local sats, exchanges them
   for proxy balance via `/v1/balance/create` or `/v1/balance/topup`,
   and stores the resulting `sk-...` key under the profile in
   `~/.config/goose/config.yaml`).

Cashu tokens are bearer instruments. They only briefly cross the wire:
external wallet → local wallet → drained on top-up → handed to the proxy
→ kept on the proxy until refund → returned to local wallet on refund.
**Goose never stores raw Cashu tokens long-term.**

## Filesystem layout

| Path | Holds |
| --- | --- |
| `~/.cdk-gooose/seed` | BIP-39 mnemonic (12 words). **Back this up.** |
| `~/.cdk-gooose/cdk-goose.redb` | Local Cashu proof store ([redb](https://github.com/cberner/redb)). |
| `~/.config/goose/config.yaml` (`ROUTSTR_PROFILES`) | Per-profile `{url, api_key}` map. The `api_key` is `sk-...`, **not** a Cashu token. |
| `~/.config/goose/config.yaml` (`ROUTSTR_ACTIVE`) | Which profile name is active. |

The wallet directory is created on first use (`goose wallet topup` or
`goose wallet balance`), with a fresh BIP-39 seed if none exists. **Don't
delete `seed` without first migrating the redb proofs** — they become
unspendable without it.

## Local wallet operations (`goose wallet`)

Pure CDK — no Routstr involvement.

```sh
goose wallet topup cashuB...     # receive a token, add proofs to local
goose wallet balance             # local balance + mint URL
goose wallet withdraw 500        # mint a 500-sat Cashu token (printed to stdout)
goose wallet withdraw            # drain the entire wallet to a Cashu token
```

`withdraw` prints the resulting token on stdout. Anyone with that string
can claim the sats — Cashu tokens are bearer instruments, treat the
output like a one-time-use private key.

## Routstr-side operations (`goose routstr`)

These connect the local wallet to a profile's tracked balance.

```sh
goose routstr balance            # local + per-profile balance summary
goose routstr topup [N]          # drain N sats (default 2000) → active profile
goose routstr refund             # drain active profile's balance → local
goose routstr profile add NAME --url URL
goose routstr profile list
goose routstr profile use NAME   # refund old + activate new + auto-topup new
goose routstr profile remove NAME
```

### Step-by-step: `goose routstr topup [N]`

1. Open the local wallet, check it has at least 1 sat.
2. Mint a `min(N, local_balance)`-sat Cashu token from the local wallet
   (`prepare_send + confirm`).
3. If the active profile has no `api_key` yet:
   `GET <profile.url>/v1/balance/create?initial_balance_token=<cashu>`.
   The proxy returns `{api_key: "sk-...", balance: <mSats>}`. Store the
   `sk-...` under the profile.
4. If the active profile already has an `api_key`:
   `POST <profile.url>/v1/balance/topup?cashu_token=<cashu>` with
   `Authorization: Bearer sk-...`. The proxy adds the new sats to the
   tracked balance.
5. Print local + proxy balance.

### Step-by-step: `goose routstr refund`

1. POST `<active.url>/v1/balance/refund` with the active `sk-...`. Proxy
   returns `{token: "cashuB...", sats: "<N>"}` (Routstr's response uses a
   stringified `sats` field on some instances; goose accepts both string
   and integer forms).
2. Receive the returned token into the local wallet.
3. Clear the active profile's `api_key`.

### Step-by-step: `goose routstr profile use <name>`

1. Run `refund` against the previously active profile (best-effort — log
   warning on failure, switch anyway).
2. Set `ROUTSTR_ACTIVE = <name>` in config.
3. If the new profile has < 2000 sats tracked balance (or no `sk-...`
   yet), drain `min(2000, local_balance)` sats from the local wallet and
   `topup` the new profile.

## Round-trip example

```sh
goose wallet topup cashuB...        # receive 5000 sats from external wallet
goose routstr profile add otrta --url https://routstr.otrta.me
goose routstr topup 2000            # otrta gets sk-... with 2000 sats
goose run --provider routstr --model glm-5.1 --text "hi"
goose routstr balance               # otrta has ~1976 sats now (24 spent on chat)

goose routstr profile add upstream --url https://api.routstr.com
goose routstr profile use upstream  # refunds otrta to local, auto-tops upstream
                                    # local: 1976 + 3000 = 4976 sats - 2000 = 2976
                                    # upstream: 2000 sats
```

## Errors and recovery

| Symptom | Cause | Fix |
| --- | --- | --- |
| `Local Cashu wallet is empty.` | Local proof store has 0 sats. | `goose wallet topup <cashu-token>`. |
| `Active routstr profile <name> has no api_key yet.` | Profile registered but never funded. | `goose routstr topup`. |
| `routstr balance api returned HTTP 4xx: ...` from topup/create | The Cashu token is from a mint the proxy doesn't trust, or the token has already been spent (Cashu single-spend). | Mint a fresh token from a wallet pointed at the correct mint. |
| `refund <name> failed: ...` during `profile use` | Old profile's proxy unreachable, or its `api_key` already consumed. | Switch back with `goose routstr profile use <old>` to retry the refund. |
| `Insufficient balance: <N> sats required` during chat | Active profile has < the model's per-request minimum. | `goose routstr topup` (default 2000 sats); for high-min models pass an explicit larger amount. |
| `Database already open. Cannot acquire lock.` from a wallet command | Two redb connections to the same file from a single process. | This was a goose bug fixed in early profile testing. If you see it after a clean rebuild, file an issue with reproduction steps. |
| Lost a Cashu token mid-flight (e.g. refund response with the token visible in a panic message) | Manually copy the `cashuB...` string from the error and run `goose wallet topup <token>` to redeem it. | Keep terminal scrollback during refund operations. |

## Security notes

- `~/.cdk-gooose/seed` controls every sat in the local wallet. Treat it
  like any other seed phrase.
- `ROUTSTR_PROFILES` in `~/.config/goose/config.yaml` stores `sk-...`
  bearer keys in plaintext. Anyone with read access to that file can
  authenticate as you against the listed Routstr instances.
- `goose wallet withdraw` prints the resulting Cashu token to stdout. If
  you're piping the output anywhere, treat the pipe like a one-time-use
  private key — anyone reading it can claim the sats.
- Cashu tokens are single-spend. Goose never re-sends a token after
  receiving it; if you accidentally pipe the same `cashuB...` to
  `wallet topup` twice, the second receive will error and the proofs
  stay where they were.
