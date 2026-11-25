---
id: PERF-DOC
title: Live-chart performance
owner: app [needs user confirmation]
priority: high
---

<!-- id: PERF-DOC -->
# Live-chart performance

The costs a performance review measured on long charts (100k 1-minute bars, 10k daily bars) and on busy live
symbols, and what the app does about them. Each requirement keeps what the user sees the same: the same studies,
values, reports, quotes and bars. Only where the work happens, and how often, changes.

Measured before (macOS, Apple silicon): the indicator library loaded twice, in the window and in the study worker
(about 490 ms and +230 MB each). A live tick of RSI on 100k bars took 61 ms in the worker plus an 83 ms copy of a
5.4 MB result. A live strategy tick sent the whole report again (up to 6.5 MB). Each plugin quote was its own event
in every window, and every chart pane painted its live updates at the same rate.

<!-- id: REQ-PERF-001 -->
## The window MUST hold the indicator library's metadata, not its code, unless it calculates a library study itself

The chart's library studies calculate in the study worker, which loads the library. The window lists, shows and
edits the studies from the entries' metadata (every field that is not a function). It imports the library's code
only to calculate a library study on its own thread: alerts on library studies, the script screener, the replay
of missed alerts, and a window without workers.

implementations:
  - scripts/indicator-manifest-plugin.mjs:indicatorManifest
  - src/window/chart/indicators/registry.ts:loadIndicatorLibrary
  - src/window/chart/indicators/indicator-code.ts:libraryEntriesToLoad
  - src/window/chart/indicators/indicator-code.ts:loadIndicatorCode
  - src/window/chart/indicators/indicator-code.ts:canCalculateHere
  - src/data/alert-studies.ts:runStudy
  - src/data/alert-replay-missed.ts:replayMissedOnce
  - src/data/script-screener.ts:runBuiltIn
  - src/window/chart/indicators/indicator-layer.ts:IndicatorLayer

verifies:
  - src/window/chart/indicators/__tests__/registry-meta.test.ts:holdsFunction
  - src/window/chart/indicators/__tests__/registry-lazy.test.ts

## Acceptance
<!-- id: REQ-PERF-001.A1 -->
- The metadata holds every library entry, in the library's order. No value at any depth is a function, and an entry's data equals the library entry without its functions.
<!-- id: REQ-PERF-001.A2 -->
- With workers, a loaded library entry has its metadata and no `calculate` until the code is asked for. Asking to calculate it on the window thread starts loading the code.
<!-- id: REQ-PERF-001.A3 -->
- Once the code is loaded, `getIndicatorEntry` returns the same entry object as before, now with `calculate` and `createRun`.
<!-- id: REQ-PERF-001.A4 -->
- An alert poll that reaches a library study whose code is not loaded yet gives no value for that poll, and a later poll calculates it. The replay of missed alerts loads the code only when an alert the window evaluates names a library study.
<!-- id: REQ-PERF-001.A5 -->
- Without workers, the entries come with their code.

<!-- id: REQ-PERF-002 -->
## A live tick of a study or a strategy on a long chart MUST cost about what its newest bars cost

On a tick, the study worker sends what changed since the result the window has. A resumed run calculates from its
first recalculated bar (`from`), so the points before it are known to be unchanged and are not compared one by one.
Daily bars restamped to the session open go back to the chart's times as a copy, made of what is sent only. A copy
of a built-in that exports `createRun` resumes like the built-in. A live strategy's report goes as its changes too.

A live strategy restores the broker state of its last closed bar on every tick. That state shares the items of the
lists that only grow (closed trades, fills), copying the lists only. Its drawn outputs before the time of its first
recalculated bar are the ones drawn before: a long list of timed points goes as its new points and where they go.

implementations:
  - src/window/chart/indicators/study-worker-types.ts:encodeDelta
  - src/window/chart/indicators/study-worker-types.ts:applyDelta
  - src/window/chart/indicators/study-worker-types.ts:advanceSnapshot
  - src/window/chart/indicators/delta-encodings.ts:encodeTimed
  - src/window/chart/indicators/delta-encodings.ts:splicedList
  - src/window/chart/indicators/packed-points.ts:packedPrefix
  - src/backtester/live-broker-state.ts:restoreEngineState
  - src/window/chart/indicators/study-calc.ts
  - src/backtester/live-broker-state.ts:saveEngineState
  - src/backtester/live-broker-state.ts:resumedBound
  - src/backtester/oakscript.ts:createStrategyRun
  - src/backtester/chart-times-copy.ts:chartTimesCopy
  - src/window/oakscript/live-script.ts:updateLiveScript
  - src/backtester/live-runs.ts:KeptRuns.keep
  - src/backtester/live-runs.ts:updateLiveStrategy
  - src/backtester/client.ts
  - src/window/oakscript/engine.ts

verifies:
  - src/window/chart/indicators/__tests__/study-calc-live.test.ts:fullOnChart
  - src/window/oakscript/__tests__/live-script.test.ts:fullRun

## Acceptance
<!-- id: REQ-PERF-002.A1 -->
- After a feed of ticks and a new bar, the window's result of RSI, Ichimoku, MACD and VWAP equals a full calculation on the same bars, on 1-minute bars and on restamped daily bars.
<!-- id: REQ-PERF-002.A2 -->
- On 3,000 bars, a tick's message is under a tenth of the full result and under 20 KB.
<!-- id: REQ-PERF-002.A3 -->
- A user script module that exports `createRun` and `calculate` (a copy of a built-in) gives a full calculation's result after each tick, intraday and restamped daily. It reports no full-run reason after its first update.
<!-- id: REQ-PERF-002.A4 -->
- A live strategy's report and drawn outputs, rebuilt from the changes in the window, equal a full run's. A tick's report message is under a fifth of the whole report.
<!-- id: REQ-PERF-002.A5 -->
- With the broker state restored on every tick, a shipped strategy's report equals a full run's, on intraday and daily bars, and on margin.
<!-- id: REQ-PERF-002.A6 -->
- A shipped strategy's markers and background colours (several calls in one list each), rebuilt in the window, equal a full run's. A tick's message is under a tenth of the whole result.

<!-- id: REQ-PERF-003 -->
## Plugin quotes MUST reach the views batched, and a plugin's bars only the windows that chart the symbol

A data plugin's quotes go to the views the way the app's own stream's do: the newest bar per symbol about once a
display frame (16 ms), the newest trade per symbol every 100 ms. A bar whose minute is over is sent at once, with its
last values. The host's alert engine still gets every quote as it comes. A bar goes only to the windows whose charts
show its symbol, the bar event's one reader. Trades go to every window, whose watchlists, panels and alerts read
them.

implementations:
  - src-tauri/src/plugins/data.rs:on_quote
  - src-tauri/src/plugins/quote_batch.rs:QuoteBatch
  - src-tauri/src/plugins/quote_batch.rs:flush
  - src-tauri/src/plugins/quote_batch.rs:chart_windows

verifies:
  - src-tauri/src/plugins/quote_batch.rs:quotes_are_sent_newest_per_symbol_per_flush_closed_bars_at_once
  - src-tauri/src/plugins/quote_batch.rs:a_bar_goes_to_the_windows_that_chart_its_symbol

## Acceptance
<!-- id: REQ-PERF-003.A1 -->
- Several quotes of a symbol within one flush send one bar and one trade, the newest.
<!-- id: REQ-PERF-003.A2 -->
- A quote of a new minute sends the previous minute's bar at once, with that minute's last values. Trades wait for their 100 ms interval, and bars go out every flush.
<!-- id: REQ-PERF-003.A3 -->
- A bar's windows are those with a chart pane on its symbol, each once. A symbol no chart shows has none.

<!-- id: REQ-PERF-004 -->
## A chart pane that is not focused MUST paint its live updates less often

A pane paints a live update at most every 33 ms when it is the focused pane, or the only one, and at most every
200 ms when it is not. Neither paints while the page is hidden. The paint is not on an animation frame of its own: the
chart library draws on the next one, and a frame of ours before it would be a second frame per update (#249).

implementations:
  - src/window/chart/view/live.ts:createLive
  - src/window/chart/view/live-paint-gap.ts:paintGap
  - src/window/chart/view/live-paint-gap.ts:paintWait

verifies:
  - src/window/chart/view/__tests__/live-paint-gap.test.ts:LIVE

## Acceptance
<!-- id: REQ-PERF-004.A1 -->
- A pane that is not focused paints its live updates at most every 200 ms; the focused pane, or the only one, every 33 ms.
