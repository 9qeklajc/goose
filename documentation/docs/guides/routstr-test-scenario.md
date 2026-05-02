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
  `Balance`, `Topup`, and `Withdraw` flows backed by CDK 0.16 + a local
  redb wallet store at `~/.cdk-gooose/cdk-goose.redb`. Exposed as
  `goose wallet …` on the CLI.

## Configuration

The provider reads three goose config keys (set via `goose configure` or
`~/.config/goose/config.yaml`):

| Key               | Required | Default                            | Notes                                    |
| ----------------- | -------- | ---------------------------------- | ---------------------------------------- |
| `ROUTSTR_HOST`    | no       | `https://api.routstr.com`          | Base URL of the Routstr proxy.           |
| `ROUTSTR_API_KEY` | yes      | —                                  | Current Cashu token. Managed by wallet CLI; can be set manually for read-only smoke tests. |

Wallet defaults (constants in `wallet.rs` / `routstr.rs`):

- Mint: `https://mint.minibits.cash/Bitcoin`
- Currency unit: `sat`
- Wallet dir: `~/.cdk-gooose/` (BIP-39 seed at `~/.cdk-gooose/seed`,
  redb store at `~/.cdk-gooose/cdk-goose.redb`).

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

The branch adds these crates to `goose-cli`: `cdk` (0.16), `cdk-redb`
(0.16), `bip39`, `home`, `url`, `tokio`, `tracing`, `serde_json`. We use
`cdk-redb` instead of `cdk-sqlite` because cdk-sqlite's rusqlite pulls in
`libsqlite3-sys 0.28`, which conflicts with goose's `sqlx 0.8.x`
(`libsqlite3-sys 0.30`) at the cargo `links = "sqlite3"` rule. A clean
build should succeed without network access to the mint (CDK initialises
lazily).

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
- **Anthropic model branch:** `supports_cache_control()` returns `true` for
  any model whose name starts with `anthropic/`, which triggers
  `update_request_for_anthropic` before posting. Run Scenario 1 with both
  an Anthropic and a non-Anthropic model from `ROUTSTR_KNOWN_MODELS` to
  exercise both paths.
- **`fetch_supported_models`:** the provider lists models from
  `<host>/v1/models`. Point `ROUTSTR_HOST` at an unreachable address and
  confirm `goose providers list` surfaces a clear error rather than
  panicking.
- **Insufficient balance:** the provider maps a 400 response with
  `code: "insufficient_balance"` to `ProviderError::InsufficientBalance(sats)`
  so the CLI prompts the user to top up. Drain the wallet (Scenario 4),
  then run a chat request — the error message should include the missing
  sat count.
