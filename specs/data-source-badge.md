---
id: DATA-BADGE-DOC
title: Data source badge
owner: shell [needs user confirmation]
priority: medium
---

<!-- id: DATA-BADGE-DOC -->
# Data source badge

The title bar shows a badge between the tabs and the window controls (`data-delay-badge`). Until now it always said
"Delayed 15 min" (or the paid plan's state), from the app's own market data alone. A chart whose symbol comes from a
data plugin (`plugins/PROTOCOL.md`, capability `datafeed`) does not get the app's data, so that text was wrong for it.
The app cannot tell how late a plugin's data is: the plugin protocol does not say.

<!-- id: REQ-DATA-BADGE-001 -->
## The badge MUST name a data plugin as the source when the focused chart's symbol comes from one

A symbol comes from a data plugin when its resolved sessions list the intervals that plugin serves. The badge follows
the focused pane of the active tab.

implementations:
  - src/window/shell/data-source-badge.ts:dataBadge
  - src/window/shell/data-source-badge.ts:servedByPlugin
  - src/window/shell/TabPanelActions.tsx:TabPanelActions

verifies:
  - src/window/shell/__tests__/data-source-badge.test.ts:base

## Acceptance
<!-- id: REQ-DATA-BADGE-001.A1 -->
- With the focused chart's symbol from a data plugin, the badge reads "Plugin data". Its tooltip says the data comes from a data plugin and that any delay is the plugin's. It shows no delay of the app's own.
<!-- id: REQ-DATA-BADGE-001.A2 -->
- With the paid tier, a plugin symbol's badge stays a button that opens the plan dialog, since the plan still covers the app's own data.
<!-- id: REQ-DATA-BADGE-001.A3 -->
- With any other symbol, or before the symbol's sessions are resolved, the badge is as before: "Delayed 15 min", or "Real-time" / "Real-time elsewhere" with the paid tier.
<!-- id: REQ-DATA-BADGE-001.A4 -->
- Focusing another pane, or switching to a tab whose focused chart has a symbol from elsewhere, changes the badge to match without a reload.
