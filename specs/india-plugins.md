---
id: INDIA-DOC
title: Indian market data plugins (Zerodha Kite Connect, Dhan)
owner: selvakumarEsra
priority: high
---

<!-- id: INDIA-DOC -->
# Indian market data plugins (Zerodha Kite Connect, Dhan)

Two data plugins, `zerodha` and `dhan`, serve Indian stocks, indices, futures
and options from the user's own Kite Connect app or Dhan account: the
instrument list (static data), history bars kept in the plugins' bar store
(the moomoo plugin's store, shared through `plugins/kit`: SQLite by default,
ClickHouse when set), and live prices streamed over the broker's WebSocket.
Symbols are `<exchange>:<trading symbol>` with the exchanges `NSE`, `BSE`
(cash and indices), `NFO`, `BFO` (equity derivatives), `MCX` (commodities) and
`CDS` (currency); spaces in a trading symbol become `_` (`NSE:NIFTY_50`). Both
plugins serve the same exchanges, so the user enables one of them.

<!-- id: REQ-INDIA-001 -->
## Each plugin MUST log in from the credentials in its settings, without a browser

Zerodha: `KITE_API_KEY`, `KITE_API_SECRET`, `KITE_USER_ID`, `KITE_PASSWORD`,
`KITE_TOTP_SECRET` (the settings `apiKey`, `apiSecret`, `userId`, `password`,
`totpSecret`). The plugin logs in to Kite with the user id, the password and a
TOTP code made from the secret, takes the Kite Connect `request_token` and
exchanges it (with the API secret's checksum) for the day's `access_token`.
Dhan: `DHAN_CLIENT_ID`, `DHAN_PIN`, `DHAN_TOTP_SECRET` make the day's access
token through Dhan's TOTP token endpoint; or `DHAN_ACCESS_TOKEN` is used as
given. The token is kept in the plugin's folder (readable by the user only)
and made again when it expires or the broker refuses it. Secrets are `secret`
settings (masked in Settings > Plugins) and never written to the log.

implementations:
  - plugins/kit/src/totp.rs:code
  - plugins/zerodha/src/auth.rs:login
  - plugins/dhan/src/auth.rs:generate

## Acceptance
<!-- id: REQ-INDIA-001.A1 -->
- The TOTP code of the RFC 6238 test secret at 59 s is `287082` (SHA-1, 6 digits, 30 s).
<!-- id: REQ-INDIA-001.A2 -->
- The Kite session checksum is the SHA-256 hex of api_key + request_token + api_secret.
<!-- id: REQ-INDIA-001.A3 -->
- Missing credentials are named in the error the app shows (e.g. "Set KITE_API_KEY in Settings > Plugins"), and a refused login is not retried more than once a minute.
<!-- id: REQ-INDIA-001.A4 -->
- Settings of type `secret` are accepted by the app's manifest check and shown as password fields.

<!-- id: REQ-INDIA-002 -->
## Each plugin MUST serve the broker's whole instrument list for search and symbol info

The list (Kite: the instruments dump; Dhan: the scrip master) is loaded at the
first connection and kept on disk, loaded again once a day. Search ranks exact
trading symbol, then prefix, then every query word found in the symbol or the
name (so `NIFTY 24000 CE` finds that strike's options), cash and indices before
derivatives, nearer expiries first; at most 50 rows. Types: `stock`, `index`,
`fund` (ETFs), `futures` (futures and options). Symbol info gives the name,
INR, Asia/Kolkata, the exchange's session (NSE/BSE/NFO/BFO 0915-1530, MCX
0900-2330, CDS 0900-1700), the price scale of the tick size and the intervals.

implementations:
  - plugins/kit/src/catalog.rs:Catalog
  - plugins/zerodha/src/instruments.rs:parse
  - plugins/dhan/src/instruments.rs:parse

## Acceptance
<!-- id: REQ-INDIA-002.A1 -->
- A Kite dump row `NSE,RELIANCE,EQ` becomes `NSE:RELIANCE` (stock) and an `NFO-OPT` row `NIFTY25NOV24000CE` becomes `NFO:NIFTY25NOV24000CE` (futures) with its token, lot size and expiry.
<!-- id: REQ-INDIA-002.A2 -->
- A Dhan row of `NSE_EQ` RELIANCE (`E`, `EQUITY`) becomes `NSE:RELIANCE`; an `OPTIDX` row of segment `D` on NSE becomes an `NFO:` symbol; an `INDEX` row becomes an `index`.
<!-- id: REQ-INDIA-002.A3 -->
- Searching `nifty 24000 ce` returns that strike's options before others; searching `RELIANCE` returns `NSE:RELIANCE` first.

<!-- id: REQ-INDIA-003 -->
## History bars MUST come from the bar store, asking the broker only for the missing trading time

`data.bars` answers from the store when it covers the window; only gaps that
can hold trading time (weekdays within the exchange's session) are asked of
the broker, in the broker's largest allowed steps (Kite: minute 60 days,
3-10 minute 100, 15-30 minute 200, 60 minute 400, day 2000; Dhan: intraday 90
days within the last five years, day in 10-year steps), paced to the broker's
rate limits (Kite 3/s, Dhan 4/s). What was asked is marked covered up to the
start of the forming bar. A refused request answers the stored bars when there
are any. Served intervals: Kite `1, 3, 5, 10, 15, 30, 60, 1D`; Dhan `1, 5, 15, 60, 1D`.

implementations:
  - plugins/kit/src/history.rs:History
  - plugins/zerodha/src/history.rs
  - plugins/dhan/src/history.rs

## Acceptance
<!-- id: REQ-INDIA-003.A1 -->
- A covered window is answered without asking the broker; a weekend gap is not asked.
<!-- id: REQ-INDIA-003.A2 -->
- A 1-minute window of 150 days is asked in three Kite steps (60, 60, 30 days).
<!-- id: REQ-INDIA-003.A3 -->
- Kite candles (`2024-11-14T09:15:00+0530`) and Dhan candles (epoch arrays) become bars stamped at their start in UNIX seconds; a daily bar starts at its day's midnight in Asia/Kolkata.

<!-- id: REQ-INDIA-004 -->
## Live prices MUST stream from the broker's WebSocket and fill the store's 1-minute bars

`data.subscribe` (and the "Always record" symbols) are subscribed on the
broker's WebSocket (Kite `full` mode, Dhan quote packets), again after each
reconnect. Each tick becomes a `data.quote` (price, previous close, the forming
minute and day). Ticks build 1-minute bars; each minute seen from its start is
written to the store's 1-minute series and marked covered, so in market hours
charts read the stream and outside them the store.

implementations:
  - plugins/kit/src/live.rs:Minutes
  - plugins/zerodha/src/ticker.rs:parse
  - plugins/dhan/src/feed.rs:parse

## Acceptance
<!-- id: REQ-INDIA-004.A1 -->
- A Kite `full` packet (184 bytes, big-endian, paise) and an index packet (32 bytes) read back their price, day open/high/low/close, volume and exchange time.
<!-- id: REQ-INDIA-004.A2 -->
- A Dhan quote packet (code 4, little-endian float32) and prev-close packet (code 6) read back price, day values, volume and the previous close.
<!-- id: REQ-INDIA-004.A3 -->
- Ticks over three minutes write the second minute (seen from its start) and not the first (joined midway); its volume is the change of the day's volume.
