# moomoo plugin

A data plugin ([Plugin API](../PROTOCOL.md) 2, `datafeed`): symbols, bars and live prices of US, Hong Kong and
China A-share stocks from the user's moomoo OpenD. Symbols are `US:AAPL`, `HK:00700`, `SH:600519`, `SZ:000001`.
Contract: [`specs/moomoo-plugin.md`](../../specs/moomoo-plugin.md).

## How it works

- **OpenD connection** (`src/opend/`): plain TCP to OpenD (default `127.0.0.1:11111`, Settings > Plugins). Each
  message is OpenD's 44-byte header and a JSON body (format 1), so no protobuf toolchain is needed. InitConnect,
  then KeepAlive at the interval OpenD asks for; reconnects every 5 s while OpenD is closed.
- **Bars** (`src/bars.rs`): `data.bars` → Qot_RequestHistoryKL, forward-adjusted, every page. Resolutions `1, 3, 5,
  15, 30, 60, 1D`; the app builds the others. Bars are returned stamped at their start: when OpenD stamps intraday
  bars at their end (a bar on a session's close), they are moved back one interval. First pages are paced to 60
  per 30 s (`src/pace.rs`), OpenD's limit.
- **Live prices** (`src/stream.rs`): `data.subscribe` → Basic quotes and 1-minute K-lines (2 subscription units per
  symbol). A dropped symbol is unsubscribed after it has been held a minute, which OpenD requires.
- **Search** (`src/search.rs`): each market's stocks, funds and indexes, loaded once per session.

## Limits of the user's account

OpenD's quotas depend on the account: 100 to 2,000 live subscriptions, and history for 100 to 2,000 distinct
stocks per rolling 7 days. Market data rights (US Level 1/3, HK LV1) come from the account as well.

## Develop

```bash
cargo test        # unit tests, and tests/app_protocol.rs against a fake app and a fake OpenD
cargo build --release
```

Not yet: the `broker` capability (OpenD's trading API), encrypted OpenD connections, pre/post-market bars, a disk
cache of bars, a Windows package and a catalog entry.
