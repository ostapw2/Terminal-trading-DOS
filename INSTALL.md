# Install

The quick way is in the [README](README.md#install): one command for
macOS / Linux (`install.sh`) or Windows (`install.ps1`); both verify a SHA-256
before installing.

## From source

You need Rust 1.80+ ([rustup.rs](https://rustup.rs)). Nothing else: SQLite is
bundled and TLS is pure Rust.

```bash
git clone https://github.com/ostapw2/trading.git
cd trading/rust
cargo build --release
./target/release/dos-commander --demo
```

## Terminal

A terminal of at least 120x30 with mouse support and a font that has box-drawing
characters. macOS Terminal / iTerm2, any Linux terminal, Windows Terminal all work.

## Where DOS keeps things

- Settings, paper ledger, cache: `~/.dos/data.db` (Windows: `%USERPROFILE%\.dos`),
  override with `DOS_DB=/path/to/file.db`. `--demo` keeps nothing.
- API keys (optional, live trading only): see the README section
  *Exchange API keys*; `DOS_BINANCE_API_KEY` / `_SECRET` keep them off disk.

## Uninstall

Delete the `dos-commander` binary and, if you want your data gone too, the
`~/.dos` folder.

## Developers

See [CONTRIBUTING.md](CONTRIBUTING.md). The checks CI runs are `cargo fmt --check`, `cargo clippy --all-targets -- -D
warnings` and `cargo test` in `rust/`.
