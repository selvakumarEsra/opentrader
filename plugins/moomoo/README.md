# moomoo plugin

A data and broker plugin ([Plugin API](../PROTOCOL.md) 2, `datafeed` and `broker`): symbols, bars and live prices of
US, Hong Kong and China A-share stocks from the user's moomoo OpenD, and trading of the user's paper and live accounts. Symbols are `US:AAPL`, `HK:00700`, `SH:600519`, `SZ:000001`.
Contract: [`specs/moomoo-plugin.md`](../../specs/moomoo-plugin.md).

## How it works

- **OpenD connection** (`src/opend/`): plain TCP to OpenD (default `127.0.0.1:11111`, Settings > Plugins). Each
  message is OpenD's 44-byte header and a JSON body (format 1), so no protobuf toolchain is needed. InitConnect,
  then KeepAlive at the interval OpenD asks for; reconnects every 5 s while OpenD is closed.
  With "RSA private key file" set (the key file also set in OpenD), the connection is encrypted
  (`src/opend/crypt.rs`): InitConnect in RSA, then AES-CBC with the key OpenD returns. An unreadable key file is
  logged and the plugin stays disconnected rather than connect in plain.
- **Bars** (`src/history.rs`, `src/bars.rs`): `data.bars` is answered from the bar store when it holds the window.
  Only the missing parts that can hold trading time (`src/hours.rs`: weekdays, sessions, the US 04:00–20:00
  extended day) are asked of OpenD, through Qot_RequestHistoryKL, forward-adjusted, every page. The 3-, 5-, 15- and
  30-minute bars are rolled up from stored 1-minute bars when those cover the window. US bars include pre- and
  post-market. Resolutions `1, 3, 5,
  15, 30, 60, 1D`; the app builds the others. Bars are returned stamped at their start: when OpenD stamps intraday
  bars at their end (a bar on a session's close), they are moved back one interval. First pages are paced to 60
  per 30 s (`src/pace.rs`), OpenD's limit.
- **Live prices** (`src/stream.rs`): `data.subscribe` → Basic quotes and 1-minute K-lines (2 subscription units per
  symbol). A dropped symbol is unsubscribed after it has been held a minute, which OpenD requires. Each closed
  1-minute bar is written to the store, so in market hours a watched symbol's bars come from the stream, and
  outside them from the store, without using the history quota.
- **Bar store** (`src/store/`): per series (symbol, resolution, session) the bars and the time ranges they cover,
  written every 30 s. By default an SQLite file in the user's cache folder (`OPENTRADER_MOOMOO_CACHE` overrides
  it). With a ClickHouse URL in Settings, a `moomoo_bars` ReplacingMergeTree table and `moomoo_covered`, created
  when missing. When a stored bar's close no longer matches OpenD's (a split or dividend adjusts history), the
  series is dropped and asked again.
- **Trading** (`src/trade/`): each OpenD account and market it may trade is one app account, `SIM-<id>-US` (paper)
  or `REAL-<id>-HK`; SH/SZ stocks trade in the HKCC or CN market. Netting positions; market, limit, stop and
  stop-limit orders (DAY, or GTC when asked), no take profit / stop loss brackets. Orders go through Trd_PlaceOrder
  and Trd_ModifyOrder; a position closes with a market order the other way. `broker.state` is sent after each order
  or fill push and checked every 5 s. A live account trades only once the user has unlocked trading in OpenD; the
  plugin keeps no trade password. `broker.quote` carries the last price only (Basic quotes have no bid and ask).
- **Search** (`src/search.rs`): each market's stocks, funds and indexes, loaded once per session.

## Fill the store yourself

The plugin program also runs as a command, outside the app, writing to the same store the charts read (the same
SQLite file, or ClickHouse with `--clickhouse`):

```bash
# Every 1-minute and daily bar OpenD has for a list, in one run. Again later: only the new bars.
opentrader-moomoo backfill US:AAPL HK:00700 SH:600519
opentrader-moomoo backfill --file watchlist.txt --from 01/01/2020 --clickhouse http://localhost:8123

# Live 1-minute bars of a list, stored as they close, until Ctrl+C (1 subscription unit per symbol).
opentrader-moomoo record --file watchlist.txt
```

Daily bars come in one request; 1-minute bars in 30-day steps from now back, until a step has none (or `--from`).
Each symbol counts once toward the history quota, which `backfill` prints before and after; a refused symbol is
reported and the others go on. `opentrader-moomoo --help` lists the options. A chart the app has already opened
reads the new bars after the plugin restarts.

## Limits of the user's account

OpenD's quotas depend on the account: 100 to 2,000 live subscriptions, and history for 100 to 2,000 distinct
stocks per rolling 7 days. Market data rights (US Level 1/3, HK LV1) come from the account as well.

## Develop

```bash
cargo test        # unit tests, and tests/app_protocol.rs against a fake app and a fake OpenD
OPENTRADER_TEST_CLICKHOUSE=http://localhost:8123 cargo test clickhouse   # the store against a real ClickHouse
cargo build --release
```

## Package

```bash
./build.sh [key]          # macOS / Linux: dist/moomoo-<version>-<platform>.zip; signed, with a catalog entry, when the key is there
./build.ps1 [-Key <key>]  # Windows: dist/moomoo-<version>.zip, .zip.sig and catalog-entry.json; stops without the key
```

The key is the app's release key (default `~/.tauri/opentrader-updater.key`), its password in
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` or `<key>.password`. The catalog (`plugins/catalog.json`) lists one package per
plugin, so it takes the Windows package, as for the other broker plugins; macOS and Linux users install the zip from
a file.
