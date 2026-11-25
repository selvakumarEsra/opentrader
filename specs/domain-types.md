---
id: DOMAIN-DOC
title: Domain types for symbols and bar intervals
owner: core [needs user confirmation]
priority: medium
version: 1
---

<!-- id: DOMAIN-DOC -->
# Domain types for symbols and bar intervals

The Rust side passes symbols and bar sizes around as strings. A symbol is
normalised in three different ways, and the unit of a bar is matched against
`"second"` / `"minute"` / `"day"` in many places. One match treats an unknown
unit as minutes before a second match rejects it. This document replaces those
strings with two types in `opentrader_core::data`. Each value is parsed once,
where it enters the program, and its meaning is fixed from then on.

Out of scope: the frontend, the plugin JSON-RPC wire format (`plugins/PROTOCOL.md`)
and the gateway's own REST tokens, which stay as they are. The commands keep
their wire shape: a bad value is now refused when it is read, instead of deep
inside a handler.

<!-- id: REQ-DOMAIN-001 -->
## A symbol MUST be one normalised `Symbol` value

`Symbol` takes the place of `SymbolRef`. It is an exchange and a ticker. Its
fields are private, so every value comes from the normalising constructor:
trimmed and uppercased, `"EXCHANGE:TICKER"`, with `""` as the exchange of a
bare ticker. It displays as its full name without allocating. It reads and
writes as that full-name string.

implementations:
  - src-tauri/crates/core/src/data/symbol.rs:Symbol
  - src-tauri/crates/core/src/commands.rs:live_symbol

## Acceptance
<!-- id: REQ-DOMAIN-001.A1 -->
- `Symbol::parse(" nse:reliance ")` has exchange `NSE`, ticker `RELIANCE` and displays `NSE:RELIANCE`; `Symbol::parse("aapl")` has exchange `""` and displays `AAPL`.
<!-- id: REQ-DOMAIN-001.A2 -->
- A `Symbol` serialises to the JSON string `"NSE:RELIANCE"`, and deserialising that string gives an equal `Symbol`.
<!-- id: REQ-DOMAIN-001.A3 -->
- `Symbol::with_ticker` keeps the exchange and normalises the new ticker (used for a renamed listing's earlier ticker).
<!-- id: REQ-DOMAIN-001.A4 -->
- `live_symbol(Some(" nasdaq:aapl"))` returns `NASDAQ:AAPL`: live subscriptions normalise exactly as `Symbol` does. An economic series still returns `None`.

<!-- id: REQ-DOMAIN-002 -->
## A bar size MUST be one `Interval` value

`Interval` is a unit (`BarUnit::Second`, `Minute` or `Day`) and a multiplier
of at least 1. `BarUnit` takes the place of `capabilities::BarFamily`, which
named the same three units. `Interval` knows its length in seconds and how many
bars make one trading day. It reads and writes the plugin resolution grammar:
`"15S"`, `"5"`, `"1D"`.

implementations:
  - src-tauri/crates/core/src/data/interval.rs:Interval
  - src-tauri/crates/core/src/data/interval.rs:BarUnit

## Acceptance
<!-- id: REQ-DOMAIN-002.A1 -->
- `"15S"`, `"5"`, `"60"` and `"1D"` parse as 15 seconds, 5 minutes, 60 minutes and 1 day. Case is ignored for the suffix.
<!-- id: REQ-DOMAIN-002.A2 -->
- `""`, `"0"`, `"0S"`, `"abc"`, `"1W"` and `"2D"` are refused with an error. Weeks and months are built by the app from days, so no `Interval` holds them.
<!-- id: REQ-DOMAIN-002.A3 -->
- `Interval::resolution()` gives back the grammar (`seconds(15)` → `"15S"`, `minutes(5)` → `"5"`, `day()` → `"1D"`), and parsing that string gives the same `Interval`.
<!-- id: REQ-DOMAIN-002.A4 -->
- `Interval::secs()` is 15 for `"15S"`, 300 for `"5"` and 86 400 for `"1D"`. `bars_per_day()` is 5 760, 288 and 1.
<!-- id: REQ-DOMAIN-002.A5 -->
- `BarUnit` serialises as `"second"`, `"minute"`, `"day"`, the existing wire words, so the frontend's arguments are unchanged.

<!-- id: REQ-DOMAIN-003 -->
## Bars before a time MUST refuse an unsupported unit before any work

`get_aggregates_before` takes a `BarUnit`. A unit it does not serve (`Day`;
daily history has its own command) is refused with an invalid-argument error
before the calendar or entitlements are read. A word that is not a unit at all
is refused when the arguments are read.

implementations:
  - src-tauri/crates/core/src/history/mod.rs:get_aggregates_before
  - src-tauri/crates/core/src/commands.rs:AggregatesBefore

## Acceptance
<!-- id: REQ-DOMAIN-003.A1 -->
- Arguments with `"timespan":"weird"` fail to deserialise. Before this change they were treated as minutes for the history-floor lookup.
<!-- id: REQ-DOMAIN-003.A2 -->
- `BarUnit::Day` returns an `invalid` error and makes no provider call.

<!-- id: REQ-DOMAIN-004 -->
## Plugin bars MUST be asked for with an `Interval`

The desktop's plugin data path (`src-tauri/src/plugins/data.rs`) takes an
`Interval` in `bars`, `recent` and `before`. It no longer takes a unit word and
a multiplier. Choosing a resolution the plugin serves, and rebuilding larger
bars from it, is unchanged.

implementations:
  - src-tauri/src/plugins/data.rs:pick
  - src-tauri/src/plugins/data.rs:bars
  - src-tauri/src/plugins/data.rs:recent
  - src-tauri/src/plugins/data.rs:before

## Acceptance
<!-- id: REQ-DOMAIN-004.A1 -->
- With served `["1","5","15","60","1D"]`: 5 minutes picks `"5"` as served; 45 minutes picks `"15"` rebuilt to 2 700 s; 7 minutes picks `"1"` rebuilt to 420 s; 1 day picks `"1D"`; 5 seconds picks nothing.
<!-- id: REQ-DOMAIN-004.A2 -->
- With served `["60"]`: 240 minutes picks `"60"` rebuilt to 14 400 s, and 30 minutes picks nothing.
