/*
 * Settings > Coding agents > AI ASSISTANT MODELS (specs/assistant-cli-models.md): the token and the model name of each
 * SDK model the assistant panel can choose (Claude Code, Codex, Gemini). A typed token is kept on this computer
 * (readable by the user only) and never shown again; an empty field uses the environment's or the login's.
 */
import { createSignal, For, onMount, Show } from "solid-js";
import { commands, type CliModel } from "../../../bindings";
import { cliStatus, refreshModels, setModelsStatus } from "../../../data/assistant/models";
import { Section } from "./parts";
import { translateWith } from "../../../i18n/translator";

/** Where each model's token comes from, and the model names one can ask. */
const HINTS: Record<string, { token: string; models: string }> = {
  claude: { token: "CLAUDE_CODE_OAUTH_TOKEN (claude setup-token) or an Anthropic API key", models: "claude-opus-5-5 (default), claude-sonnet-5-5" },
  codex: { token: "CODEX_API_KEY (an OpenAI API key)", models: "Codex's default, or a model name" },
  gemini: { token: "GEMINI_API_KEY (Google AI Studio)", models: "gemini-pro-latest (default), gemini-flash-latest" },
};

function ModelRow(props: { m: CliModel }) {
  const [token, setToken] = createSignal("");
  const [model, setModel] = createSignal(props.m.model);
  const [error, setError] = createSignal<string | null>(null);
  const save = (t: string | null, name: string | null) =>
    void commands.assistantCliConfigure(props.m.id, t, name).then((r) => {
      if (r.status === "ok") {
        setModelsStatus(r.data);
        setError(null);
        if (t !== null) setToken("");
      } else setError(r.error);
    });
  const hint = () => HINTS[props.m.id];
  return (
    <div class="app-settings-agents-setup">
      <div class="app-settings-agents-setup-head">
        <span class="app-settings-field-label" data-no-translate>{props.m.name}</span>
        <span class="app-settings-note">
          {props.m.ready ? translateWith("Ready, with {0}", props.m.source ?? "") : translateWith("Not ready: needs {0}", props.m.need ?? "")}
        </span>
      </div>
      <label class="app-settings-field">
        <span class="app-settings-field-label">Token</span>
        <input
          class="app-settings-input"
          type="password"
          autocomplete="off"
          placeholder={props.m.tokenSet ? "•••••••• (set: type to replace)" : hint()?.token}
          value={token()}
          onInput={(e) => setToken(e.currentTarget.value)}
          onBlur={() => token().trim() && save(token(), null)}
          onKeyDown={(e) => { if (e.key === "Enter" && token().trim()) save(token(), null); }}
        />
      </label>
      <Show when={props.m.tokenSet}>
        <div class="app-settings-agents-row">
          <button type="button" class="app-settings-btn" onClick={() => save("", null)}>Forget the token</button>
        </div>
      </Show>
      <label class="app-settings-field">
        <span class="app-settings-field-label">Model</span>
        <input
          class="app-settings-input"
          type="text"
          placeholder={hint()?.models}
          value={model()}
          onInput={(e) => setModel(e.currentTarget.value)}
          onBlur={() => model() !== props.m.model && save(null, model())}
          onKeyDown={(e) => { if (e.key === "Enter") save(null, model()); }}
        />
      </label>
      <Show when={error()}>{(e) => <span class="app-settings-field-error">{e()}</span>}</Show>
    </div>
  );
}

export function AssistantModels() {
  onMount(() => void refreshModels());
  return (
    <Section label="AI ASSISTANT MODELS">
      <div class="app-settings-note">
        The AI assistant can answer with Claude Code, Codex or Gemini instead of its built-in model, run on this computer through each one's SDK
        with your own account (Node.js 18 or newer needed; the first question sets the SDKs up). The model reads your charts and data with the
        tools above, only during your question, and cannot place orders. Your questions go to that model's provider, not to OpenTrader.
      </div>
      <Show when={cliStatus()} fallback={<div class="app-settings-note">Looking for Node.js and your credentials…</div>}>
        {(s) => (
          <>
            <Show when={!s().node}>
              <span class="app-settings-field-error">Node.js was not found: install it from nodejs.org, then reopen Settings.</span>
            </Show>
            <For each={s().models}>{(m) => <ModelRow m={m} />}</For>
          </>
        )}
      </Show>
    </Section>
  );
}
