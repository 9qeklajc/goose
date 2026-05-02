# Routstr Provider — Manual Test Scenario

Test plan for the `routstr` branch, which integrates the
[Routstr](https://routstr.com/docs) LLM proxy and a Cashu/CDK-backed wallet
into goose. The Routstr proxy bills LLM usage in Bitcoin sats; goose pays per
request with an ecash token carried in the `Authorization` header and reclaims
unused sats after each call via `/v1/wallet/refund`.

What this branch ships:

- `goose::providers::routstr::RoutstrProvider` — an OpenAI-compatible provider
  that posts to a Routstr host and authenticates with a Cashu token
  (`crates/goose/src/providers/routstr.rs`).
- A wallet CLI module (`crates/goose-cli/src/commands/wallet.rs`) with
  `Balance`, `Topup`, and `Withdraw` flows backed by CDK + a local SQLite
  wallet at `~/.cdk-gooose/cdk-goose.sqlite`. The subcommand wiring is
  currently commented out under `TODO(ROU-27)` in `crates/goose-cli/src/cli.rs`
  while the provider is being ported to the new `Provider` trait — see
  *Known limitation* below.

## Configuration

The provider reads three goose config keys (set via `goose configure` or
`~/.config/goose/config.yaml`):

| Key               | Required | Default                            | Notes                                    |
| ----------------- | -------- | ---------------------------------- | ---------------------------------------- |
| `ROUTSTR_HOST`    | yes      | `https://api.routstr.com`          | Base URL of the Routstr proxy.           |
| `ROUTSTR_BASE_PATH` | yes    | `v1/chat/completions`              | Chat-completions path; rarely changed.   |
| `ROUTSTR_API_KEY` | yes      | —                                  | Current Cashu token. Managed by wallet CLI; can be set manually for read-only smoke tests. |
| `OPENAI_TIMEOUT`  | no       | `600`                              | Request timeout in seconds.              |

Wallet defaults (constants in `wallet.rs` / `routstr.rs`):

- Mint: `https://mint.minibits.cash/Bitcoin`
- Currency unit: `sat`
- Wallet dir: `~/.cdk-gooose/` (BIP-39 seed at `~/.cdk-gooose/seed`,
  SQLite at `~/.cdk-gooose/cdk-goose.sqlite`).

## Prerequisites

1. A Routstr host you can reach (default `https://api.routstr.com`, or a
   self-hosted instance).
2. A Cashu mint reachable from the test machine (default
   `https://mint.minibits.cash/Bitcoin`).
3. A funded Cashu token for top-up. Any wallet that supports the configured
   mint can produce one (e.g. Minibits, Cashu.me).
4. A clean test machine — the wallet writes a BIP-39 seed to `~/.cdk-gooose/`
   and treats whatever is there as authoritative. Back it up or use a throwaway
   `$HOME` if you re-run.

## Build

```sh
cargo build -p goose-cli --release
```

The branch adds these crates to `goose-cli`: `cdk`, `cdk-sqlite`, `bip39`,
`home`, `url`, `tokio`, `tracing`, `serde_json`. A clean build should succeed
without network access to the mint (CDK initialises lazily).

## Scenario 1 — Provider smoke test (no wallet)

Verifies that `RoutstrProvider` is registered and can complete a chat request
against a real Routstr host using a manually-supplied token.

1. Obtain a valid ecash token from any Cashu wallet against the same mint the
   Routstr host trusts.
2. `goose configure` → set `ROUTSTR_HOST`, `ROUTSTR_API_KEY` (the token),
   leave defaults for the rest.
3. Select the `routstr` provider and a known model
   (default: `anthropic/claude-sonnet-4`; full list in
   `ROUTSTR_KNOWN_MODELS`).
4. Run `goose session start` and send `say hi`.

Pass criteria:

- Goose returns a chat response (any non-error completion).
- `ROUTSTR_API_KEY` is unchanged in config (provider does **not** rotate the
  token on its own — the wallet CLI does that during top-up/balance).
- `tracing` debug logs show a `POST .../v1/chat/completions` with
  `Authorization: Bearer <token>`.

Fail signals: 401/403 from the proxy means the token is empty/spent — refill
via Scenario 2.

## Scenario 2 — Wallet top-up + balance roundtrip

Verifies that the CDK wallet creates a seed, receives a Cashu token, and
swaps the balance into a single token written back to `ROUTSTR_API_KEY`.

> **Re-enable required.** Uncomment the `Wallet { command: WalletCommand }`
> arm and the `WalletCommand` enum in `crates/goose-cli/src/cli.rs` before
> running this scenario, or call the handlers directly from a test binary.
> Tracked by `TODO(ROU-27)`.

1. From a fresh `$HOME` (or after deleting `~/.cdk-gooose/`), run
   `goose wallet balance`.
2. Expect: `sats: 0`, a new seed is created at `~/.cdk-gooose/seed`, and
   `ROUTSTR_API_KEY` is set to a Cashu token encoding 0 sats (the swap of an
   empty balance).
3. Acquire a funded ecash token (e.g. 100 sats from Minibits) for the
   configured mint.
4. Run `goose wallet topup <token>`.
5. Expect: the balance increases by the token amount; `ROUTSTR_API_KEY` is
   replaced with a fresh token encoding the new balance.
6. Run `goose wallet balance` again — value should match step 5; the API key
   rotates because the wallet swaps unspent outputs each balance call.

## Scenario 3 — Refund-on-top-up

Verifies `handle_refund`: before swapping, the wallet POSTs the *current*
`ROUTSTR_API_KEY` to `<ROUTSTR_HOST>/v1/wallet/refund` and re-claims any
unused sats from the proxy.

1. Set up Scenario 2 with a non-zero balance.
2. Run a chat request (Scenario 1) so the proxy debits some sats but not all.
3. Run `goose wallet topup <new-token>`.

Pass criteria:

- Tracing log: `Claimed change from mint: <N> sats.` for some `N > 0`.
- After top-up, the wallet balance equals
  `(remaining sats refunded) + (top-up sats received)`.
- `ROUTSTR_API_KEY` is cleared (`clear_current_token`) by the refund path
  before the new token is written.

Fail signals: a `Failed to claim change: …` error in logs means the proxy
refused or the mint rejected the refund — capture the offending token from
the error log for debugging.

## Scenario 4 — Withdraw

Verifies `handle_wallet_withdraw`: drains the wallet (or a partial amount)
into a Cashu token printed on stdout.

1. Start with a non-zero balance from Scenario 2.
2. `goose wallet withdraw 50` — expect a single ecash token on stdout
   encoding 50 sats and the wallet balance to drop by 50.
3. `goose wallet withdraw` (no amount) — expect a token encoding the full
   remaining balance and a balance of 0 afterwards.
4. `goose wallet withdraw` on an empty wallet — expect `Wallet is empty.`
   on stdout, no token, no panic.

Pass criteria: the printed token can be received by an external Cashu wallet
and the amount matches.

## Edge cases worth covering

- **Empty top-up:** `goose wallet topup ""` should print
  `No token provided. Operation cancelled.` and not mutate the wallet.
- **Token already spent:** topping up with a previously-redeemed token
  surfaces `Failed to claim change: …` and leaves the balance unchanged.
- **Mint unreachable:** point `DEFAULT_MINT_URL` at a bogus host; wallet ops
  should fail loudly rather than hang. CDK uses the wallet's `timeout`
  (`OPENAI_TIMEOUT`, default 600s) — keep this in mind for CI.
- **Anthropic model branch:** `is_anthropic_model(model_name)` triggers
  `update_request_for_anthropic` in `RoutstrProvider::complete`. Run
  Scenario 1 with both an Anthropic and a non-Anthropic model from
  `ROUTSTR_KNOWN_MODELS` to exercise both paths.
- **`fetch_supported_models_async`:** the provider lists models from
  `<host>/v1/models`. If the host is offline or returns malformed JSON,
  the call must return `Ok(None)` (not an error) per the current
  implementation — verify by pointing `ROUTSTR_HOST` at an unreachable
  address and confirming `goose providers list` does not panic.

## Known limitation — `TODO(ROU-27)`

`crates/goose-cli/src/cli.rs` currently has the wallet subcommand and its
`WalletCommand` enum commented out:

```text
// TODO(ROU-27): wallet subcommands disabled while routstr provider is being
// ported to new Provider trait
```

Until the provider port lands, `goose wallet …` is not exposed on the CLI.
Reviewers running Scenarios 2–4 must either:

- Uncomment the two blocks in `cli.rs` and rebuild, or
- Drive the handlers (`handle_wallet_balance`, `handle_wallet_topup`,
  `handle_wallet_withdraw`) directly from an integration test binary.

This re-enable is the next step in ROU-27 and is intentionally out of scope
for this PR.
