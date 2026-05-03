---
sidebar_position: 50
title: Routstr (Cashu-paid LLM proxy)
sidebar_label: Routstr
description: Use the Routstr Cashu-paid LLM proxy with goose, manage multiple proxy profiles, and switch between them
---

# Routstr

[Routstr](https://routstr.com/docs) is an OpenAI-compatible LLM proxy that
bills per request in Bitcoin sats via [Cashu](https://cashu.space/) ecash
tokens. goose ships:

- a `routstr` provider that talks to any Routstr instance via a per-profile
  `sk-...` API key
- a single shared local Cashu wallet (`goose wallet …`) that holds your sats
- a `goose routstr …` subcommand that manages multiple Routstr **profiles**
  and moves sats between the local wallet and the proxy

This guide walks the full setup, the multi-profile workflow, and the
auto-refund-on-switch flow. For the wallet internals see the
[wallet guide](./routstr-wallet.md); for the QA matrix see the
[test scenario doc](./routstr-test-scenario.md).

## Mental model — three pieces

1. **The local Cashu wallet** (`~/.cdk-gooose/`). One BIP-39 seed, one redb
   proof store, one mint (Minibits, hardcoded). This is your source of
   truth for sats. Every Routstr top-up drains some of these sats; every
   Routstr refund deposits some back.
2. **A Routstr profile.** A name, a URL, and (once funded) an `sk-...`
   API key the proxy issued in exchange for a Cashu token. You can have
   any number of profiles — `otrta`, `upstream`, `self-hosted`, … — and
   switch between them.
3. **Active profile pointer** (`ROUTSTR_ACTIVE`). Names whichever profile
   should answer the next chat request. `goose routstr profile use <name>`
   moves it.

The Cashu token you minted in an external wallet **never lives in goose's
config** — it's redeemed into the local wallet on `goose wallet topup`,
and from there can be split per-profile by `goose routstr topup`.

## End-to-end quickstart

```sh
# 1. Get a Cashu token from any external Cashu wallet (Minibits, Cashu.me).
#    Receive it into your local goose wallet:
goose wallet topup cashuB...

# 2. Pick the Routstr instance and a model in one go:
goose configure                # → Configure Providers → Routstr
                               # → Routstr URL: https://api.routstr.com
                               # → pick a model from the proxy's catalogue

# 3. Fund the profile from your local wallet (default 2000 sats):
goose routstr topup            # or `goose routstr topup 500` for a custom amount

# 4. Chat:
goose run --provider routstr --model anthropic/claude-sonnet-4 \
  --text "hi from a Cashu-paid wallet"
```

`goose configure → Configure Providers → Routstr` is the single entry
point for setup:

- Asks for the Routstr URL (defaults to the active profile's URL, or
  `https://api.routstr.com` if no profile is active).
- If the URL **matches an existing profile** → switches to it (refunds
  whatever is in the previously active profile back to your local
  Cashu wallet first).
- If the URL **doesn't match any profile** → refunds the previously
  active profile, creates a new profile named `default` pointing at the
  new URL, and makes it active.
- Then fetches `<active-url>/v1/models` and presents the picker. The
  list is whatever **that** instance serves — instances disagree on
  which models they expose, and `goose configure` always uses the URL
  the user just confirmed.

Behind the scenes, the chat path uses the active profile's `sk-` key as
the `Authorization: Bearer …` header.

## Multiple profiles

Add as many as you want:

```sh
goose routstr profile add otrta       --url https://routstr.otrta.me
goose routstr profile add self-hosted --url https://routstr.example.internal
goose routstr profile list
```

```text
active    name                       url                                       balance
  *       default                    https://api.routstr.com                   1923 sats (1923000 mSats)
          otrta                      https://routstr.otrta.me                  (no api_key yet)
          self-hosted                https://routstr.example.internal          (no api_key yet)
```

Each profile keeps its own `sk-` key, balance, request count, etc. on the
respective proxy.

## Switching — what `goose routstr profile use <name>` actually does

```sh
goose routstr profile use otrta
```

Three things, in order:

1. **Refund the active profile.** POST `<active-url>/v1/balance/refund`
   with the active `sk-` key. The proxy returns a Cashu token encoding all
   unspent sats. Goose redeems that token into the **local wallet** and
   clears the `sk-` key from the old profile (it's now consumed).
2. **Flip `ROUTSTR_ACTIVE`** to the new profile name.
3. **Auto-topup the new profile.** If the new profile's tracked balance is
   less than 2000 sats (or it has no `sk-` yet), goose drains
   `min(2000 sats, local-wallet-balance)` from the local wallet and
   exchanges it for a fresh `sk-` on the new proxy.

The end state: previous profile is empty, new profile is funded, your
local wallet absorbed any unspent change. If the refund call fails (proxy
down, key already consumed, etc.) goose logs a warning and proceeds with
the switch — re-run `goose routstr profile use <old>` later to retry the
refund.

```text
$ goose routstr profile use otrta
✓ refunded 976 sats from "default" into local wallet
✓ active routstr profile is now "otrta"
✓ created api_key for "otrta" with 976 sats (976000 mSats) initial balance
  local wallet: 0 sats (976 sats sent to proxy)
```

## Manual top-up / refund (without switching)

```sh
goose routstr topup            # active profile, +2000 sats from local wallet
goose routstr topup 500        # active profile, +500 sats
goose routstr refund           # drain the active profile's balance back to local
```

`refund` is also useful before swapping mints — drain the active profile
first, then reconfigure goose's local wallet (manual today; see the
[Limitations](#limitations) note).

## Removing a profile

```sh
goose routstr profile remove self-hosted
```

Goose calls `/v1/balance/refund` first so any unspent sats come back to
the local wallet, then drops the profile from config. If the refund
fails (proxy unreachable), the profile is dropped anyway with a warning;
the api_key is logged so you can manually refund later.

## How the proxy-side bits work

| Goose action | Proxy endpoint | Effect |
| --- | --- | --- |
| `goose routstr topup` (first time) | `GET /v1/balance/create?initial_balance_token=<cashu>` | Proxy mints an `sk-...` key with balance = the token's sats. |
| `goose routstr topup` (subsequent) | `POST /v1/balance/topup?cashu_token=<cashu>` with `Bearer sk-...` | Proxy adds the token's sats to the existing key's balance. |
| `goose routstr refund` | `POST /v1/balance/refund` with `Bearer sk-...` | Proxy returns a Cashu token for the entire remaining balance and zeroes the key. |
| `goose routstr balance` (per profile) | `GET /v1/balance/info` with `Bearer sk-...` | Proxy returns balance, reserved, request counts, total spent. |
| chat completion | `POST /v1/chat/completions` with `Bearer sk-...` | Proxy debits the cost from the key's tracked balance. |

## Insufficient balance during chat

If the proxy returns `Insufficient balance: <N> sats required` during a
chat call (commonly because a high-cost model has a higher per-request
escrow than what's left in your tracked balance), goose surfaces the
exact sats number you need. Top up with:

```sh
goose routstr topup            # +2000 sats from local wallet
# - or, if local wallet is empty -
goose wallet topup cashuB...   # add sats to local first
goose routstr topup
```

## Switching the host without `goose configure`

Three escape hatches if you want to bypass the interactive flow:

```sh
# (a) per-shell override of the active profile's URL:
export ROUTSTR_HOST="https://routstr.example.internal"

# (b) edit the config directly:
$EDITOR ~/.config/goose/config.yaml
# tweak ROUTSTR_PROFILES.<name>.url, then run:
goose routstr profile list

# (c) just `goose routstr profile use <name>` to switch among existing profiles
```

Env var `ROUTSTR_HOST` wins over the config file for the duration of the
shell session.

## Limitations

- **Minibits is the only supported mint** (`https://mint.minibits.cash/Bitcoin`,
  hardcoded in `crates/goose-cli/src/commands/wallet.rs` as
  `DEFAULT_MINT_URL`). Cashu tokens minted at any other mint will fail to
  receive into the local wallet, and Routstr instances that trust a
  different mint will reject `sk-` keys created with the wrong mint's
  proofs.
- **One BIP-39 seed for the whole wallet.** All sats live under
  `~/.cdk-gooose/seed`; lose that file and the proofs in `cdk-goose.redb`
  are unrecoverable. There is no per-profile seed.
- **`ROUTSTR_PROFILES` lives in plaintext** at `~/.config/goose/config.yaml`.
  The `sk-` keys are stored alongside the URLs (file permissions are your
  only barrier on a shared machine). The wallet's seed is similarly
  plaintext at `~/.cdk-gooose/seed`. Treat both as private keys.
- **The Routstr provider isn't selectable as a configure-time prompt
  beyond model picking.** The `goose configure → Routstr` flow runs against
  the *active* profile (whatever `goose routstr profile use` last set).
  To configure a different profile, switch first.
- **High-min-escrow models on community proxies can still 402 with a
  small balance.** The error message tells you the proxy's minimum; top
  up to at least that and retry.
