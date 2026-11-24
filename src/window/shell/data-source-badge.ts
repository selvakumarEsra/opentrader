/*
 * The title bar's data badge (specs/data-source-badge.md REQ-DATA-BADGE-001): the app's own data's delay or plan,
 * or, for a focused chart whose symbol a data plugin serves, that plugin as the source. The app cannot know a
 * plugin's delay (the plugin protocol does not say), so it shows none.
 *
 * Kept apart from TabPanelActions so upstream's badge stays as it is there.
 */
import { cachedSymbolSessions } from "../../data/session/symbol-sessions";

export type DataBadge = { label: string; tip: string; realtime: boolean };

/** Whether `symbol`'s bars come from a data plugin: its resolved sessions list the intervals it serves (reactive;
 *  false until they are resolved). */
export function servedByPlugin(symbol: string | null | undefined): boolean {
  return !!symbol && !!cachedSymbolSessions(symbol)?.intervals.length;
}

/** The badge's text and tooltip. `paid`: the paid tier is on (the badge opens the plan dialog). */
export function dataBadge(s: { plugin: boolean; paid: boolean; realtimeActive: boolean; realtimeElsewhere: boolean }): DataBadge {
  if (s.plugin) {
    return {
      label: "Plugin data",
      tip: s.paid
        ? "This chart's data comes from a data plugin; any delay is the plugin's. Click for the app's own data plan."
        : "This chart's data comes from a data plugin; any delay is the plugin's.",
      realtime: false,
    };
  }
  if (!s.paid) return { label: "Delayed 15 min", tip: "Market data is delayed by 15 minutes.", realtime: false };
  if (s.realtimeActive) return { label: "Real-time", tip: "Market data is real-time.", realtime: true };
  if (s.realtimeElsewhere) return { label: "Real-time elsewhere", tip: "Real-time data is in use on another device. Click to use it here.", realtime: false };
  return { label: "Delayed 15 min", tip: "Market data is delayed by 15 minutes. Click for real-time data.", realtime: false };
}
