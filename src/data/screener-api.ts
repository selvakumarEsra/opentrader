/*
 * Stock screener backend calls (Rust `screener_*` commands). The backend
 * polls the market every 10 s while at least one owner has the screener open
 * and emits `screener-update` after every poll; scans run on its in-memory
 * table.
 *
 * Each call takes an optional plugin market (`zerodha:IN`, window/screener/plugin-markets.ts): its rows are the
 * plugin's (`plugin_screener_*`); none is the app's USA.
 */
import { commands, events, type FieldInfo, type PluginMarketStatus, type ScanRequest, type ScanResult, type ScreenerProgress, type ScreenerStatus, type ScreenerUpdate } from "../bindings";
import { isCustomField } from "./screener-catalog";

async function unwrap<T>(p: Promise<{ status: "ok"; data: T } | { status: "error"; error: string }>): Promise<T> {
  const r = await p;
  if (r.status === "error") throw new Error(r.error);
  return r.data;
}

type Market = string | null | undefined;

/** A plugin market's status as the app's own. */
const statusOf = (s: PluginMarketStatus): ScreenerStatus => ({ version: 0, updatedMs: s.updatedMs, total: s.rows, stateAsof: null, complete: s.updatedMs !== null });

/** Start polling for `owner` (window label + ":screener"). */
export const screenerOpen = (owner: string, market?: Market): Promise<ScreenerStatus> =>
  market ? unwrap(commands.pluginScreenerOpen(market, owner)).then(statusOf) : unwrap(commands.screenerOpen(owner));
/** Stop polling for `owner`. */
export const screenerClose = (owner: string, market?: Market): Promise<null> =>
  market ? commands.pluginScreenerClose(market, owner).then(() => null) : unwrap(commands.screenerClose(owner));
/** A plugin market's rows, status and error (the screener's status line). */
export const pluginMarketStatus = (market: string): Promise<PluginMarketStatus> => commands.pluginScreenerStatus(market);
/** Scan the market table. The custom lengths it reads ("EMA137") are asked for first: the backend computes them
 *  from the closes file and they hold values from its next update (null before). */
export const screenerScan = async (req: ScanRequest, market?: Market): Promise<ScanResult> => {
  if (market) return unwrap(commands.pluginScreenerScan(market, req));
  const custom = customFieldsOf(req).filter((f) => !askedCustom.has(f));
  if (custom.length) {
    for (const f of custom) askedCustom.add(f);
    await unwrap(commands.screenerCustomFields(custom)).catch((e) => console.warn("[screener] custom lengths", e));
  }
  return unwrap(commands.screenerScan(req));
};

const askedCustom = new Set<string>();

/** Custom-length field ids a scan reads: columns, filter sides, sort. */
export function customFieldsOf(req: ScanRequest): string[] {
  const ids = new Set<string>();
  const visit = (v: unknown) => {
    if (typeof v === "string" && isCustomField(v)) ids.add(v);
    else if (Array.isArray(v)) v.forEach(visit);
  };
  for (const c of req.columns) visit(c);
  for (const cl of req.filter) {
    visit(cl.left);
    visit(cl.right);
  }
  if (req.sort) visit(req.sort.sortBy);
  return [...ids];
}
/** Download progress of what the next complete table waits on (the loading wheel). */
export const screenerProgress = (market?: Market): Promise<ScreenerProgress> =>
  market ? Promise.resolve({ receivedBytes: null, totalBytes: null, downloading: [] }) : unwrap(commands.screenerProgress());
export const screenerFields = (market?: Market): Promise<FieldInfo[]> =>
  market ? commands.pluginScreenerFields(market) : unwrap(commands.screenerFields());
/** Distinct values of a text field (sector, industry) in the loaded data. */
export const screenerValues = (field: string, market?: Market): Promise<string[]> =>
  market ? unwrap(commands.pluginScreenerValues(market, field)) : unwrap(commands.screenerValues(field));

/** Subscribe to the per-poll update event (of `market`, when given). Returns the unlisten function. */
export function onScreenerUpdate(cb: (u: ScreenerUpdate) => void, market?: Market): Promise<() => void> {
  if (market) {
    return events.pluginScreenerUpdate.listen((e) => {
      if (e.payload.key !== market) return;
      void commands.pluginScreenerStatus(market).then((s) =>
        cb({ version: e.payload.version, updatedMs: s.updatedMs, total: s.rows, stateAsof: null, complete: s.updatedMs !== null, error: s.error }),
      );
    });
  }
  return events.screenerUpdate.listen((e) => cb(e.payload));
}

/** Backend poll period (the refresh countdown restarts at every update). */
export const POLL_MS = 10_000;
