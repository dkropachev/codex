# User-Facing Features Report

Scope: this is a product-level inventory of the user-facing Codex surfaces documented in this repository's README, docs, TUI guide, CLI entrypoints, and workflow parser. Hidden or internal-only commands are listed separately at the end so the main inventory stays focused on what users actually touch.

## Entry Points

- CLI install and launch: `npm install -g @openai/codex`, `brew install --cask codex`, or a release binary from GitHub.
- IDE integration: the README points users to the Codex IDE install path for VS Code, Cursor, and Windsurf.
- Desktop app: `codex app` opens or installs the Codex Desktop app.
- Web entry point: the README points to Codex Web as a separate cloud-based surface.
- Default interactive experience: `codex` without a subcommand opens the interactive TUI.

## Interactive TUI

The TUI is the main user-facing shell experience. The composer handles text input, attachments, large paste placeholders, slash-command routing, workflow aliases, and submit preparation. It also keeps burst/paste handling safe for IME input.

Slash-command groups in the TUI:

- Session and history: `/new`, `/resume`, `/fork`, `/clear`, `/compact`, `/rename`.
- Task and mode control: `/review`, `/plan`, `/goal`, `/codex`, `/workflow`, `/side`, `/agent`, `/subagents`, `/collab`, `/model`, `/fast`, `/ide`, `/experimental`, `/personality`, `/realtime`, `/settings`.
- Workspace and output helpers: `/init`, `/copy`, `/raw`, `/diff`, `/mention`, `/status`, `/debug-config`, `/title`, `/statusline`, `/theme`, `/vim`, `/keymap`.
- Tools and integrations: `/skills`, `/hooks`, `/memories`, `/mcp`, `/apps`, `/plugins`, `/logout`, `/feedback`, `/ps`, `/stop`.
- Safety and permissions: `/permissions`, `/setup-default-sandbox`, `/sandbox-add-read-dir`, `/approve`.
- Review and approval helpers: `/approve` is the user-visible alias behind the approval retry flow for a recent auto-review denial.

Notable TUI subfeatures:

- `/status` shows active model, reasoning, context usage, rate limits, and instruction sources.
- `/debug-config` shows merged config, requirement sources, and config provenance.
- `/codex` switches the current thread into Codex config mode, and `/codex <request>` submits a request in that mode.
- `/workflow` supports workflow mode, workflow commands, and workflow aliases registered from `workflow.yaml.command`.
- Workflow aliases can show option hints from `usage.options` or `api.inputSchema`, can surface live suggestions from `complete(ctx, input)`, and accept the inline preview with `Tab`.
- `/raw` toggles raw scrollback mode for copy-friendly terminal selection.
- `/diff` shows the git diff, including untracked files.
- `/goal` sets or views the goal for a long-running task.
- `/side` opens an ephemeral side conversation.
- `/agent` and `/subagents` switch the active agent thread.
- `/model` chooses model and reasoning effort, and `/fast` toggles Fast mode.
- `/permissions` controls what Codex is allowed to do.
- `/setup-default-sandbox` and `/sandbox-add-read-dir` manage sandbox access.
- `/realtime` and `/settings` control the experimental voice mode and its devices.
- `/title`, `/statusline`, `/theme`, `/keymap`, and `/vim` are presentation and keyboard preferences.

## Non-Interactive CLI

The CLI exposes the main automation and support workflows outside the TUI:

- `codex exec`: run Codex non-interactively.
- `codex review`: run a code review non-interactively.
- `codex apply`: apply the latest diff produced by Codex as a `git apply`.
- `codex resume`: resume a previous interactive session.
- `codex fork`: fork a previous interactive session.
- `codex login`: authenticate with ChatGPT, API key, or Agent Identity.
- `codex logout`: remove stored credentials.
- `codex account`: inspect and refresh named accounts and account pools.
- `codex completion`: generate shell completion scripts.
- `codex update`: self-update the installation.
- `codex cloud`: browse Codex Cloud tasks and apply changes locally.

Session management subfeatures:

- `codex resume` supports `--last`, `--all`, and `--include-non-interactive`.
- `codex fork` supports `--last` and `--all`.
- Both commands accept remote app-server connection overrides.

## Authentication And Accounts

- `codex login --with-api-key` reads an API key from stdin.
- `codex login --with-agent-identity` reads the experimental Agent Identity token from stdin.
- `codex login --device-auth` uses the device-code flow.
- `codex login --account <ID>` stores ChatGPT credentials under a named account.
- `codex login status` shows login status.
- `codex logout --account <ID>` removes one account's credentials.
- `codex logout --all` removes default and named-account credentials.
- `codex account list` shows default, named, and logical pool accounts.
- `codex account limits` shows ChatGPT Codex usage limits.
- `codex account refresh` refreshes ChatGPT tokens and usage snapshots.

## Sandbox, Exec, And Policy

- `codex sandbox macos` / `seatbelt`: run a command under Seatbelt on macOS.
- `codex sandbox linux` / `landlock`: run a command under the Linux sandbox, with bubblewrap by default.
- `codex sandbox windows`: run a command under the Windows restricted token sandbox.
- `codex execpolicy check`: check execpolicy files against a command.
- The docs also describe the broader sandbox and approval model used by Codex runs.

## Workflows

Workflow support is a major user-facing surface. The TUI and CLI both use the same workflow command parser, and workflows can also be invoked through registered aliases from `workflow.yaml.command`.

Workflow commands:

- Discovery and inspection: `list`, `show <id>`, `where <id>`, `status [<id>]`, `impact <id>`.
- Authoring and editing: `develop <description>`, `describe <id> <description>`, `docs <id> <instruction>`, `edit <id> <instruction>`, `fix <id>`.
- Execution: `run <id>`, `mode`, and registered workflow aliases.
- Validation and lifecycle: `validate <id>`, `config show`, `config set <key> <value>`, `config clear <key>`, `publish`, `discard`, `done`.

Workflow subfeatures:

- `run` accepts `--input '{...}'`, `--input @file.json`, and top-level `--key value` fields.
- The composer can surface workflow aliases by exact alias, id, title, or search terms.
- Exact alias matches can show inline option hints and live completion suggestions.
- Workflow execution can be staged in session-specific `.workflow-staging/sessions/<session-id>` trees.
- `/workflow done` publishes staged changes back to the live workflow.
- `/workflow validate <id>` checks docs, layout, coverage markers, local packages, loadability, autocomplete readiness, and validation commands/tests.
- `/workflow repair <id>` reports applied fixes, blocked findings, unsupported findings, validation results, and stop reasons.

## Plugins

- `/plugins` is the TUI entry point for browsing, installing, enabling, and inspecting plugins.
- Plugin marketplace management is exposed in the CLI as `codex plugin marketplace add|upgrade|remove`.
- Plugins can contribute skills, MCP servers, apps, and UI metadata.
- Marketplace-backed plugins can be installed from GitHub, HTTP(S) Git, SSH, or local paths.
- Installed plugin state lives under Codex home plugin storage.

## Skills

- `/skills` exposes the local and plugin-provided skill system.
- Skills are local `SKILL.md` instruction bundles.
- Skills can be bundled, locally configured, or contributed by plugins.
- `$skill` mentions and natural-language trigger rules can activate skills.
- Skill metadata contributes to the model-visible instruction budget.

## MCP And Apps

- `/mcp` is the TUI surface for configured MCP tools; `/mcp verbose` expands detail.
- `codex mcp list|get|add|remove|login|logout` manages MCP servers and their OAuth state.
- `codex api` returns a machine-readable catalog of app-server methods, MCP tools, built-in tools, workflow runtime APIs, and discovered workflows.
- `/apps` exposes connector-backed apps and their installation state.
- Apps appear in the composer with `$` mention support.
- MCP configuration includes stdio and HTTP servers, tool approvals, and process reuse scope.

## Memories

- `/memories` controls memory use and generation.
- Memory config supports use on/off, generation on/off, external-context protection, rollout age limits, idle-thread limits, and extraction/consolidation model overrides.
- The guide treats memories as contextual instructions that should stay high-signal and sparse.

## Implement

- `/implement enable|disable|implicit --max-cycles=N` controls targeted review/fix cycles after agent edits.
- `codex implement enable|disable|implicit --max-cycles=N` persists the same settings in config.
- `mode = auto` runs the loop after normal edits, while `mode = implicit` limits it to explicit `/implement` turns.
- `max_cycles` bounds how many review/fix passes run before findings are surfaced.

## Model Router And Tool Router

- `/model-router` temporarily overrides routing for the current thread.
- `codex model-router policy` inspects effective candidates, hard rules, score bias, and lifecycle gates.
- `codex model-router tune` replays historical turns to tune router metrics.
- `codex model-router report show|apply` manages stored tuning reports.
- `codex model-router lifecycle`, `shadows`, `usage`, `promote`, and `demote` expose lifecycle and accounting controls.
- `codex tool-router tune` analyzes tool-router telemetry and can persist dynamic guidance.
- `tool_router` is the structured internal tool dispatch surface behind many of the model-visible tool calls.

## Configuration And Debug Surfaces

- `/status` and `/debug-config` are the main TUI debug surfaces.
- `codex features list|enable|disable` manages feature flags.
- `codex debug models` renders the model catalog, optionally using the bundled catalog only.
- `codex debug prompt-input` renders the model-visible prompt input payload.
- `codex debug app-server send-message-v2` sends a message directly to the app server.
- `codex app-server` runs the app server or related tooling; `--listen` controls the transport endpoint and `--analytics-default-enabled` changes the default analytics stance.
- `codex app-server proxy` proxies stdio bytes to the app-server control socket.
- `codex app-server generate-ts` and `codex app-server generate-json-schema` generate protocol artifacts for integrations.
- `codex api --mcp-detail full|toolsAndAuthOnly` controls the detail level of MCP inventory in the catalog.

Config-facing feature areas documented in `docs/config.md`:

- Connecting to MCP servers and per-tool approvals.
- Apps connectors and app installation state.
- Notify hooks.
- Implement review/fix defaults.
- JSON schema generation for `config.toml`.
- SQLite state DB location and usage.
- ChatGPT account pools.
- Built-in DeepSeek provider.
- Model router discovery, lifecycle, and candidate policy.
- Custom CA certificates.
- Notices and do-not-show-again prompts.
- Plan mode default reasoning effort.
- Realtime start instructions.

## Hidden Or Internal Only

These are surfaced in code or docs but are not part of the normal user-facing inventory:

- `workflow-quality-hook`.
- `responses-api-proxy`, `stdio-to-uds`, `mcp-broker`, `exec-server`.
- `execpolicy` as a hidden top-level command.
- `trace-reduce` and `clear-memories` under `debug`.
- `generate-internal-json-schema` under `app-server`.
- Slash commands that are debug-only or hidden in normal builds: `rollout`, `test-approval`, `debug-m-drop`, `debug-m-update`.

## Primary Sources Consulted

- `README.md`
- `docs/getting-started.md`
- `docs/authentication.md`
- `docs/exec.md`
- `docs/sandbox.md`
- `docs/execpolicy.md`
- `docs/config.md`
- `docs/tui-chat-composer.md`
- `docs/skills.md`
- `docs/slash_commands.md`
- `codex-rs/tui/codex_guide.md`
- `codex-rs/tui/src/slash_command.rs`
- `codex-rs/tui/src/chatwidget/slash_dispatch.rs`
- `codex-rs/cli/src/main.rs`
- `codex-rs/cli/src/mcp_cmd.rs`
- `codex-rs/cli/src/workflow_cmd.rs`
- `codex-rs/workflows/src/command.rs`
