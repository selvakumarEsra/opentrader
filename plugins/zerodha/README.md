# Zerodha plugin

A data plugin ([Plugin API](../PROTOCOL.md) 2, `datafeed`): Indian stocks, indices, futures and options from the
user's own Zerodha Kite Connect app — instrument search and symbol info, history kept in the bar store, and live
prices. Symbols are `NSE:RELIANCE`, `NSE:NIFTY_50`, `NFO:NIFTY26NOVFUT`, `NFO:NIFTY26N2424000CE`. Exchanges:
NSE, BSE, NFO, BFO, MCX, CDS, BCD, NCO. Contract: [`specs/india-plugins.md`](../../specs/india-plugins.md).
The Dhan plugin claims the same exchanges, so enable only one of the two.

## Settings

Settings > Plugins > Zerodha (each falls back to the environment variable named in its label):

| Setting | Env | |
|---|---|---|
| API key | `KITE_API_KEY` | from developers.kite.trade |
| API secret | `KITE_API_SECRET` | |
| User ID | `KITE_USER_ID` | the Kite login (`AB1234`) |
| Password | `KITE_PASSWORD` | |
| TOTP secret | `KITE_TOTP_SECRET` | the base32 key shown when external TOTP was turned on |
| Always record | | symbols streamed to the store while the app is open (`NSE:RELIANCE, NSE:NIFTY_50`) |
| ClickHouse URL, database, user, password | | store bars in ClickHouse instead of SQLite |

Secrets are kept in OpenTrader's settings on this computer and shown masked. The Kite app's redirect URL can be
anything; the plugin reads the `request_token` off the redirect itself.

## How it works

- **Login** (`src/auth.rs`): Kite's own login flow without a browser — the user ID and password, then a TOTP code
  made from the secret, then the app's `request_token`, exchanged for the day's access token (checksum
  SHA-256 of key, token and secret). The token is kept in the plugin's cache folder (`token.json`, mode 0600) and
  used until 06:00 IST, when Kite ends it. A refused login is not tried again for 60 s; a 403 TokenException logs in
  again once.
- **Instruments** (`src/instruments.rs`): Kite's instrument dump (about 125,000 rows), loaded at most every 20 hours
  and kept on disk for search with Kite down. Options read as `NIFTY 27/11/2025 24000 CE`.
- **History** (`src/history.rs`, plugins/kit `history`): `data.bars` is answered from the store; only the missing
  ranges that can hold trading time are asked of `/instruments/historical`, newest first, in Kite's steps (60 days
  of 1-minute bars, … 2000 of daily), paced to 3 requests a second. Resolutions `1, 3, 5, 10, 15, 30, 60, 1D`.
- **Live** (`src/ticker.rs`): Kite's WebSocket in full mode for the charted and "Always record" symbols; each tick
  is a live price, and each whole minute a stored 1-minute bar. Reconnects with backoff.
- **Store** (plugins/kit `store`): SQLite in the user's cache folder `opentrader-zerodha`
  (`OPENTRADER_ZERODHA_CACHE` overrides it), or ClickHouse tables `zerodha_bars` / `zerodha_covered`.

## Fill the store unattended

The plugin program also runs as a command, outside the app, writing to the same store the charts read. It logs in
on its own (the same flow as the app), so it can run from cron or launchd:

```bash
# Credentials as KITE_*=… lines in a file only you can read (chmod 600), or in the environment.
opentrader-zerodha backfill NSE:RELIANCE NSE:NIFTY_50 --env ~/.config/opentrader/zerodha.env
opentrader-zerodha backfill --file watchlist.txt --from 01/01/2020 --resolutions 1,1D --env ~/.config/opentrader/zerodha.env
opentrader-zerodha backfill --file watchlist.txt --clickhouse http://localhost:8123 --database trading_db --user trading_user --password …
```

`--from` (dd/mm/yyyy, default 01/01/2000) is how far back to ask; a step with no bars ends it sooner. Resolutions
default to `1,1D`. Run again later, only the bars the store is missing are asked. Exit status 0 when every series
was filled, 1 when one failed, 2 for wrong arguments. The instrument list is public and needs no login.

## Build and test

```bash
cargo test            # unit tests and an end-to-end run against a fake Kite and a fake app
cargo build --release # target/release/opentrader-zerodha
```

Releases (tag `plugin-zerodha-v<version>`, or with the app's release) are built by
`.github/workflows/plugin-india.yml` and published to selvakumarEsra/opentrader.
