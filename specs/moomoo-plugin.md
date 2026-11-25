---
id: MOOMOO-DOC
title: Moomoo OpenD data plugin
owner: plugins [needs user confirmation]
priority: medium
version: 2
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

Version 2 adds trading (`broker`), encrypted OpenD connections, pre/post-market
bars, a disk cache of bars, and the packages for Windows, macOS and Linux that are its catalog entries.

<!-- id: REQ-MOOMOO-001 -->
## OpenD messages MUST be framed and checked exactly

implementations:
  - plugins/moomoo/src/opend/frame.rs:encode
  - plugins/moomoo/src/opend/frame.rs:decode

verifies:
  - plugins/moomoo/src/opend/frame.rs:header_holds_the_fields_in_order
  - plugins/moomoo/src/opend/frame.rs:decodes_what_it_encodes_and_waits_for_the_rest
  - plugins/moomoo/src/opend/frame.rs:refuses_a_bad_header_or_checksum

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

verifies:
  - plugins/moomoo/src/opend/client.rs:an_answer_is_its_s2c_or_its_reason

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

verifies:
  - plugins/moomoo/src/market.rs:maps_symbols_both_ways
  - plugins/moomoo/src/market.rs:each_market_has_its_zone_and_session

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
  - plugins/kit/src/pace.rs:Pace

verifies:
  - plugins/moomoo/src/bars.rs:resolutions_map_to_kline_types
  - plugins/moomoo/src/bars.rs:end_stamped_minutes_move_back_to_their_start
  - plugins/moomoo/src/bars.rs:start_stamped_minutes_stay
  - plugins/moomoo/src/bars.rs:a_daily_bar_starts_at_midnight_in_new_york
  - plugins/moomoo/src/bars.rs:blanks_go_and_the_newest_count_stay
  - plugins/kit/src/pace.rs:the_61st_waits_until_the_first_is_30_s_old

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
minute bar and the day's bar. A 1-minute K-line push moves the chart too, without
waiting for the next Basic push: its close is the price, and the day's bar takes
its high, low and close. It is sent only once a Basic quote has been seen for the
symbol, so the previous close and the day's bar are known. Once a minute the plugin
writes to its log how many Basic and K-line pushes each symbol got in that minute,
so the push rate of an account can be seen; a minute with no push writes nothing. A dropped symbol is unsubscribed once it has been
held for 1 minute, since OpenD refuses an earlier cancel. After a reconnect the
whole set is subscribed again.

implementations:
  - plugins/moomoo/src/stream.rs:Subscriptions
  - plugins/moomoo/src/stream.rs:quote_of
  - plugins/moomoo/src/stream.rs:Quotes.minute_quote
  - plugins/moomoo/src/stream.rs:PushCount

verifies:
  - plugins/moomoo/src/stream.rs:new_symbols_at_once_dropped_ones_after_a_minute
  - plugins/moomoo/src/stream.rs:a_reconnect_subscribes_the_wanted_set_again
  - plugins/moomoo/src/stream.rs:basic_pushes_become_quotes_of_wanted_symbols_only
  - plugins/moomoo/src/stream.rs:a_minute_push_after_a_basic_one_moves_the_price
  - plugins/moomoo/src/stream.rs:pushes_are_counted_per_symbol_for_the_log

## Acceptance
<!-- id: REQ-MOOMOO-005.A1 -->
- Going from set {A} to {A, B} subscribes B only. Going to {} unsubscribes nothing before A's minute is up, and A once it is.
<!-- id: REQ-MOOMOO-005.A2 -->
- A Basic push for `US:AAPL` gives `{"ticker":"US:AAPL","price":cur,"prevClose":lastClose,"day":[midnight,open,high,low,cur,volume]}`, together with the last 1-minute bar seen for it.
<!-- id: REQ-MOOMOO-005.A3 -->
- A push for a symbol not in the set is not sent to the app.
<!-- id: REQ-MOOMOO-005.A4 -->
- After a Basic push for `US:AAPL` (cur 200.5, high 202, low 197), a 1-minute K-line push (high 203, low 199, close 202.5) gives a `data.quote` with price 202.5, the Basic push's previous close, the day `[midnight, open, 203, 197, 202.5, volume]` and that minute bar. A K-line push before any Basic push for the symbol sends nothing.
<!-- id: REQ-MOOMOO-005.A5 -->
- After 3 Basic and 2 K-line pushes for `US:AAPL` and 1 Basic push for `HK:00700`, the minute's log line is `[debug] pushes in the last minute: HK:00700 1 basic, 0 K-line; US:AAPL 3 basic, 2 K-line`, and the next minute with no push writes no line.

<!-- id: REQ-MOOMOO-006 -->
## `data.search` MUST find symbols by code or name

The plugin loads each exchange's list of stocks, ETFs and indexes once
(Qot_GetStaticInfo, 3202, by market). It returns up to 50 matches: exact code
first, then code prefix, then name match.

implementations:
  - plugins/moomoo/src/search.rs:rank

verifies:
  - plugins/moomoo/src/search.rs:exact_code_then_prefix_then_name
  - plugins/moomoo/src/search.rs:at_most_fifty_rows
  - plugins/moomoo/src/search.rs:reads_listed_securities_of_served_markets_and_types

## Acceptance
<!-- id: REQ-MOOMOO-006.A1 -->
- For query `aap`, `US:AAPL` comes before `US:AAPU` and before a stock named "Aapex…". At most 50 rows are returned. Rows carry whole symbols and the type `stock`, `fund` or `index`.

<!-- id: REQ-MOOMOO-007 -->
## Bars MUST come from the local disk store, kept current by the live stream

OHLCV bars are stored on disk per (symbol, resolution, session), with the time
ranges they cover, so charts open fast. A small account can fetch history for
only 100 different stocks per rolling 7 days, so the store also saves quota.

- **During market hours** the live stream fills the store. Each closed 1-minute
  bar of a subscribed symbol is written as it closes, and a run of pushes
  without a reconnect covers the minutes in between. A 3/5/15/30-minute chart's
  newest part is rolled up from those 1-minute bars.
- **Outside market hours** bars come from the store. A part of the window that
  holds no trading time (night, weekend) counts as covered.
- **OpenD's history** is asked only for an uncovered part that holds trading
  time. The ask starts one bar before that part. If the overlapping bar differs
  (a split or dividend changed the adjusted prices), the stored series is
  dropped. What was asked counts as covered up to the bars still forming, even
  when OpenD returned none (a holiday is asked once).
- **When OpenD refuses** (quota, no connection), the stored bars of the window
  are returned.
- **Where bars are stored:** in ClickHouse when the user gives its URL
  (table `<database>.moomoo_bars`, a ReplacingMergeTree ordered by symbol,
  exchange, resolution, session and timestamp, partitioned by month, plus
  `moomoo_covered` for the ranges), or else in an embedded SQLite file whose
  bars table is clustered on (series, time). Both read a series in one range
  scan. Bars streamed in a minute reach the store in one batched write.

implementations:
  - plugins/kit/src/store/mod.rs:Series
  - plugins/kit/src/store/mod.rs:Store
  - plugins/kit/src/store/sqlite.rs:Sqlite
  - plugins/kit/src/store/clickhouse.rs:ClickHouse
  - plugins/moomoo/src/hours.rs:has_trading
  - plugins/moomoo/src/history.rs:History
  - plugins/moomoo/src/history.rs:History.bars

verifies:
  - plugins/moomoo/src/history.rs:a_covered_window_is_answered_without_opend
  - plugins/moomoo/src/history.rs:the_weekend_after_a_fetch_needs_no_opend
  - plugins/moomoo/src/history.rs:a_longer_window_asks_from_one_bar_before_the_covered_end
  - plugins/moomoo/src/history.rs:changed_adjusted_prices_drop_the_stored_series
  - plugins/moomoo/src/history.rs:opend_refusing_answers_the_stored_bars
  - plugins/moomoo/src/history.rs:streamed_minutes_answer_minute_and_five_minute_charts
  - plugins/kit/src/store/sqlite.rs:a_flushed_series_is_read_back_by_a_new_store
  - plugins/kit/src/store/sqlite.rs:a_dropped_series_is_deleted
  - plugins/kit/src/store/clickhouse.rs:refuses_what_is_not_an_address_or_a_name
  - plugins/moomoo/src/hours.rs:nights_and_weekends_hold_no_trading

## Acceptance
<!-- id: REQ-MOOMOO-007.A1 -->
- A second `data.bars` for a window already covered makes no OpenD request.
<!-- id: REQ-MOOMOO-007.A2 -->
- On a Saturday, a window from Friday's fetch until now makes no OpenD request.
<!-- id: REQ-MOOMOO-007.A3 -->
- A window that extends past the covered part asks OpenD only from one bar before the covered end. If the overlapping bar's close differs, the series is dropped and the whole window is fetched.
<!-- id: REQ-MOOMOO-007.A4 -->
- Closed 1-minute bars from the stream are in the store, and a `data.bars` of 1 or 5 minutes over the streamed minutes makes no OpenD request.
<!-- id: REQ-MOOMOO-007.A5 -->
- When OpenD answers with an error, the stored bars inside the window are returned (and an empty store gives the error).
<!-- id: REQ-MOOMOO-007.A6 -->
- The store survives a restart, on both backends. The SQLite file lives under the user's cache folder (`OPENTRADER_MOOMOO_CACHE` overrides it).
<!-- id: REQ-MOOMOO-007.A7 -->
- With a ClickHouse URL, the plugin creates its tables if they are missing. A re-written bar replaces the earlier one (`FINAL`). A dropped series is deleted.

<!-- id: REQ-MOOMOO-008 -->
## US symbols MUST include pre- and post-market trading

For US symbols, `data.symbolInfo` gives `extended: "0400-2000"`. Intraday bars
(60 minutes and shorter) and live 1-minute bars are asked with OpenD's extended
time, and the app keeps or hides them with its RTH/ETH switch. Daily bars stay
regular. Telling end stamps from start stamps uses the extended day's bounds
(04:00 and 20:00).

implementations:
  - plugins/moomoo/src/market.rs:Market.extended
  - plugins/moomoo/src/bars.rs:end_stamped
  - plugins/moomoo/src/hours.rs:has_trading

verifies:
  - plugins/moomoo/src/bars.rs:extended_us_bars_are_judged_on_the_extended_day

## Acceptance
<!-- id: REQ-MOOMOO-008.A1 -->
- A US 5-minute request carries `extendedTime: true`; a US daily request and any HK request do not.
<!-- id: REQ-MOOMOO-008.A2 -->
- Start-stamped US extended bars at 04:00 … 16:00 … 19:55 are returned unchanged. End-stamped ones (04:05 … 20:00) move back 5 minutes.

<!-- id: REQ-MOOMOO-009 -->
## The OpenD connection MUST be encrypted when the user gives OpenD's RSA key

With the setting "RSA private key file" (the PEM key file also set in OpenD),
InitConnect is RSA-encrypted (PKCS#1 v1.5, in chunks sized to the key) and asks
for AES-CBC. Every later body is AES-encrypted with the key OpenD returns: CBC
with PKCS#7 when OpenD gives an IV, else OpenD's ECB form (zero padding plus a
16-byte block holding the length of the last block). The header's SHA-1 is
always that of the plain body. Without the setting, nothing is encrypted.

implementations:
  - plugins/moomoo/src/opend/crypt.rs:Rsa
  - plugins/moomoo/src/opend/crypt.rs:Aes

verifies:
  - plugins/moomoo/src/opend/crypt.rs:rsa_round_trips_bodies_longer_than_a_chunk
  - plugins/moomoo/src/opend/crypt.rs:aes_round_trips_in_both_forms

## Acceptance
<!-- id: REQ-MOOMOO-009.A1 -->
- RSA: what is encrypted with the key's public half decrypts with its private half, for bodies longer than one chunk.
<!-- id: REQ-MOOMOO-009.A2 -->
- AES-CBC and the ECB form decrypt what they encrypt, for bodies of 0, 15, 16 and 33 bytes. The ECB form's output is the padded length plus 16.
<!-- id: REQ-MOOMOO-009.A3 -->
- Against a fake OpenD holding the same key, an encrypted session answers `data.bars`.
<!-- id: REQ-MOOMOO-009.A4 -->
- An unreadable key file is reported in the plugin's log, and the plugin does not connect without encryption instead.

<!-- id: REQ-MOOMOO-010 -->
## The plugin MUST trade the user's moomoo accounts as a broker plugin

The manifest adds `broker`. Each account of OpenD's account list and each market
it may trade is one app account: `SIM-<id>-US` ("Paper US …", `isDemo: true`)
or `REAL-<id>-HK`. SH and SZ stocks trade through the HKCC (Stock Connect) or CN
market. Positions are netted. Orders can be market, limit, stop or stop-limit,
day or GTC, with no brackets. The plugin places orders (Trd_PlaceOrder, with a
packet id), modifies and cancels them (Trd_ModifyOrder), closes positions with
a market order the opposite way, and sends the whole `broker.state` again when
OpenD pushes an order or fill change, and at least every 5 s. A real account
trades only after the user unlocks trading in OpenD. The plugin keeps no trade
password. The app's trading switch and its confirmation of live orders apply as
for every broker.

implementations:
  - plugins/moomoo/src/trade/mod.rs:Broker
  - plugins/moomoo/src/trade/map.rs:order_request
  - plugins/moomoo/src/trade/map.rs:state_of

verifies:
  - plugins/moomoo/src/trade/map.rs:a_paper_account_with_two_markets_is_two_demo_accounts
  - plugins/moomoo/src/trade/map.rs:a_gtc_stop_limit_becomes_opends_order
  - plugins/moomoo/src/trade/map.rs:a_wrong_market_or_a_bracket_is_refused
  - plugins/moomoo/src/trade/map.rs:statuses_map_to_the_apps
  - plugins/moomoo/src/trade/map.rs:closing_four_of_a_long_ten_sells_four_at_market
  - plugins/moomoo/src/trade/map.rs:orders_and_fills_read_back_and_tell_their_toast

## Acceptance
<!-- id: REQ-MOOMOO-010.A1 -->
- A paper account with US and HK rights gives the accounts `SIM-1-US` and `SIM-1-HK`, both `isDemo: true`.
<!-- id: REQ-MOOMOO-010.A2 -->
- `broker.place` of a buy stop-limit of 10 `US:AAPL` at limit 101, stop 100, GTC sends `trdSide 1, orderType 11, code AAPL, secMarket 2, qty 10, price 101, auxPrice 100, timeInForce 1` under the selected account's header, with a fresh packet id. The answer is `{orderId}`.
<!-- id: REQ-MOOMOO-010.A3 -->
- An order for `HK:00700` on a US account, or one with a take profit or stop loss, is refused with a message and nothing is sent.
<!-- id: REQ-MOOMOO-010.A4 -->
- Order statuses map: submitting → `placing`, submitted or partly filled → `working`, filled → `filled`, cancelled or deleted → `cancelled`, failed or disabled → `rejected`.
<!-- id: REQ-MOOMOO-010.A5 -->
- An order push from OpenD is followed by a `broker.event` (`executed` for a fill) and a new `broker.state`.
<!-- id: REQ-MOOMOO-010.A6 -->
- `broker.closePosition` of a long 10 with `qty: 4` sends a market sell of 4.

<!-- id: REQ-MOOMOO-011 -->
## The plugin MUST ship as signed packages

`build.ps1` (Windows) and `build.sh` (macOS, Linux) build the release program.
They zip it with the manifest, whose entry and platforms are set for that
system. They sign the zip with the release key (`npx tauri signer sign`) and
write the catalog entry. The catalog gets one package per system: Windows, macOS
and Linux. A missing key stops the build before any catalog entry is
written.

implementations:
  - plugins/moomoo/build.ps1
  - plugins/moomoo/build.sh
  - plugins/moomoo/opentrader-plugin.json

## Acceptance
<!-- id: REQ-MOOMOO-011.A1 -->
- `build.sh` without the key leaves the unsigned zip and says it was not signed. No catalog entry is written.
<!-- id: REQ-MOOMOO-011.A2 -->
- The zip's manifest names `opentrader-moomoo.exe` and `["windows"]` from `build.ps1`, and `opentrader-moomoo` and that system from `build.sh`.
<!-- id: REQ-MOOMOO-011.A3 -->
- The plugin's own manifest names `windows`, `macos` and `linux`: it runs on each of them, with OpenD on that computer or another one.

<!-- id: REQ-MOOMOO-012 -->
## `opentrader-moomoo backfill` MUST fill the bar store with a list's whole history in one run

Run by the user, outside the app:
`opentrader-moomoo backfill [options] SYMBOL… | --file LIST`. For each symbol
and each resolution (`--resolution`, default `1,1D`) it asks OpenD for every bar
the store lacks, and stores them in the same series the app's charts read (with
the same store options as the plugin's settings: `--clickhouse URL`,
`--database`, `--user`, `--password`; else the embedded store). Without `--from`
(dd/mm/yyyy or yyyy-mm-dd) it goes back as far as OpenD has bars: daily bars in
one request, 1-minute bars in 30-day steps from now back to the first step
with none. Windows the store already covers are not asked again, so a second run
only fetches the bars since the first. A symbol OpenD refuses (quota, no rights)
is reported and skipped, and the others go on. It prints the history quota
OpenD reports before and after, and ends with a line per symbol and resolution.

implementations:
  - plugins/moomoo/src/cli.rs:backfill
  - plugins/moomoo/src/history.rs:History.fill

verifies:
  - plugins/moomoo/src/cli.rs:a_backfill_goes_back_to_the_first_step_without_bars_and_a_second_asks_nothing
  - plugins/moomoo/src/cli.rs:a_refused_symbol_fails_and_the_next_is_filled
  - plugins/moomoo/src/history.rs:an_answer_cut_at_the_most_bars_covers_only_up_to_its_newest

## Acceptance
<!-- id: REQ-MOOMOO-012.A1 -->
- Against a source with 1-minute bars in the last 70 days only, a backfill without `--from` stores them all and asks no step older than the first empty one.
<!-- id: REQ-MOOMOO-012.A2 -->
- A second backfill right after the first asks OpenD nothing for days already covered.
<!-- id: REQ-MOOMOO-012.A3 -->
- A symbol whose asks fail is reported as failed; the next symbol is still filled, and the run's exit code is not 0.
<!-- id: REQ-MOOMOO-012.A4 -->
- When one answer holds the most bars a request may collect, the store covers only up to its newest bar, so the rest is asked again.

<!-- id: REQ-MOOMOO-013 -->
## `opentrader-moomoo record` MUST keep a list's live 1-minute bars in the store

`opentrader-moomoo record [options] SYMBOL… | --file LIST` connects to OpenD,
subscribes the symbols' 1-minute K-lines (within the account's subscription
quota), and stores each closed bar as the plugin does, until stopped (Ctrl+C).
It writes every 30 s and when it stops. It subscribes again after a reconnect.
Minutes missed while disconnected are not marked covered, so a later backfill
or chart fetches them.

implementations:
  - plugins/moomoo/src/cli.rs:record
  - plugins/moomoo/src/stream.rs:MinuteCover

verifies:
  - plugins/moomoo/src/stream.rs:closed_minutes_cover_the_span_since_the_last_one_until_a_reconnect
  - plugins/moomoo/src/cli.rs:the_command_line_reads_symbols_options_and_refuses_strangers

## Acceptance
<!-- id: REQ-MOOMOO-013.A1 -->
- Two closed minutes in a row cover the span from the first's start to the second's end; after a reconnect, the next closed minute covers only itself.
<!-- id: REQ-MOOMOO-013.A2 -->
- Symbols that are not the plugin's (`AAPL`, `XX:NOPE`) are refused before connecting, with the expected form.

<!-- id: REQ-MOOMOO-014 -->
## Plugin releases MUST be built by GitHub Actions and published to selvakumarEsra/opentrader only

A pushed tag `plugin-moomoo-v<version>` (matching the manifest and Cargo.toml)
builds, tests and releases the plugin. Each system gets one self-contained
program holding the whole plugin (the app's plugin, `backfill` and `record`, the
embedded store; nothing else to install): `opentrader-moomoo-<version>-windows-x86_64.exe`,
`…-macos-universal` (Apple Silicon and Intel), `…-linux-x86_64`, and the zip of
each with its manifest for Settings > Plugins. They are published to the
release of the tag in `selvakumarEsra/opentrader`, never in the upstream
`deepentropy/opentrader`: the release job runs only in that repository and
names it. Catalog entries (when signed) point at that repository's release.

implementations:
  - .github/workflows/plugin-moomoo.yml
  - plugins/package.py:make_zip

## Acceptance
<!-- id: REQ-MOOMOO-014.A1 -->
- The workflow's release job has the condition `github.repository == 'selvakumarEsra/opentrader'` and publishes with `-R selvakumarEsra/opentrader`.
<!-- id: REQ-MOOMOO-014.A2 -->
- The release of a tag holds the three programs and the three zips; each program runs `--help` on its system in the workflow before it is published.
<!-- id: REQ-MOOMOO-014.A3 -->
- A catalog entry's `url` is `https://github.com/selvakumarEsra/opentrader/releases/download/plugin-moomoo-v<version>/<zip>`.
<!-- id: REQ-MOOMOO-014.A4 -->
- The release holds `SHA256SUMS` of its files, each file has a build provenance attestation of the workflow, the programs are built without a restored compiler cache, and a tag with a `-` is published as a prerelease.

<!-- id: REQ-MOOMOO-015 -->
## The plugin MUST record the "Always record" symbols for as long as the app runs

The setting "Always record" lists symbols (`US:AAPL, US:MSFT`, separated by
commas, spaces or new lines). While the app runs the plugin (it starts the
plugin when it opens and stops it when it quits), the plugin keeps them
subscribed at OpenD with the symbols the app asks for, and stores their closed
1-minute bars as `record` does, whether or not a chart shows them. They are
subscribed again after a reconnect; a changed list subscribes the new ones and
releases the ones dropped (unless the app still asks for them).

implementations:
  - plugins/moomoo/src/lib.rs:Plugin.notification
  - plugins/moomoo/src/lib.rs:record_list

verifies:
  - plugins/moomoo/src/lib.rs:always_record_folds_case_and_duplicates_and_leaves_out_others

## Acceptance
<!-- id: REQ-MOOMOO-015.A1 -->
- With "Always record" set to `US:AAPL` and no chart open, the plugin subscribes US:AAPL's 1-minute K-lines at OpenD and stores its pushed closed minutes.
<!-- id: REQ-MOOMOO-015.A2 -->
- Entries that are not the plugin's symbols (`AAPL`, `XX:NOPE`) are left out with a warning in the plugin's log; duplicates and case are folded (`us:aapl` is `US:AAPL`).

<!-- id: REQ-MOOMOO-016 -->
## The plugin MUST fetch the whole US list on its first run and keep it on disk

On its first connection to OpenD the plugin loads the whole list of US stocks,
funds and indexes (names and types, not bars: OpenD's history quota does not
allow every symbol's history) and writes it next to the bar store
(`symbols-US.json`). Later runs read that file at start, so symbol search over
US answers at once and while OpenD is down, and load it again from OpenD when
it is more than a day old. A failed load keeps the file there.

implementations:
  - plugins/moomoo/src/search.rs:Catalog
  - plugins/moomoo/src/lib.rs:Plugin.pump

## Acceptance
<!-- id: REQ-MOOMOO-016.A1 -->
- With no list file, the first connection to OpenD loads the US list and writes it; a search then finds `AAPL` without asking OpenD again.
<!-- id: REQ-MOOMOO-016.A2 -->
- A list file younger than a day is used at start without asking OpenD; an older one is used until OpenD's answer replaces it.
