/*
 * The AI assistant's SDK models call the app's tools over MCP (specs/assistant-cli-models.md): a destructive one waits
 * for the user's Allow in the panel. The assistant's session sets the question; the bridge asks it.
 */
import type { HandleOptions } from "./mcp";

type Confirm = NonNullable<HandleOptions["confirm"]>;

let confirm: Confirm = async () => false;

export function setAssistantConfirm(c: Confirm): void {
  confirm = c;
}

export const assistantConfirm: Confirm = (name, args) => confirm(name, args);
