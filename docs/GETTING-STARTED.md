# Getting started

DOS is a terminal trading dashboard: charts, a screener, an order book and a
paper-trading engine, driven from the keyboard and the mouse. Everything below
works without an exchange account.

## 1. Install

macOS / Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/ostapw2/Terminal-trading-DOS/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/ostapw2/Terminal-trading-DOS/main/install.ps1 | iex
```

(While the repository is private, log in once with the GitHub CLI: `gh auth login`.)
Other options (manual download, build from source): [INSTALL.md](../INSTALL.md).

## 2. First run: demo

```bash
dos-commander --demo
```

Synthetic prices, no network, nothing saved, no key read. The status line shows
`DEMO DATA`. Use it to learn the keys. Quit with **F10** or **q**.

Your terminal should be at least 120x30, with mouse support and a font that has
box-drawing characters.

## 3. Real market data

```bash
dos-commander
```

Public Binance data (and Yahoo for stocks / forex) needs **no key**. Orders are
simulated locally ("paper"): no money moves, no exchange is contacted for trading.

## 4. Find your way around

| Key | View |
|---|---|
| **F4** | **Screener** — Binance 24h pairs; `s` sorts, `Space` ticks a pair, `Enter` opens its chart |
| **F3** | **Markets** — candles / line / bars / footprint; `f` timeframe, `c` chart type, `+ -` zoom, `, .` scroll, `0` back to live |
| **F6** | **Order book** — live depth and recent trades |
| **F5** | **Dashboard** — paper equity, positions, orders, fills |
| **F9** | **Settings** — market universe, timeframe, API keys, cache |
| **F1** | Help for the current view |
| **:** | Command palette (`:help`, `:risk`, `:keys`, `:live`, `:paper`) |

The mouse works everywhere: click the F-keys, menu titles, rows and buttons;
the wheel scrolls.

## 5. A first paper trade

1. **F4**, move with the arrow keys, press **Space** on two or three pairs.
2. In the Trade Ticket (right) pick the size ($ or quantity), then click
   **BUY MKT** or press **B** (**N** sells, **Z** closes).
3. **F5** shows the resulting positions and PnL.

The paper ledger is saved between runs. To start over: menu **Cache** → reset
paper ledger (asks twice).

## 6. Adding your Binance key (optional, for live trading)

Skip this if you only want charts and paper trading.

1. In Binance, create an API key. Keep **Enable Withdrawals off**. Start with
   **read-only**; restrict the key to your IP if you can.
2. Hand it to DOS in one of two ways:
   - Environment variables (nothing is written to disk):
     `DOS_BINANCE_API_KEY` and `DOS_BINANCE_API_SECRET`
     (Futures: `DOS_BINANCE_FUTURES_API_KEY` / `_SECRET`).
   - **F9** → paste key and secret → Apply. Stored in your data file (below).
3. Type `:keys check`. DOS asks Binance what the key can do and tells you in
   plain words (read-only / can trade / **can withdraw — create another key**).
4. `:keys` shows where the key comes from; `:keys clear` removes a stored one.

Live trading is **experimental**: Spot only, and not yet run against a testnet.
`:live` always asks for confirmation (the default answer is No) and refuses a
key that can withdraw. Risk limits: `:risk`.

## Where your data lives

- Settings, paper ledger, chart cache: `~/.dos/data.db`
  (Windows: `%USERPROFILE%\.dos\data.db`). Change it with `DOS_DB=/path/file.db`.
- API keys, if you stored them via F9: in the same file, readable only by your
  user account, **not encrypted**. Prefer the environment variables on shared
  machines.
- `--demo` stores nothing.

## Troubleshooting

- **Garbled lines or boxes:** use a font with box-drawing characters and make the
  window at least 120x30.
- **No hover highlight:** your terminal does not report mouse motion; clicks and
  the keyboard still work.
- **"DB UNAVAILABLE" in the status line:** the data folder is not writable;
  settings and the ledger will not be saved. Set `DOS_DB` to a writable path.
- **Key check says "HTTP 401 / Invalid API-key":** key or secret mistyped, or the
  key was deleted; also check the IP restriction.
- **Clock errors (-1021):** your system clock is off by more than a few seconds.

## Uninstall

Delete the `dos-commander` binary, and `~/.dos` if you also want your data gone.

DOS is not financial advice. Trading involves risk; use it at your own risk.
