/** Names of the indicator library's entries (scripts/indicator-manifest-plugin.mjs): [id, name, short name, group
 *  ("s" standard, "c" community, "k" candlestick)]. */
declare module "virtual:indicator-manifest" {
  const manifest: { version: string; entries: [string, string, string, "s" | "c" | "k"][] };
  export default manifest;
}

/** The indicator library's entries without their code (scripts/indicator-manifest-plugin.mjs): every field that is not
 *  a function. */
declare module "virtual:indicator-meta" {
  import type { IndicatorRegistryEntry } from "lightweight-charts-indicators";
  const meta: { version: string; entries: Omit<IndicatorRegistryEntry, "calculate" | "createRun" | "requiredSymbols">[] };
  export default meta;
}
