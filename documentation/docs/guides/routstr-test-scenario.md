---
sidebar_position: 51
title: Routstr — Manual Test Scenario
sidebar_label: Routstr (test plan)
description: QA matrix for the Routstr provider, wallet CLI, and multi-profile flows
---

# Routstr Provider — Manual Test Scenario

> User-facing setup is in the [Routstr guide](./routstr.md). The wallet
> internals are in the [wallet guide](./routstr-wallet.md). This page is
> the QA / regression matrix.

Test plan for the `routstr` branch. The branch integrates the
[Routstr](https://routstr.com/docs) LLM proxy and a Cashu/CDK-backed
wallet into goose. The provider authenticates with a per-profile
`sk-...` API key issued by the proxy; sats live in a single shared local
Cashu wallet and are moved to/from a profile via the proxy's
`/v1/balance/{create,topup,refund,info}` endpoints.

What this branch ships:

- `goose::providers::routstr::RoutstrProvider` — OpenAI-compatible
  provider that authenticates with the active profile's `sk-...` key.
- `goose::providers::routstr_api` — config schema (`ROUTSTR_PROFILES`,
  `ROUTSTR_ACTIVE`) plus the proxy balance-API client.
- `goose wallet …` (`crates/goose-cli/src/commands/wallet.rs`) — pure
  local CDK wallet (topup / balance / withdraw against a local redb
  store at `~/.cdk-gooose/cdk-goose.redb`).
- `goose routstr …` (`crates/goose-cli/src/commands/routstr.rs`) —
  profile management (`profile add/list/use/remove`) and proxy moves
  (`topup`, `refund`, `balance`).

## Configuration

| Key | Type | Notes |
| --- | --- | --- |
| `ROUTSTR_PROFILES` | map of `{name -> {url, api_key}}` | Written by `goose routstr profile add` / `goose routstr topup`. `api_key` is `sk-...`, **not** a Cashu token. |
| `ROUTSTR_ACTIVE` | string | Name of the currently-active profile. Written by `goose routstr profile use`. |
| `ROUTSTR_HOST` | string (env or config) | Per-shell URL override. Wins over the active profile's `url` when set; useful for one-off scripts. |

Wallet defaults (constants in `wallet.rs`):

- Mint: `https://mint.minibits.cash/Bitcoin`
- Currency: `sat`
- Wallet dir: `~/.cdk-gooose/` (BIP-39 seed at `~/.cdk-gooose/seed`,
  redb store at `~/.cdk-gooose/cdk-goose.redb`)

## Prerequisites

1. A Routstr instance you can reach (default `https://api.routstr.com`,
   community `https://routstr.otrta.me`, or a self-hosted one).
2. A Cashu mint reachable from the test machine. The instance you pick
   must trust this mint — the public Routstr instances accept Minibits
   tokens.
3. A funded Cashu token from any wallet that mints against the same
   mint (Minibits app, Cashu.me, etc.). Even ~500 sats is enough to
   exercise all scenarios.
4. A clean test machine — the wallet writes a BIP-39 seed to
   `~/.cdk-gooose/`. Back it up or use a throwaway `$HOME` if you re-run.

## Build

```sh
cargo build -p goose-cli --no-default-features \
  --features "code-mode,aws-providers,telemetry,otel,rustls-tls"
```

The CDK / wallet stack adds: `cdk = "0.16"`, `cdk-redb = "0.16"`,
`bip39`, `home`. Clean build should succeed without network access.

## Scenario 1 — Provider smoke test (with the new profile flow)

End-to-end: receive a Cashu token, register a profile, top it up, chat.

```sh
goose wallet topup cashuBfromMinibits...
goose routstr profile add otrta --url https://routstr.otrta.me
goose routstr topup 500
goose run --provider routstr --model glm-5.1 --text "say hi" --no-session -q
```

Pass criteria:

- `goose wallet topup` reports `Received N sats. Local wallet balance: N sats.`
- `goose routstr topup` prints `✓ created api_key for "otrta" with N sats (...)`
  and `local wallet: 0 sats (N sats sent to proxy)`.
- The chat replies with the model's response (any non-error completion).
- `goose routstr balance` shows the active profile with a non-zero
  balance and the request/spent counters incremented.

## Scenario 2 — Profile switching with auto-refund

Verifies the multi-host workflow.

```sh
goose routstr profile add upstream --url https://api.routstr.com
goose routstr profile use upstream
goose routstr balance
```

Pass criteria:

- `profile use` prints `✓ refunded N sats from "<old>" into local wallet`.
- The same call prints `✓ active routstr profile is now "upstream"`.
- If the local wallet has sats, the same call prints
  `✓ created api_key for "upstream" with M sats (...)` (auto-topup).
- `goose routstr balance` shows the old profile with `(no api_key — fund
  with goose routstr topup)` and the new profile funded.

Repeat in reverse to confirm symmetric behaviour:

```sh
goose routstr profile use otrta
goose routstr balance
```

## Scenario 3 — Manual refund

```sh
goose routstr refund
goose wallet balance
```

Pass criteria:

- The refund prints `✓ refunded N sats from "<active>" into local wallet`.
- The local wallet balance increases by ~N (Routstr's per-token mint
  fees may shave a few sats).
- The active profile's `api_key` is cleared in `~/.config/goose/config.yaml`.

## Scenario 4 — Withdraw

Drains the local wallet to a Cashu token (no proxy involved).

```sh
goose wallet withdraw 250
goose wallet withdraw           # drain remainder
goose wallet withdraw           # on empty wallet
```

Pass criteria:

- First call prints a `cashuB...` token (≥250 sats) and decreases the
  local balance by 250.
- Second call prints a token for the remaining balance.
- Third call prints `Local wallet is empty.` with no token.

## Scenario 5 — Insufficient balance error

```sh
# from a low-balance profile, ask for an expensive model
goose run --provider routstr --model gpt-5.5-openai --text "hi" --no-session -q
```

Pass criteria:

- The error message reads
  `Insufficient balance: <N> sats required. Please top up your balance to continue.`
- `<N>` is the proxy's per-model minimum (e.g. ~3400 sats on
  `routstr.otrta.me` for `gpt-5.5-openai`), normalised to integer sats
  even when the proxy reports millisats.
- `goose routstr topup <larger-N>` followed by a retry succeeds.

## Edge cases worth covering

- **Empty topup token:** `goose wallet topup ""` prints
  `No token provided. Operation cancelled.` and doesn't touch the wallet.
- **Already-spent token:** running `goose wallet topup` twice with the
  same token surfaces `Failed to receive token: ...` from CDK and leaves
  the balance unchanged.
- **Mint mismatch:** point a profile at a Routstr instance that doesn't
  trust Minibits. `goose routstr topup` against that profile should fail
  with a 4xx from `/v1/balance/create` and not consume local sats.
- **Refund of an already-spent api_key:** if the proxy reports the key
  exhausted, `goose routstr profile use` logs a warning and proceeds —
  re-running `goose routstr profile use <old>` should be a no-op.
- **`goose configure → Configure Providers → Routstr`:** with the active
  profile having a funded `sk-`, configure should fetch
  `<url>/v1/models` and present an interactive picker. Without an
  `sk-` key, configure fails with the actionable
  `Routstr profile "<name>" has no api_key yet. Run goose routstr topup`
  message.
- **Stringified `sats` field on refund:** some Routstr instances return
  `{"token": "...", "sats": "976"}` (string). `parse_response` accepts
  both stringified and integer-form sats/msats — covered by
  `routstr_api::tests::refund_amount_accepts_stringified_sats`.

## Test harness summary

```sh
cargo test -p goose --no-default-features --features "rustls-tls" \
  --lib providers::routstr providers::routstr_api
```

12 unit tests cover:
- model-list parsing for both Routstr schemas (minimal + rich)
- insufficient-balance error parsing for both response envelopes
- mSats → sats normalisation including stringified amounts
- balance-info / balance-create response parsing with extra fields
- `require_api_key` gate on the chat path
