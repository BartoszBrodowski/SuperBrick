# S1 findings: ACP + MCP shim against `opencode acp` (opencode 2.2.5)

Run with `cargo run --release -- run --model openai/gpt-6-luna [--mcp-via-config] [--only 1,2,3,4]`.
Logs go to `out/<model>/acp.jsonl` and the report to `report.json`.

| Probe | GPT-6 Luna, MCP via `session/new` | GPT-6 Luna, MCP via config | claude-agent/haiku |
|---|---|---|---|
| 1 MCP tools (`add` then `summary`) | PASS 6.9 s | PASS 5.1 s | provider error: "Not logged in · Run `claude auth login`" |
| 2 image in the prompt | PASS 1.1 s ("blue circle") | PASS 1.4 s | same |
| 3 image in a tool result (`look`) | PASS 2.2 s ("green triangle") | PASS 2.8 s | same |
| 4 sandbox (asked to write a file) | PASS ("CANNOT", nothing written) | PASS | not run |

- **Handshake:**
  - `initialize` takes 0.2–0.7 s.
  - The agent advertises `promptCapabilities.image` and `embeddedContext`, and `mcpCapabilities.http` (no `sse`).
  - `session/new` takes 0.4–1.9 s and returns no model list, so the app must pick the model through config.
- **Both MCP routes work.** `session/new { mcpServers: [stdio shim] }` is the one to use, because it needs no config file per session. The shim is `cad mcp-stdio --socket <owner-only unix socket>` and proxies to the running app.
- **Tool calls arrive through opencode's code-mode `execute` tool:** `{"code": "await tools.cad.add({...})"}`. ACP shows each as an `execute [other]` tool call. So the AI panel should label tool cards from `rawInput.code` and from our own MCP log, not from the ACP title.
- **Session updates seen:**
  - `agent_message_chunk`, `tool_call`, `tool_call_update`, `available_commands_update`, `config_option_update`, `usage_update`.
  - There were no `plan` or `agent_thought_chunk` updates for this model.

## Sandbox hole found, and the fix
- `OPENCODE_CONFIG` is **merged on top of** `~/.config/opencode/opencode.jsonc`, so the user's global MCP servers and plugins stay live.
- The first probe-4 run called `tools.composio.COMPOSIO_REMOTE_BASH_TOOL("printf hi > …/pwned.txt")` through `execute`, and that call completed on Composio's remote sandbox. It also used opencode's `grep`.
- Fix, verified on both routes:
  - spawn `opencode acp` with `XDG_CONFIG_HOME=<empty dir>`;
  - unset `OPENCODE_CONFIG`;
  - pass the whole config in memory with `OPENCODE_CONFIG_CONTENT`;
  - set `OPENCODE_DISABLE_PROJECT_CONFIG=1`, `OPENCODE_DISABLE_CLAUDE_CODE=1` and `OPENCODE_DISABLE_AUTOUPDATE=1`.
- The config copies only `provider`/`providers` from the user's config. They can hold API keys, so they are never written to disk.
- It denies and disables edit, write, patch, bash, webfetch, read, grep, glob, list, todo and task.
- After the fix, `execute` could reach only `tools.cad.*`, `tools.opencode.list_mcp_resources` and a JS `fetch`. A `file://` fetch failed.
- **To check in M7:** whether `fetch` inside `execute` can reach http hosts, since that would be an exfiltration path. If it can, disable `execute` and test whether MCP tools are still offered as plain tools.

## Still open
- claude-agent/haiku: the `claude` CLI is not logged in from this shell (`claude auth status` → `loggedIn: false`). The user has to run `claude auth login`; then re-run with `--model claude-agent/haiku`.
