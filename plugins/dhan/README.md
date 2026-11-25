# Dhan plugin

A data plugin ([Plugin API](../PROTOCOL.md) 2, `datafeed`): Indian stocks, ETFs, indices, futures and options from
the user's own Dhan account (DhanHQ API v2) — instrument search and symbol info, history kept in the bar store, and
live prices. Symbols are `NSE:RELIANCE`, `NSE:NIFTY_50`, `NFO:NIFTY-24NOV2026-24000-CE`, `NFO:NIFTY-24NOV2026-FUT`.
Exchanges: NSE, BSE, NFO, BFO, MCX, CDS, BCD. Contract: [`specs/india-plugins.md`](../../specs/india-plugins.md).
The Zerodha plugin claims the same exchanges, so enable only one of the two.

## Settings

Settings > Plugins > Dhan (each falls back to the environment variable named in its label):

| Setting | Env | |
|---|---|---|
| Client ID | `DHAN_CLIENT_ID` | |
| PIN | `DHAN_PIN` | the 6-digit login PIN |
| TOTP secret | `DHAN_TOTP_SECRET` | the base32 key shown when TOTP was turned on |
| Access token | `DHAN_ACCESS_TOKEN` | instead of PIN and TOTP: a token made at web.dhan.co, used as given |
| Always record | | symbols streamed to the store while the app is open |
| ClickHouse URL, database, user, password | | store bars in ClickHouse instead of SQLite |

Secrets are kept in OpenTrader's settings on this computer and shown masked.

## How it works

- **Token** (`src/auth.rs`): with a PIN and TOTP secret the plugin makes the day's token itself
  (`auth.dhan.co/app/generateAccessToken`), keeps it in its cache folder (`token.json`, mode 0600) and uses it until
  10 minutes before it ends; a refused token (401, DH-901) is made again once. A pasted access token is used as given.
- **Instruments** (`src/instruments.rs`): Dhan's public scrip master (about 160,000 rows), loaded at most every 20
  hours and kept on disk. Only listed equity series, ETFs, indices and unexpired derivatives are kept.
- **History** (`src/history.rs`): `/v2/charts/intraday` (90 days a request, 5 years back) and
  `/v2/charts/historical`, asked only for what the store is missing, paced to 4 requests a second. Resolutions
  `1, 5, 15, 60, 1D`; the app builds the others.
- **Live** (`src/feed.rs`): Dhan's market feed (binary, little-endian) in full mode, up to 100 instruments a
  message; ticks become live prices and stored 1-minute bars. A disconnect for an expired token (807) makes a new one.
- **Store**: SQLite in the cache folder `opentrader-dhan` (`OPENTRADER_DHAN_CACHE`), or ClickHouse tables
  `dhan_bars` / `dhan_covered`.

## Fill the store unattended

`opentrader-dhan backfill SYMBOL… [--file FILE] [--from dd/mm/yyyy] [--resolutions 1,1D] [--clickhouse URL …]
[--env FILE]`: the same command as the Zerodha plugin's (see plugins/zerodha/README.md), with `DHAN_*` credentials.

## Build and test

```bash
cargo test
cargo build --release # target/release/opentrader-dhan
```

Releases (tag `plugin-dhan-v<version>`, or with the app's release) are built by `.github/workflows/plugin-india.yml`
and published to selvakumarEsra/opentrader.
