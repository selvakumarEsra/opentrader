---
id: MOOMOO-DOC
title: Moomoo OpenD data plugin
owner: plugins [needs user confirmation]
priority: medium
version: 1
depends_on: DOMAIN-DOC
---

<!-- id: MOOMOO-DOC -->
# Moomoo OpenD data plugin

A data plugin (`plugins/PROTOCOL.md`, capability `datafeed`, Plugin API 2) that
gets symbols, bars and live prices from the user's moomoo OpenD gateway on this
computer. It is a standalone program under `plugins/moomoo/` with its own Cargo
workspace, like `plugins/ibkr/`. The app does not change.

OpenD speaks framed messages over TCP (default `127.0.0.1:11111`). Each message
is a 44-byte header (`"FT"`, proto id, format, version, serial number, body
length, SHA-1 of the body, 8 reserved bytes) and a body. The plugin uses the
JSON body format (format 1), so it needs no protobuf toolchain.

Out of scope for version 1: the `broker` capability, encrypted OpenD connections,
pre/post-market bars, and a disk cache of bars.

<!-- id: REQ-MOOMOO-001 -->
## OpenD messages MUST be framed and checked exactly

implementations:
  - plugins/moomoo/src/opend/frame.rs:encode
  - plugins/moomoo/src/opend/frame.rs:decode

## Acceptance
<!-- id: REQ-MOOMOO-001.A1 -->
- An encoded message is 44 header bytes plus the body. The header holds `F`, `T`, the proto id (u32 LE), format 1, version 0, the serial number (u32 LE), the body length (u32 LE) and the SHA-1 of the body.
<!-- id: REQ-MOOMOO-001.A2 -->
- Decoding gives back the proto id, the serial number and the body. A header not starting `FT`, or a body whose SHA-1 differs, is an error.
<!-- id: REQ-MOOMOO-001.A3 -->
- The decoder waits for a whole frame: one given in two pieces decodes once both have arrived.

<!-- id: REQ-MOOMOO-002 -->
## The plugin MUST keep one OpenD connection alive and say when it is down

On start, and again after each loss, the plugin connects, sends InitConnect
(1001), and then sends KeepAlive (1004) at the interval OpenD returns. Answers
are matched to requests by serial number. Pushes (3005, 3007) go to the stream.
While OpenD is unreachable, requests fail with a message naming the host and
port. The plugin retries every 5 s and logs the loss once.

implementations:
  - plugins/moomoo/src/opend/client.rs:Client

## Acceptance
<!-- id: REQ-MOOMOO-002.A1 -->
- Against a fake OpenD, a request is answered with the `s2c` of the reply carrying the same serial number. A reply whose `retType` is not 0 becomes an error carrying `retMsg`.
<!-- id: REQ-MOOMOO-002.A2 -->
- With nothing listening on the port, `data.bars` returns a `-32000` error naming the address.

<!-- id: REQ-MOOMOO-003 -->
## Symbols MUST map between the app and OpenD one to one

The plugin serves exchanges `US`, `HK`, `SH` and `SZ` (OpenD markets 11, 1, 21 and 22).
`US:AAPL` is OpenD's `{market: 11, code: "AAPL"}`. Each exchange has a fixed time
zone, regular session and price scale. [needs review: exchange names]

implementations:
  - plugins/moomoo/src/market.rs:Market

## Acceptance
<!-- id: REQ-MOOMOO-003.A1 -->
- `US:AAPL`, `HK:00700`, `SH:600519` and `SZ:000001` map to markets 11, 1, 21 and 22 and back. `XX:Y` and a bare `AAPL` map to nothing.
<!-- id: REQ-MOOMOO-003.A2 -->
- `data.symbolInfo` gives `America/New_York` / `0930-1600` for US, `Asia/Hong_Kong` / `0930-1200,1300-1600` for HK and `Asia/Shanghai` / `0930-1130,1300-1500` for SH and SZ, and the intervals `1, 3, 5, 15, 30, 60, 1D`.

<!-- id: REQ-MOOMOO-004 -->
## `data.bars` MUST return start-stamped bars within OpenD's rate limit

`data.bars` maps the resolution to a K-line type, asks Qot_RequestHistoryKL (3103)
with forward-adjusted prices, and follows `nextReqKey` pages. It returns the newest
`countBack` bars in `[from, to]`, oldest first, stamped at their start. A daily
bar starts at its day's midnight in the exchange's time zone. If OpenD stamps
intraday bars at their end (a session's first bar falls at open + interval),
they are moved back by one interval. First pages are paced to at most 60 per
30 s; later pages are not limited.

implementations:
  - plugins/moomoo/src/bars.rs:kl_type
  - plugins/moomoo/src/bars.rs:to_bars
  - plugins/moomoo/src/bars.rs:end_stamped
  - plugins/moomoo/src/pace.rs:Pace

## Acceptance
<!-- id: REQ-MOOMOO-004.A1 -->
- Resolutions `1, 3, 5, 15, 30, 60, 1D` map to K-line types 1, 10, 6, 7, 8, 9 and 2. Any other resolution returns an empty list.
<!-- id: REQ-MOOMOO-004.A2 -->
- US 1-minute bars whose first time in the session is `09:31:00` are returned starting at 09:30 New York time. Bars starting at `09:30:00` are returned unchanged.
<!-- id: REQ-MOOMOO-004.A3 -->
- A daily bar `2025-05-09 00:00:00` on US is returned at 2025-05-09 00:00 in New York (UNIX 1746763200).
<!-- id: REQ-MOOMOO-004.A4 -->
- Bars marked `isBlank` are dropped. With more than `countBack` bars, the newest `countBack` are kept.
<!-- id: REQ-MOOMOO-004.A5 -->
- The 61st first-page request within 30 s waits until the oldest of the 60 is 30 s old.

<!-- id: REQ-MOOMOO-005 -->
## Live quotes MUST follow `data.subscribe` within OpenD's subscription rules

`data.subscribe` gives the whole set of symbols to stream. The plugin subscribes
new symbols to Basic quotes and 1-minute K-lines (2 quota units each). It sends
one `data.quote` per push: the last price, the previous close, the forming
minute bar and the day's bar. A dropped symbol is unsubscribed once it has been
held for 1 minute, since OpenD refuses an earlier cancel. After a reconnect the
whole set is subscribed again.

implementations:
  - plugins/moomoo/src/stream.rs:Subscriptions
  - plugins/moomoo/src/stream.rs:quote_of

## Acceptance
<!-- id: REQ-MOOMOO-005.A1 -->
- Going from set {A} to {A, B} subscribes B only. Going to {} unsubscribes nothing before A's minute is up, and A once it is.
<!-- id: REQ-MOOMOO-005.A2 -->
- A Basic push for `US:AAPL` gives `{"ticker":"US:AAPL","price":cur,"prevClose":lastClose,"day":[midnight,open,high,low,cur,volume]}`, together with the last 1-minute bar seen for it.
<!-- id: REQ-MOOMOO-005.A3 -->
- A push for a symbol not in the set is not sent to the app.

<!-- id: REQ-MOOMOO-006 -->
## `data.search` MUST find symbols by code or name

The plugin loads each exchange's list of stocks, ETFs and indexes once
(Qot_GetStaticInfo, 3202, by market). It returns up to 50 matches: exact code
first, then code prefix, then name match.

implementations:
  - plugins/moomoo/src/search.rs:rank

## Acceptance
<!-- id: REQ-MOOMOO-006.A1 -->
- For query `aap`, `US:AAPL` comes before `US:AAPU` and before a stock named "Aapex…". At most 50 rows are returned. Rows carry whole symbols and the type `stock`, `fund` or `index`.
