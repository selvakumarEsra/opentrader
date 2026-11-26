---
id: BAR-STORE-DOC
title: Compact bar store file
owner: plugins
priority: medium
---

<!-- id: BAR-STORE-DOC -->
# Compact bar store file

The data plugins (moomoo, zerodha, dhan) keep their OHLCV bars in one SQLite file each, `bars.sqlite` in the plugin's
cache folder (`plugins/kit/src/store/sqlite.rs`). Its rows repeat the series' whole name (`US:AAPL|1|ext`) and hold
every price as an 8-byte decimal: a real moomoo store took 71 bytes per bar (450,753 bars, 32 MB), and 500 symbols with
2 years of 1-minute bars would take about 16 GB. This document makes the file compact and says how existing files
are moved over. Measured on 400,000 synthetic 1-minute bars, the old layout took 62 bytes per bar. A layout with the
series as a number and prices as whole price steps took 25.

Out of scope: the ClickHouse backend, and loading a series whole into memory (`Store`), which stay as they are.
The store's behaviour (REQ-MOOMOO-007, REQ-INDIA-003, REQ-INDIA-004) does not change: only the file's layout does.

<!-- id: REQ-BAR-STORE-001 -->
## The bar store MUST give back exactly the bars and covered ranges written to it

A bar read back has the same start time, open, high, low, close and volume, bit for bit, as when it was written, and
a series' covered ranges are the same ranges. This holds for prices on the symbol's price step and for prices off it
(split-adjusted prices are not on the step), for volumes with decimals, and for every series of a file at once.

implementations:
  - plugins/kit/src/store/sqlite.rs:Sqlite
  - plugins/kit/src/store/sqlite.rs:steps
  - plugins/kit/src/store/sqlite.rs:price_of

verifies:
  - plugins/kit/src/store/sqlite.rs:bars_come_back_exactly_as_written
  - plugins/kit/src/store/sqlite.rs:prices_are_whole_steps_only_when_they_come_back_exactly

## Acceptance
<!-- id: REQ-BAR-STORE-001.A1 -->
- Bars with prices on a 0.01 step (150.25), off any step (150.123456789, as after a split adjustment), and with a decimal volume (0.5) are each read back by a new store, opened on the same file, equal to the written values bit for bit.
<!-- id: REQ-BAR-STORE-001.A2 -->
- A series' covered ranges, and a series dropped and refilled (adjusted prices changed), read back as written.
<!-- id: REQ-BAR-STORE-001.A3 -->
- Two series of one symbol that differ only in resolution or session (`US:AAPL|1|ext`, `US:AAPL|1|rth`, `US:AAPL|5|ext`) keep their bars apart.

<!-- id: REQ-BAR-STORE-002 -->
## The bar store file MUST take at most half the space per bar of the old layout

For bars whose prices lie on a decimal price step, the file takes at most half the bytes per bar of the old layout
holding the same bars. For bars off any step it takes no more than the old layout. Sizes are compared after the file
has been compacted (no free pages).

implementations:
  - plugins/kit/src/store/sqlite.rs:Sqlite
  - plugins/kit/src/store/sqlite.rs:steps

verifies:
  - plugins/kit/src/store/sqlite.rs:the_compact_file_takes_half_the_space_or_less

## Acceptance
<!-- id: REQ-BAR-STORE-002.A1 -->
- 400,000 consecutive 1-minute bars of one series with prices on a 0.01 step take at most half the bytes of the same bars in the old layout.
<!-- id: REQ-BAR-STORE-002.A2 -->
- The same number of bars with prices off any step take no more bytes than in the old layout.

<!-- id: REQ-BAR-STORE-003 -->
## An existing bars.sqlite MUST be converted in place the first time a new plugin opens it

When the plugin, or its `backfill` or `record` command, opens a file in the old layout, it converts the file to the
compact one in place, keeping every bar and covered range, with nothing for the user to do. The conversion happens
whole or not at all. When it cannot finish, the file stays in the old layout, unchanged. The plugin then logs why,
answers from a store in memory for that run, and converts the file at its next start. A second process opening the
file while it is being converted waits for the conversion, then opens the converted file. After the conversion the
file's size on disk is that of the compact layout, not the old size.

implementations:
  - plugins/kit/src/store/sqlite.rs:Sqlite
  - plugins/kit/src/store/sqlite.rs:to_compact

verifies:
  - plugins/kit/src/store/sqlite.rs:an_old_file_is_converted_in_place
  - plugins/kit/src/store/sqlite.rs:a_failed_conversion_leaves_the_old_file
  - plugins/kit/src/store/sqlite.rs:two_openers_convert_once
  - plugins/kit/src/store/sqlite.rs:the_compact_file_takes_half_the_space_or_less
  - plugins/kit/src/store/sqlite.rs:a_million_bars_convert_in_under_30_seconds

## Acceptance
<!-- id: REQ-BAR-STORE-003.A1 -->
- A file in the old layout with several series, bars and covered ranges, opened by the new store, reads back every series exactly as the old store read it (REQ-BAR-STORE-001).
<!-- id: REQ-BAR-STORE-003.A2 -->
- A conversion stopped part way (the process killed, or a write refused) leaves the file in the old layout, readable by the old store with all its bars. The next open converts it.
<!-- id: REQ-BAR-STORE-003.A3 -->
- The plugin's log names the conversion, the number of series and bars moved, and the time it took. A failed conversion logs its reason.
<!-- id: REQ-BAR-STORE-003.A4 -->
- Two processes opening one old-layout file at the same time convert it once, and both then read the same bars.
<!-- id: REQ-BAR-STORE-003.A5 -->
- After converting a file whose bars are on a price step, its size on disk is at most about half its old size (REQ-BAR-STORE-002).
<!-- id: REQ-BAR-STORE-003.A6 -->
- Converting 1,000,000 bars takes less than 30 s on a recent laptop.

<!-- id: REQ-BAR-STORE-004 -->
## A bar store file MUST name its layout, and no plugin MUST use a file of a layout it does not know

The file records its layout version. A plugin opening a file of a newer layout than it knows does not read, write or
convert it: it logs that the plugin must be updated, and answers from a store in memory for that run. A plugin
version from before this change, opening a converted file, must not read or write its bars. Such a version cannot be
changed, so the compact layout is made so that its reads and writes fail. They must not succeed on empty or wrong
tables.

implementations:
  - plugins/kit/src/store/sqlite.rs:Sqlite
  - plugins/kit/src/store/sqlite.rs:to_compact

verifies:
  - plugins/kit/src/store/sqlite.rs:a_newer_layout_is_left_alone
  - plugins/kit/src/store/sqlite.rs:the_old_store_cannot_use_a_converted_file

## Acceptance
<!-- id: REQ-BAR-STORE-004.A1 -->
- A file marked with a layout version above the plugin's is left byte for byte unchanged by opening it, and the plugin's log asks for a plugin update.
<!-- id: REQ-BAR-STORE-004.A2 -->
- The old store's code (as in plugin kit 0.1.0 / moomoo 0.2.4) opening a converted file gets an error for both reading and writing a series, and the file's bars are unchanged afterwards.
