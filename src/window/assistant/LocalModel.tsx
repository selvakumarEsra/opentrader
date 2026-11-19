/*
 * The built-in model's download screen, shown only while it is the assistant's model (specs/assistant-cli-models.md,
 * REQ-ASSIST-CLI-001): one sentence, a beta warning and Activate, which downloads it (src/data/assistant/local.ts),
 * and the ready SDK models to use instead.
 * Nothing comes with the installer; Remove is in the panel's menu once it is there.
 */
import { For, Show, onMount } from "solid-js";
import { translateWith } from "../../i18n/translator";
import { chooseModel, cliModels, refreshModels } from "../../data/assistant/models";
import { cancelLocal, gb, installLocal, localBlocker, localError, localInfo, localProgress, refreshLocal } from "../../data/assistant/local";

export function AssistantSetup() {
  onMount(() => {
    void refreshLocal();
    void refreshModels();
  });
  const busy = () => !!localInfo()?.installing || !!localProgress();
  const pct = () => {
    const p = localProgress();
    return p && p.total ? Math.round(((p.done ?? 0) / p.total) * 100) : 0;
  };
  return (
    <div class="asst-column asst-models">
      <div class="asst-empty asst-setup">
        <p class="asst-greeting">Your assistant works the app for you: charts, indicators, screens and alerts.</p>
        <Show when={localInfo()}>
          {(i) => (
            <Show
              when={!busy()}
              fallback={
                <div class="asst-progress-block">
                  <div class="asst-progress"><div style={{ width: `${pct()}%` }} /></div>
                  <div class="asst-inline">
                    <span class="asst-muted">{translateWith("Activating… {0}%", pct())}</span>
                    <button type="button" class="asst-btn" data-i18n-sense="action" onClick={cancelLocal}>Stop</button>
                  </div>
                </div>
              }
            >
              <Show when={localBlocker(i())} fallback={
                <>
                  <p class="asst-beta">
                    <span class="asst-beta-tag">Beta</span>
                    It can make mistakes and some steps may fail.
                  </p>
                  <button type="button" class="asst-btn is-primary" onClick={() => void installLocal()}>
                    {translateWith("Activate ({0})", gb(i().downloadBytes))}
                  </button>
                </>
              }>
                {(why) => <span class="asst-muted">{why()}</span>}
              </Show>
            </Show>
          )}
        </Show>
        <Show when={localError()}>{(e) => <p class="asst-status is-error">{e()}</p>}</Show>
        <Show when={cliModels().some((m) => m.ready)}>
          <div class="asst-setup-or">
            <span class="asst-muted">Or answer with your own account, nothing to download:</span>
            <For each={cliModels().filter((m) => m.ready)}>
              {(m) => (
                <button type="button" class="asst-btn" onClick={() => chooseModel(m.id)}>
                  {translateWith("Use {0}", m.name)}
                </button>
              )}
            </For>
          </div>
        </Show>
      </div>
    </div>
  );
}
