# Changelog

All notable changes to this project documented here.  Format:
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) +
[SemVer](https://semver.org/).

## [Unreleased]

Pre-alpha state — nothing has been tagged yet.

### Security (0.1.1)

- rustls 0.23.40 -> 0.23.45 (TLS 1.3 handshake advisory RUSTSEC-2026-0285).
- `DOS_BINANCE_BASE` accepts only `https://api.binance.com` and
  `https://testnet.binance.vision`; signed calls never follow redirects.
- Environment-supplied keys are never written to the database, even when the
  other half of the pair is edited in Settings; API secrets are masked in
  Settings; cleared keys are overwritten on disk (`secure_delete`).
- Workflows: actions pinned to commit SHAs, `--locked` builds, release built
  without a cache.

### Added

- `dos-commander --demo` (or `DOS_DEMO=1`): offline synthetic market data with
  an in-memory database; REST and WebSocket are replaced at the network layer
  only, so parsers, order-book sequencing and the chart pipeline run unchanged.

- API keys: `DOS_BINANCE_API_KEY/SECRET` (and `_FUTURES_`) env vars, never
  written to the database; `:keys`, `:keys check` (Binance `apiRestrictions`:
  read-only / can trade / can withdraw), `:keys clear`; `:live` refuses a key
  that can withdraw and checks an unchecked key first.

### Foundations

- Multi-runtime monorepo: `shared/`, `python/` (deprecated reference),
  `rust/` (production), `legacy/` (skeleton).
- Shared design tokens via `shared/tokens.toml` + `shared/codegen.py` →
  emits `python/src/dos/design/tokens_generated.py`,
  `rust/src/tokens.rs`, `legacy/tokens.inc`.
- Strict ASCII rendering policy in interactive UI; CP437/Unicode block
  elements scoped exception inside chart cells (DESIGN.md §9.6).
- LICENSE (PolyForm Noncommercial 1.0.0 — source-available, free for
  non-commercial use), README, INSTALL.md, .gitignore, GitHub
  Actions CI (build + test + token-sync drift check).

### Rust runtime

- `dos-commander` binary, ~2.3 MB stripped.  Single static executable.
- 4 views switched via F-keys: Panels (file manager), Markets (chart),
  Screener (top gainers), Dashboard (pie + equity curve + positions +
  trades log).
- 5 market types switchable via F9 Settings: US Stocks, Crypto,
  EU Stocks, Forex, Commodities (10 sample symbols each).
- Mouse + keyboard everywhere; SGR mouse + DECSET 1003 hover cursor.
- Live Yahoo Finance fetching via `ureq` + `serde_json`.  `--live`
  startup flag prefetches all symbols; `R` in Markets refreshes the
  active symbol in a background thread (mpsc + std::thread).
- SQLite persistence via `rusqlite` (bundled).  `~/.dos/data.db` stores
  positions + balance + clock + last 50 trades.  Snapshot every 10 s
  and on clean exit.
- Technical indicators: SMA(20), SMA(50), EMA(20) overlays toggled via
  `s` / `S` / `e`; `i` clears all.
- Quick paper trading: `B` / `N` in Markets place 10-share buy/sell
  market orders against dashboard balance + portfolio.
- Live tick simulation in Markets (~250 ms tick) and Dashboard
  (drift-all + 5 s random trade).

### Tests

- 17 integration smoke tests in `rust/tests/smoke.rs` —
  `TestBackend`-driven render checks for every view + state invariants.
- 3 unit tests in `data/indicators.rs` for SMA / EMA / RSI math.
- 16 Python parity tests in `python/tests/` (deprecated suite, still
  green).

### Distribution (planned for v0.1.0)

- GitHub Actions release pipeline (`.github/workflows/release.yml`)
  cross-compiles via `cargo-zigbuild` for:
  - x86_64-unknown-linux-musl
  - aarch64-unknown-linux-musl
  - x86_64-pc-windows-gnu, i686-pc-windows-gnu
  - aarch64-apple-darwin, x86_64-apple-darwin
- One-liner install script (`install.sh`) for Linux / macOS.
- (planned) Homebrew tap, Scoop manifest.
