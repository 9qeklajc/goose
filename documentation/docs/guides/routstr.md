---
sidebar_position: 50
title: Routstr (Cashu-paid LLM proxy)
sidebar_label: Routstr
description: Use the Routstr Cashu-paid LLM proxy with goose, switch hosts, and list available models
---

# Routstr

[Routstr](https://routstr.com/docs) is an OpenAI-compatible LLM proxy that
bills per request in Bitcoin sats via [Cashu](https://cashu.space/) ecash
tokens. goose ships a `routstr` provider that talks to any Routstr instance
(the public `https://api.routstr.com` or your own self-hosted one) and a
`goose wallet` subcommand that manages the Cashu balance the proxy spends.

This guide walks through:

- listing the models a Routstr instance offers
- switching between Routstr hosts (default vs. self-hosted)
- the env vars that override the same settings without the interactive flow

For the full wallet workflow (top-up, balance, withdraw, refund-on-top-up),
see the [Routstr wallet guide](./routstr-wallet.md). For the QA / regression
matrix, see the [test scenario doc](./routstr-test-scenario.md).

## One-time setup

The Routstr provider is a two-step setup: fund the wallet first, then run
`goose configure`. The configure flow only ever asks for `ROUTSTR_HOST` —
the API key is a Cashu token that `goose wallet topup` writes for you.

```sh
goose wallet topup cashuB...   # 1. fund the wallet (see Wallet quickstart below)
goose configure                # 2. pick Routstr, confirm host, choose a model
```

In the configure flow:

1. **Pick Configure Providers → Routstr** from the picker.
2. **Confirm `ROUTSTR_HOST`.** Press Enter to keep the default
   (`https://api.routstr.com`), or type a new URL to point at another
   instance.
3. **Pick a model.** goose calls `<host>/v1/models` and presents the list
   interactively. With many models the picker switches to a search prompt —
   type `claude`, `gemini`, `llama`, etc. to narrow it down.

Goose writes `GOOSE_PROVIDER=routstr` and `GOOSE_MODEL=<your-pick>` on save.

> **Heads-up:** if you run `goose configure → Routstr` *before* you've topped
> up the wallet, the model fetch will fail with
> `ROUTSTR_API_KEY is not set. Run \`goose wallet topup <cashu-token>\` to
> fund your Cashu wallet, then retry.` Top up first, then re-run configure.

## Listing models

The list is whatever `<host>/v1/models` returns at the time you ran
`configure`, so it always matches the host you pointed at. To refresh the list
(say, the proxy added a new model), re-run:

```sh
goose configure
```

… and pick **Configure Providers → Routstr** again. The flow only re-asks
for `ROUTSTR_HOST`; the API key stays whatever the wallet last wrote.

If you only want to peek at the catalogue without going through `configure`,
you can hit the same endpoint directly:

```sh
curl -H "Authorization: Bearer $ROUTSTR_API_KEY" \
     "${ROUTSTR_HOST:-https://api.routstr.com}/v1/models" | jq '.data[].id'
```

## Switching the base URL

`ROUTSTR_HOST` controls which Routstr instance goose talks to. The default is
`https://api.routstr.com`. To point at a self-hosted Routstr:

### Option 1 — `goose configure` (persistent)

Run `goose configure → Configure Providers → Routstr`. The first prompt is
`ROUTSTR_HOST`; type the new URL and press Enter.

```text
●  ROUTSTR_HOST is already configured
?  Would you like to update this value? Yes
?  Enter new value for ROUTSTR_HOST: https://routstr.my-company.internal
```

The change is written to `~/.config/goose/config.yaml`. Re-running `configure`
later picks up the new host on the very next model fetch.

### Option 2 — env var (per-shell override)

```sh
export ROUTSTR_HOST="https://routstr.my-company.internal"
export ROUTSTR_API_KEY="cashuB..."
goose session start
```

Env vars beat the config file, so this is the right path for one-off testing
or pinning a specific host inside a script. Unsetting the env var falls back
to whatever's in `config.yaml`.

### Option 3 — edit the config file

```yaml
# ~/.config/goose/config.yaml
ROUTSTR_HOST: https://routstr.my-company.internal
ROUTSTR_API_KEY: cashuB...
```

Useful when you're scripting setup across machines.

## Host-switch checklist

Different Routstr instances usually trust different Cashu mints. When you
swap `ROUTSTR_HOST`:

- **Make sure your wallet's mint matches.** `goose wallet` defaults to
  `https://mint.minibits.cash/Bitcoin`. If the new host trusts a different
  mint, the proxy will reject your token with `401 / 403`.
- **Re-run `goose configure`** so goose refreshes its model list against the
  new host. The previous host's `GOOSE_MODEL` may not exist on the new one.
- **Check the balance.** After switching, run `goose wallet balance` to
  confirm `ROUTSTR_API_KEY` still encodes a valid token for the new host's
  mint. If it doesn't, top up against the right mint first.

## Wallet quickstart

The Cashu wallet ships as `goose wallet` and writes its seed to
`~/.cdk-gooose/`:

```sh
# create / open the wallet
goose wallet balance

# add 100 sats from a token issued by an external Cashu wallet
goose wallet topup cashuB...

# drain (or partially drain) the wallet back to a Cashu token
goose wallet withdraw 50
```

`balance` and `topup` consolidate proofs into a single ecash token and write
it back to `ROUTSTR_API_KEY`, so the next chat request has a fresh token.

The full top-up workflow — including the refund-on-top-up step that reclaims
unspent sats from Routstr before each top-up — is documented in the
[Routstr wallet guide](./routstr-wallet.md).

## Configuration reference

| Key               | Source                            | Default                        | Notes                                                                                       |
| ----------------- | --------------------------------- | ------------------------------ | ------------------------------------------------------------------------------------------- |
| `ROUTSTR_API_KEY` | `goose wallet topup/balance/withdraw` | —                          | Cashu token written by the wallet. **Not** prompted in `goose configure` and not stored in the keychain (the wallet rewrites it on every consolidate). |
| `ROUTSTR_HOST`    | `goose configure → Routstr`, env var, or `~/.config/goose/config.yaml` | `https://api.routstr.com` | Base URL of the Routstr proxy. The only key the configure flow asks about. |

`goose configure` only prompts for `ROUTSTR_HOST`; the API key is
intentionally invisible to the configure flow because the wallet owns it.
If the API key isn't set when the model fetch runs, configure fails fast
with a clear "run `goose wallet topup` first" hint.

## Anthropic prompt caching

When `GOOSE_MODEL` starts with `anthropic/`, the provider applies the same
prompt-caching markers as the OpenRouter provider — `cache_control: ephemeral`
on the system message, the last two user turns, and the final tool spec.
Routstr forwards these unchanged to Anthropic's underlying endpoint, so cache
hits accrue on subsequent turns within the same session. No configuration
required.

## Insufficient balance

If the proxy returns a `400` or `402` with `code: "insufficient_balance"`,
goose maps it to `ProviderError::InsufficientBalance(<sats>)` and surfaces a
clear top-up prompt instead of a generic 4xx. Fix it with:

```sh
goose wallet topup <new-cashu-token>
```

… and retry the chat request.
