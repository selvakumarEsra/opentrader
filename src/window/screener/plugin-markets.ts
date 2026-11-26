/*
 * The Stock Screener's markets served by data plugins (specs/screener-plugin-markets.md REQ-SCREEN-MKT-001): India
 * from the Zerodha plugin, USA (moomoo) from the moomoo plugin, listed after the app's own USA while their plugin is
 * enabled. A screen keeps its market's key (`zerodha:IN`) in `Screen.market`; unset is the app's USA.
 */
import { createSignal } from "solid-js";
import { commands, events, type PluginMarket } from "../../bindings";
import type { Screen } from "../../data/screener-catalog";
import { translate, translateWith } from "../../i18n/translator";

const [list, setList] = createSignal<PluginMarket[] | null>(null);
let started = false;

function load(): void {
  void commands.pluginScreenerMarkets().then(setList, () => setList([]));
}

/** The plugin markets the enabled plugins serve, India first (empty until read). */
export function pluginMarkets(): PluginMarket[] {
  if (!started) {
    started = true;
    load();
    void events.pluginsChanged.listen(() => load());
  }
  const order = (m: PluginMarket) => (m.market === "IN" ? 0 : 1);
  return [...(list() ?? [])].sort((a, b) => order(a) - order(b) || a.key.localeCompare(b.key));
}

/** The market list has been read from the backend. */
export const pluginMarketsLoaded = (): boolean => (pluginMarkets(), list() !== null);

/** A market as the picker shows it: its name, the pill's text and its flag icon. */
export type MarketView = { key: string | null; title: string; short: string; flag: string };

export const USA: MarketView = { key: null, title: "USA", short: "US", flag: "scr-flag-us" };

/** A market's name: India, USA (moomoo) (the dictionaries have these), or `USA (<plugin>)`. */
function marketName(market: string, plugin: string, pluginName: string): string {
  if (market === "IN") return "India";
  if (plugin === "moomoo") return "USA (moomoo)";
  return `${market === "US" ? "USA" : market} (${pluginName})`;
}

export function viewOf(m: PluginMarket): MarketView {
  const title = marketName(m.market, m.plugin, m.pluginName);
  return { key: m.key, title, short: title, flag: m.market === "IN" ? "scr-flag-in" : "scr-flag-us" };
}

/** The plugin market a screen shows: its saved market while a plugin serves it, else null (the app's USA). */
export function activeMarket(s: Pick<Screen, "market">): string | null {
  if (!s.market) return null;
  return pluginMarkets().some((m) => m.key === s.market) ? s.market : null;
}

/** The market the screen shows, as the picker names it. */
export function activeView(s: Pick<Screen, "market">): MarketView {
  const key = activeMarket(s);
  const m = key ? pluginMarkets().find((x) => x.key === key) : undefined;
  return m ? viewOf(m) : USA;
}

/** Why a saved screen shows USA in place of its market: its plugin is no longer enabled (REQ-SCREEN-MKT-001.A3). */
export function missingMarketNotice(s: Pick<Screen, "market">): string | null {
  if (!s.market || !pluginMarketsLoaded() || activeMarket(s)) return null;
  const [plugin = "", market = ""] = s.market.split(":");
  const pluginName = plugin === "zerodha" ? "Zerodha" : plugin;
  return translateWith("The {0} market needs the {1} plugin: showing USA. Turn it on in Settings > Plugins.", translate(marketName(market, plugin, pluginName)), pluginName);
}
/** A plugin market's rows time in the status line: dd/mm/yyyy HH:MM:SS (REQ-SCREEN-MKT-002.A3). */
export function formatDateTime(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getDate())}/${p(d.getMonth() + 1)}/${d.getFullYear()} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}
