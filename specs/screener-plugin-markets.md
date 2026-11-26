---
id: SCREEN-MKT-DOC
title: Screener markets from data plugins (India from Zerodha, USA from moomoo)
owner: screener [needs user confirmation]
priority: medium
---

<!-- id: SCREEN-MKT-DOC -->
# Screener markets from data plugins

The Stock Screener serves one market, USA, from the app's own full-market data: live quotes, the reference list
(name, type, exchange) and a nightly state file with fundamentals and technicals
(`src-tauri/crates/core/src/screener/`). Its Market pill (`src/window/screener/FilterPills.tsx`) lists USA only.

This document adds markets served by data plugins:
- **India**, from the Zerodha plugin: NSE and BSE cash-market stocks.
- **USA (moomoo)**, from the moomoo plugin: moomoo's US stock list.

The existing USA market stays as it is, on the app's own data. A plugin market holds the columns its plugin can give.
The others are empty for it. Zerodha's API has quotes but no fundamentals (no market cap, P/E, EPS or sector). So
India's technical columns are computed on this computer from the daily bars in the Zerodha plugin's bar store
(REQ-INDIA-003). moomoo's snapshot has quotes and some fundamentals.

Out of scope:
- Fundamentals for India.
- Technical ratings (Recommend.*) and candlestick patterns for plugin markets.
- Markets of other plugins (dhan).
- Alerts and watchlist pages built on a plugin market.

<!-- id: REQ-SCREEN-MKT-001 -->
## The Market picker MUST list the plugin markets that can be served, beside USA

The Market popover lists USA first. India follows when the Zerodha plugin is installed and enabled for chart data,
and USA (moomoo) when the moomoo plugin is. Each entry has its flag and name. Choosing an entry shows that market's
rows. The pill then shows its flag and name. The choice is saved with the screen and restored when the screen
opens again. A saved screen whose market's plugin is no longer enabled opens on USA and says why. "Primary listing"
stays in the popover for every market (REQ-SCREEN-MKT-003).

implementations:
  - src/window/screener/FilterPills.tsx:FilterPills
  - src/window/screener/plugin-markets.ts:pluginMarkets

## Acceptance
<!-- id: REQ-SCREEN-MKT-001.A1 -->
- With both plugins enabled, the popover lists USA, India and USA (moomoo), in that order. With neither enabled, it lists USA only, as today.
<!-- id: REQ-SCREEN-MKT-001.A2 -->
- Choosing India shows Indian stocks. The pill reads "India" with the Indian flag. Closing and reopening the saved screen keeps India.
<!-- id: REQ-SCREEN-MKT-001.A3 -->
- Disabling the Zerodha plugin, then opening a screen saved on India, opens it on USA with a notice that the India market needs the Zerodha plugin.
<!-- id: REQ-SCREEN-MKT-001.A4 -->
- The names "India" and "USA (moomoo)" and the notice are translated in every language the app ships.

<!-- id: REQ-SCREEN-MKT-002 -->
## A data plugin MUST be able to serve a screener market's rows

A data plugin declares the screener markets it serves in its manifest. While a screener panel shows such a market,
the app asks the plugin for the market's rows: every listed instrument with the fields the plugin has. Each field
uses the screener's existing field id where one exists. The plugin says which fields it gives, and when each row's
values were taken. Rows are asked for again at the plugin's refresh interval. The plugin stays within its broker's
rate limits, leaving room for the charts' own requests. A plugin that fails to answer leaves the last rows shown,
with the error and its time in the screener's status. When no panel shows the market, the app stops asking.
The protocol change is written in `plugins/PROTOCOL.md`.

implementations:
  - src-tauri/src/plugins/screener.rs:PluginMarkets
  - plugins/kit/src/feed.rs:Feed
  - plugins/PROTOCOL.md

## Acceptance
<!-- id: REQ-SCREEN-MKT-002.A1 -->
- A plugin whose manifest declares market `IN` is asked for its rows only while a panel shows India. It is not asked again after the last such panel closes.
<!-- id: REQ-SCREEN-MKT-002.A2 -->
- A field the plugin does not give is empty in every row of its market. Filtering or sorting on it matches nothing, and the field's pill and column show it is not available for this market.
<!-- id: REQ-SCREEN-MKT-002.A3 -->
- A refresh that fails keeps the previous rows. The status shows the error and when the rows were last updated (dd/mm/yyyy, time).
<!-- id: REQ-SCREEN-MKT-002.A4 -->
- Filtering, sorting and scrolling a plugin market read rows the app already holds. They never wait on the plugin.

<!-- id: REQ-SCREEN-MKT-003 -->
## The India market MUST list NSE and BSE stocks with live quotes from the Zerodha plugin

The rows are the cash-market equities (no indices, ETFs, futures or options) of Zerodha's NSE and BSE instrument
lists. Each row has: symbol (`NSE:RELIANCE`), name, exchange, price, change, change %, volume, the day's open,
high and low, and the previous close. Values are in INR. Quotes are refreshed at least every 30 s while the market
is open. Outside market hours, the last session's values stay. With "Primary listing" on, a company listed on both
exchanges shows once, as its NSE row. A company listed on BSE only keeps its BSE row. Without Zerodha credentials,
the rows list the instruments with empty quote fields, and the status says to log in to Zerodha
(Settings > Plugins > Zerodha).

implementations:
  - plugins/zerodha/src/lib.rs:Zerodha
  - plugins/zerodha/src/screen.rs:rows

## Acceptance
<!-- id: REQ-SCREEN-MKT-003.A1 -->
- India lists `NSE:RELIANCE` and `BSE:RELIANCE`, and no `NFO:` or index rows. With Primary listing on, it lists `NSE:RELIANCE` only.
<!-- id: REQ-SCREEN-MKT-003.A2 -->
- A BSE-only stock is listed with Primary listing on.
<!-- id: REQ-SCREEN-MKT-003.A3 -->
- During market hours, a stock's price and change % in the screener are at most 30 s older than Kite's quote.
<!-- id: REQ-SCREEN-MKT-003.A4 -->
- Price columns of India rows show INR.
<!-- id: REQ-SCREEN-MKT-003.A5 -->
- With no Zerodha credentials, India lists the instruments, the quote columns are empty, and the status names Settings > Plugins > Zerodha.

<!-- id: REQ-SCREEN-MKT-004 -->
## India's technical columns MUST be computed from the daily bars in the Zerodha plugin's bar store

For each India row, the plugin computes from its stored daily bars, stepped with the live quote:
- performance % over 1 week, 1 month, 3 months, 6 months, year to date and 1 year,
- simple and exponential moving averages of 10, 20, 50, 100 and 200 days,
- RSI (14), average volume over 10, 30 and 90 days, relative volume,
- the 52-week high and low.

Missing daily history is fetched in the background, within Kite's history rate limit, starting with NSE rows.
While it fills, the status shows how many stocks have their history. A stock's technical columns stay empty until
it has enough bars for that value. Each day after the close, the new daily bar is added for every row.

implementations:
  - plugins/kit/src/screen.rs:technicals
  - plugins/zerodha/src/screen.rs:rows

## Acceptance
<!-- id: REQ-SCREEN-MKT-004.A1 -->
- For a stock with 250 stored daily closes, SMA 50, EMA 20, RSI 14 and 1-month performance equal the values computed from those closes with the live price as the newest close, to the cent.
<!-- id: REQ-SCREEN-MKT-004.A2 -->
- A stock listed 30 sessions ago has SMA 20 and 1-month performance, and empty SMA 50/200 and 1-year performance.
<!-- id: REQ-SCREEN-MKT-004.A3 -->
- On a first run with an empty store, the status counts the stocks with history up to all of them. The history requests never exceed Kite's 3 per second.
<!-- id: REQ-SCREEN-MKT-004.A4 -->
- After a market close, the next refresh has that session's daily bar in the store for every listed stock that traded.

<!-- id: REQ-SCREEN-MKT-005 -->
## USA (moomoo) MUST list moomoo's US stocks with its snapshot values

The rows are the US stocks of the moomoo plugin's US list (REQ-MOOMOO-016). Each row has: symbol, name, exchange,
price, change, change %, volume, the day's open, high and low, the previous close, market cap, P/E (TTM), P/B,
EPS (TTM), dividend yield, and the 52-week high and low, as OpenD's snapshot gives them, in USD. Values are
refreshed at least every 60 s while a US session (pre-market, regular or post-market) is open. OpenD's snapshot
requests stay at most half of its limit. When OpenD is down, the last rows stay and the status says OpenD is not
connected.

implementations:
  - plugins/moomoo/src/screen.rs:rows
  - plugins/moomoo/src/lib.rs:Moomoo

## Acceptance
<!-- id: REQ-SCREEN-MKT-005.A1 -->
- USA (moomoo) lists `US:AAPL` with price, market cap and P/E filled in. Filtering Mkt cap 10 B to 200 B USD matches the moomoo rows in that range.
<!-- id: REQ-SCREEN-MKT-005.A2 -->
- Over 30 s of refreshes, OpenD's snapshot requests stay at or under half of its limit.
<!-- id: REQ-SCREEN-MKT-005.A3 -->
- With OpenD closed, the last rows stay and the status says OpenD is not connected. When OpenD is back, the rows refresh without reopening the screener.
<!-- id: REQ-SCREEN-MKT-005.A4 -->
- The existing USA market's rows and columns are unchanged with the moomoo plugin enabled.
