# Security

- **Report a vulnerability** privately through GitHub's *Report a vulnerability*
  button on the repository's Security tab. Please do not open a public issue.
- **Your keys:** DOS never sends API keys anywhere except to Binance, signed,
  for the requests you trigger. Create keys **without withdrawal permission**,
  restrict them to your IP, and prefer the `DOS_BINANCE_API_KEY` /
  `DOS_BINANCE_API_SECRET` environment variables over storing them in
  `~/.dos/data.db` (plain text, owner-only).
- **Live trading is experimental** and has not been verified against an exchange
  testnet. Use paper mode or tiny sizes until you trust it.
