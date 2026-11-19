/*
 * The AI assistant's model (specs/assistant-cli-models.md, REQ-ASSIST-CLI-001): an SDK model — Claude Code, Codex,
 * Gemini — run on this computer with the user's credentials (assistant_cli.rs), or the built-in one (local_model.rs)
 * when the user chooses it. Until the user chooses, the first ready SDK model answers. A chosen SDK model that is not
 * ready is not replaced by the built-in one unless the user turned that fallback on.
 */
import { createSignal } from "solid-js";
import { commands, type CliModel, type CliStatus } from "../../bindings";
import * as kv from "../kv";

export const MODEL_KEY = "ot:assistant:model";
export const FALLBACK_KEY = "ot:assistant:fallback";
export const BUILTIN = "builtin";
/** The SDK models in the order the default is picked from. */
export const SDK_ORDER = ["claude", "codex", "gemini"];

const [status, setStatus] = createSignal<CliStatus | null>(null);
const [chosen, setChosen] = createSignal<string | null>(kv.getItem(MODEL_KEY));
const [fallback, setFallbackSignal] = createSignal<boolean>(kv.getItem(FALLBACK_KEY) === "1");
kv.onExternalChange(MODEL_KEY, () => setChosen(kv.getItem(MODEL_KEY)));
kv.onExternalChange(FALLBACK_KEY, () => setFallbackSignal(kv.getItem(FALLBACK_KEY) === "1"));

/** The SDK models, with what each lacks (null until asked). */
export const cliStatus = status;

/** Asks which SDK models are ready. */
export async function refreshModels(): Promise<CliStatus | null> {
  try {
    const s = await commands.assistantCliStatus();
    setStatus(s);
    return s;
  } catch {
    return null;
  }
}

/** Keeps the status Settings changed. */
export function setModelsStatus(s: CliStatus): void {
  setStatus(s);
}

export const cliModels = (): CliModel[] => status()?.models ?? [];

/** The model the choice names, if it is an SDK one. */
export const cliModel = (id: string): CliModel | undefined => cliModels().find((m) => m.id === id);

/** The default before the user chooses: the first ready SDK model, else the first one (which then says what it needs). */
export function defaultModel(): string {
  return SDK_ORDER.find((id) => cliModel(id)?.ready) ?? SDK_ORDER[0];
}

/** The model the user chose, or the default. */
export function chosenModel(): string {
  return chosen() ?? defaultModel();
}

/** Whether the user chose a model (rather than taking the default). */
export const hasChosen = (): boolean => chosen() !== null;

/** The chosen SDK model, known not to be ready (status asked). */
function unready(): CliModel | undefined {
  const id = chosenModel();
  if (id === BUILTIN || !status()) return undefined;
  const m = cliModel(id);
  return m && !m.ready ? m : undefined;
}

/** The model the next question goes to: the chosen one; the built-in one in place of a chosen SDK model that is not
 *  ready only when the fallback is on. */
export function activeModel(): string {
  const id = chosenModel();
  if (id === BUILTIN) return BUILTIN;
  return unready() && fallback() ? BUILTIN : id;
}

/** The chosen SDK model is not ready and nothing answers in its place: the panel says what it needs. */
export function notReady(): CliModel | undefined {
  return fallback() ? undefined : unready();
}

/** The chosen SDK model is not ready and the built-in one answers instead. */
export function fellBack(): CliModel | undefined {
  return fallback() ? unready() : undefined;
}

export function chooseModel(id: string): void {
  setChosen(id);
  kv.setItem(MODEL_KEY, id);
}

/** "Answer with the built-in model when the chosen one is not ready" (off unless turned on). */
export const builtinFallback = fallback;

export function setBuiltinFallback(on: boolean): void {
  setFallbackSignal(on);
  kv.setItem(FALLBACK_KEY, on ? "1" : "0");
}

/** A model's name as the panel shows it. */
export function modelName(id: string): string {
  return id === BUILTIN ? "Built-in" : (cliModel(id)?.name ?? id);
}
