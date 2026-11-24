/*
 * The indicator library's entries for the window (specs/performance.md REQ-PERF-001).
 *
 * The chart's studies calculate in the study worker, which loads the library itself. The window lists, shows and
 * edits them from their metadata (`virtual:indicator-meta`: every field that is not a function), and imports the
 * library's code only to calculate a study on its own thread: alerts, the script screener, the replay of missed
 * alerts, a window without workers. The code goes onto the same entry objects, so what holds one keeps it.
 *
 * Kept apart from registry.ts, which takes its entries from `libraryEntriesToLoad`.
 */
import { createSignal } from 'solid-js';
import type { IndicatorRegistryEntry } from 'lightweight-charts-indicators';

let loadingMeta: Promise<IndicatorRegistryEntry[]> | null = null;
let loadingCode: Promise<void> | null = null;
/** The entries the metadata gave (the ones the code goes onto). */
const metaEntries = new WeakSet<IndicatorRegistryEntry>();
const [codeLoaded, setCodeLoaded] = createSignal(false);
/** The library's code is on its entries (they calculate on this thread). */
export const indicatorCodeLoaded = codeLoaded;

/** The library's entries without their code (once). */
function libraryMeta(): Promise<IndicatorRegistryEntry[]> {
  loadingMeta ??= import('virtual:indicator-meta').then((m) => {
    const entries = m.default.entries as IndicatorRegistryEntry[];
    for (const e of entries) metaEntries.add(e);
    return entries;
  });
  return loadingMeta;
}

/** The library's entries for the registry: their metadata, with their code in a window without workers (it
 *  calculates them itself). */
export function libraryEntriesToLoad(): Promise<IndicatorRegistryEntry[]> {
  return typeof Worker === 'undefined' ? loadIndicatorCode().then(libraryMeta) : libraryMeta();
}

/** Import the library's code (once) onto its entries, so they calculate on this thread. */
export function loadIndicatorCode(): Promise<void> {
  loadingCode ??= libraryMeta().then(async (entries) => {
    const byId = new Map(entries.map((e) => [e.id, e]));
    const m = await import('lightweight-charts-indicators');
    for (const e of m.indicatorRegistry) {
      const own = byId.get(e.id) as unknown as Record<string, unknown> | undefined;
      if (!own) continue;
      for (const [k, v] of Object.entries(e)) if (typeof v === 'function') own[k] = v;
    }
    setCodeLoaded(true);
  });
  return loadingCode;
}

/** Whether `entry` can calculate on this thread now; a library entry without its code starts loading it (false). */
export function canCalculateHere(entry: IndicatorRegistryEntry): boolean {
  if (typeof entry.calculate === 'function') return true;
  if (metaEntries.has(entry)) void loadIndicatorCode();
  return false;
}
