// Vite plugin: `virtual:indicator-manifest`, the names of the indicator library's entries (id, name, short name,
// group) read from the installed library at build time. The app lists the indicators from it and loads the library
// itself on first use (src/window/chart/indicators/registry.ts), which keeps the library out of the first download.
//
// `virtual:indicator-meta`: the entries themselves without their code (inputs, plots, styles... every field that is
// not a function). The window shows and edits the studies from it; the study worker calculates them with the library
// (specs/performance.md REQ-PERF-001), so the window does not hold the library's ~1,900 studies a second time.
//
// The names are read by a node process of their own: importing the library here kept its ~1,900 studies in the build's
// memory until the end of the build, which then ran out of memory at the release workflow's heap size (#258).
import { execFile } from "node:child_process";
import { promisify } from "node:util";

const ID = "virtual:indicator-manifest";
const RESOLVED = "\0" + ID;
const META_ID = "virtual:indicator-meta";
const META_RESOLVED = "\0" + META_ID;

const READ_NAMES = `
const GROUP = { standard: "s", community: "c", candlestick: "k" };
const { indicatorRegistry, version } = await import("lightweight-charts-indicators");
const entries = indicatorRegistry.map((e) => [e.id, e.name, e.shortName, GROUP[e.group]]);
process.stdout.write(JSON.stringify({ version, entries }));
`;

// Every field but the functions (calculate, createRun, requiredSymbols); the entries hold no function deeper down.
const READ_META = `
const { indicatorRegistry, version } = await import("lightweight-charts-indicators");
const entries = indicatorRegistry.map((e) => Object.fromEntries(Object.entries(e).filter(([, v]) => typeof v !== "function")));
process.stdout.write(JSON.stringify({ version, entries }));
`;

async function read(script) {
  const { stdout } = await promisify(execFile)(process.execPath, ["--input-type=module", "-e", script], {
    cwd: process.cwd(),
    maxBuffer: 64 * 1024 * 1024,
  });
  return stdout;
}

export function indicatorManifest() {
  return {
    name: "indicator-manifest",
    resolveId(id) {
      return id === ID ? RESOLVED : id === META_ID ? META_RESOLVED : null;
    },
    async load(id) {
      if (id === RESOLVED) return `export default ${await read(READ_NAMES)};`;
      // A JSON string parses much faster than the same object written as a JavaScript literal.
      if (id === META_RESOLVED) return `export default JSON.parse(${JSON.stringify(await read(READ_META))});`;
      return null;
    },
  };
}
