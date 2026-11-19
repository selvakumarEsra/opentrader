---
id: ASSIST-CLI-DOC
title: AI assistant on Claude Code, Codex or Gemini (their SDKs)
owner: selvakumarEsra
priority: high
---

<!-- id: ASSIST-CLI-DOC -->
# AI assistant on Claude Code, Codex or Gemini

The AI assistant panel answers with the built-in local model (src-tauri/src/local_model.rs). This adds a choice of
model, each run on this computer through its own SDK with the user's own credentials: Claude Code (the Claude Agent
SDK, with `CLAUDE_CODE_OAUTH_TOKEN` — also read as `CLAUDE_CODE_TOKEN` — or an `ANTHROPIC_API_KEY`, or the Claude
Code login on this computer), Codex (the Codex SDK, with `CODEX_API_KEY` / `OPENAI_API_KEY` or the Codex login) and
Gemini (the Google Gen AI SDK, with `GEMINI_API_KEY` / `GOOGLE_API_KEY`). The SDKs run in a small Node.js program
(src-tauri/assistant-bridge/) that the app sets up in its data folder the first time one is used. With it the assistant answers
sophisticated stock and options questions and gives trading ideas, reading the app's own data (bars, the chart,
fundamentals, filings, screens, the journal) through the app's tools, the same ones Settings > Coding agents serves
over MCP (src/agent/). OpenTrader keeps the tokens typed in Settings > Coding agents in its config folder on this computer (readable by the
user only); a token set in the environment is used when none is typed. Each SDK sends the conversation only to its
own provider.

A model can read and change charts, indicators, drawings, alerts and watchlists; it cannot trade: no app tool
places, changes or cancels an order.

<!-- id: REQ-ASSIST-CLI-001 -->
## The user MUST choose the assistant's model, an SDK model by default, the built-in one only when chosen

The assistant answers with Claude Code, Codex or Gemini by default: the first of them that is ready, in that order,
until the user chooses one. The built-in model (a 2.3 GB download) is used only when the user chooses it, or, when
they turn on "Answer with the built-in model when the chosen one is not ready", in place of a chosen SDK model that is
not ready. The choice is made in Settings > Coding agents > AI assistant models and in the panel's model menu; a
model without credentials (or without Node.js 18+ on this computer) is listed disabled, with what it needs. The choice
is kept (src/data/kv.ts) and used for the next questions of every chat. The panel never asks to download the built-in
model unless it is the model in use; that screen offers the ready SDK models instead.

implementations:
  - src/data/assistant/models.ts:activeModel
  - src/data/assistant/models.ts:chosenModel
  - src/data/assistant/models.ts:notReady
  - src/window/assistant/AssistantPanel.tsx:ChatView
  - src/window/assistant/LocalModel.tsx:AssistantSetup
  - src/window/header/app-settings/AssistantModels.tsx:AssistantModels
  - src-tauri/src/assistant_cli.rs:assistant_cli_status

verifies:
  - src/data/assistant/__tests__/models.test.ts

## Acceptance
<!-- id: REQ-ASSIST-CLI-001.A1 -->
- With a Claude Code login and nothing chosen, the panel opens on the chat (no download screen) and the first question goes to Claude Code; with only a Codex login, to Codex.
<!-- id: REQ-ASSIST-CLI-001.A2 -->
- With Codex chosen in Settings, the panel's menu shows Codex checked, and the next question goes to Codex; the choice is kept after a restart.
<!-- id: REQ-ASSIST-CLI-001.A3 -->
- A chosen SDK model that is not ready leaves the composer disabled with a note naming what it needs and where to set it, unless the fallback is on: then the built-in model answers (if installed), saying so.
<!-- id: REQ-ASSIST-CLI-001.A4 -->
- The download screen shows only with Built-in chosen (or as the fallback); it lists the ready SDK models to use instead.
<!-- id: REQ-ASSIST-CLI-001.A5 -->
- Changing the model mid-chat keeps the chat: the next question goes to the new model with the chat so far.

<!-- id: REQ-ASSIST-CLI-002 -->
## A question to an SDK model MUST run on this computer through that model's SDK and stream its answer into the panel

The app runs the chosen model's SDK with the chat so far, the question, and the active chart's context (its symbol,
interval and panes, the date as dd/mm/yyyy), and shows its answer as it comes. Stop ends the run. The first question
to an SDK model sets the SDKs up (an `npm` install in the app's data folder), saying so in the panel.

implementations:
  - src-tauri/src/assistant_cli.rs:assistant_cli_chat
  - src-tauri/assistant-bridge/bridge.mjs
  - src/data/assistant/session.ts:sendSdk
  - src/data/assistant/session.ts:sdkPrompt

verifies:
  - src/data/assistant/__tests__/session-sdk.test.ts
  - src-tauri/assistant-bridge/bridge.test.mjs

## Acceptance
<!-- id: REQ-ASSIST-CLI-002.A1 -->
- Asking "what is the trend of this chart?" with Claude Code chosen on `NSE:RELIANCE` 1D runs the Claude Agent SDK; the prompt it receives names `NSE:RELIANCE`, `1D` and today's date; the answer text appears in the panel as it streams.
<!-- id: REQ-ASSIST-CLI-002.A2 -->
- Stop while a model answers ends its run (the SDK program and what it started) within 2 s; the answer so far stays, marked stopped.
<!-- id: REQ-ASSIST-CLI-002.A3 -->
- A refused token, or a run that fails, shows the error in the answer (the SDK's message); the built-in model is not tried instead.
<!-- id: REQ-ASSIST-CLI-002.A4 -->
- No SDK process is left running after its answer ends or the app quits.

<!-- id: REQ-ASSIST-CLI-003 -->
## An SDK model MUST reach the app's data and chart actions through the app's tools, and MUST NOT run a destructive one without the user's confirmation

During a question the model is given the app's MCP tools (src/agent/) for that run only, whether or not Settings >
Coding agents is turned on, and only from this computer. Each tool it calls shows in the answer as a step, as the
built-in model's do. A tool marked destructive (deleting an alert, a note, a layout, a watchlist, a series…) waits
for the user's Allow in the panel; Deny answers the model that the user did not allow it. Beside the app's tools a
model may search and read the web; it gets no shell and no access to files. Nothing of the user's own set-up for
that model's program comes with it (their MCP servers, plugins, skills or hooks): only their login.

implementations:
  - src-tauri/assistant-bridge/bridge.mjs:codexHome
  - src-tauri/assistant-bridge/bridge.mjs:codexItemAllowed
  - src/agent/mcp.ts:handleMessage
  - src/agent/assistant-confirm.ts
  - src-tauri/src/agent.rs:begin_run
  - src-tauri/src/agent.rs:end_run
  - src/data/assistant/session.ts

verifies:
  - src/agent/__tests__/mcp.test.ts
  - src/data/assistant/__tests__/session-sdk.test.ts
  - src-tauri/assistant-bridge/bridge.test.mjs

## Acceptance
<!-- id: REQ-ASSIST-CLI-003.A1 -->
- A question that needs bars ("RSI of NSE:INFY on 1D?") shows a `get_bars` (or `get_chart_data`) step, and the answer uses its values.
<!-- id: REQ-ASSIST-CLI-003.A2 -->
- With Settings > Coding agents off, the model still reaches the tools during a question, and the MCP endpoint answers nothing once the question has ended.
<!-- id: REQ-ASSIST-CLI-003.A3 -->
- A `delete_alert` call from the model waits for Allow; on Deny the alert stays and the model is told "The user did not allow it."
<!-- id: REQ-ASSIST-CLI-003.A4 -->
- The tools offered to the model include none that places, modifies or cancels an order.
<!-- id: REQ-ASSIST-CLI-003.A5 -->
- Asked to run `ls ~`, Codex cannot: it has no shell, no code runner and none of the MCP servers of the user's `~/.codex/config.toml`; a call to any tool but the app's stops the answer with an error.
<!-- id: REQ-ASSIST-CLI-003.A6 -->
- Two windows asking at once each get their own answer, and Stop in one leaves the other's running.

<!-- id: REQ-ASSIST-CLI-004 -->
## Answers on stocks and options MUST rest on the app's data and say what they rest on

The instructions given to an SDK model ask it to read the symbol's data through the app's tools before answering
about it; to name the data's last bar date; to show its working for options figures (strike, expiry, days to
expiry, implied volatility used, Greeks); and to give each trading idea with its entry, invalidation (stop) and
risks. The panel's note that AI can make mistakes and nothing is financial advice shows with every model.

implementations:
  - src/data/assistant/prompts.ts:SDK_INSTRUCTIONS
  - src/window/assistant/AssistantPanel.tsx

## Acceptance
<!-- id: REQ-ASSIST-CLI-004.A1 -->
- The instructions sent to the model contain the rules above (data first, last bar date, options working, entry / stop / risks).
<!-- id: REQ-ASSIST-CLI-004.A2 -->
- The note "AI can make mistakes. Nothing here is financial advice." shows under the composer whichever model is chosen.

<!-- id: REQ-ASSIST-CLI-005 -->
## Conversations with an SDK model MUST NOT be sent to the app's usage statistics

The built-in model's conversations go to the usage statistics when they are on (src/data/assistant/telemetry.ts).
An SDK model's never do: questions, answers, tool steps and ratings stay on this computer (and with that model's own
provider). Chats are kept as the built-in model's are.

implementations:
  - src/data/assistant/session.ts:sendSdk

verifies:
  - src/data/assistant/__tests__/session-sdk.test.ts

## Acceptance
<!-- id: REQ-ASSIST-CLI-005.A1 -->
- With usage statistics on, a question answered by Claude Code sends no `$ai_generation`, `$ai_span`, `$ai_trace` or `assistant_feedback` event.
<!-- id: REQ-ASSIST-CLI-005.A2 -->
- A chat answered by an SDK model is kept and reopened like a built-in one.
