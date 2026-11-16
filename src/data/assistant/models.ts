/*
 * The AI assistant's model (specs/assistant-cli-models.md): the built-in one (local_model.rs) or an SDK model —
 * Claude Code, Codex, Gemini — run on this computer with the user's credentials (assistant_cli.rs). The choice is kept;
 * one that is no longer ready falls back to the built-in model.
 */
import { createSignal } from "solid-js";
import { commands, type CliModel, type CliStatus } from "../../bindings";
import * as kv from "../kv";

export const MODEL_KEY = "ot:assistant:model";
export const BUILTIN = "builtin";

const [status, setStatus] = createSignal<CliStatus | null>(null);
const [chosen, setChosen] = createSignal<string>(kv.getItem(MODEL_KEY) ?? BUILTIN);
kv.onExternalChange(MODEL_KEY, () => setChosen(kv.getItem(MODEL_KEY) ?? BUILTIN));

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

/** The chosen model, as the user chose it. */
export const chosenModel = chosen;

/** The model the next question goes to: the chosen one, or the built-in one when the chosen one is not ready (once
 *  the status is known). */
export function activeModel(): string {
  const id = chosen();
  if (id === BUILTIN) return BUILTIN;
  const m = cliModel(id);
  if (!status()) return id;
  return m?.ready ? id : BUILTIN;
}

/** The chosen SDK model is not ready: the panel says the built-in one answers instead. */
export function fellBack(): CliModel | undefined {
  const id = chosen();
  if (id === BUILTIN || !status()) return undefined;
  const m = cliModel(id);
  return m && !m.ready ? m : undefined;
}

export function chooseModel(id: string): void {
  setChosen(id);
  kv.setItem(MODEL_KEY, id);
}

/** A model's name as the panel shows it. */
export function modelName(id: string): string {
  return id === BUILTIN ? "Built-in" : (cliModel(id)?.name ?? id);
}
