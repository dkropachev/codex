# Codex Guide

This guide is a source map for user-facing Codex surfaces. Before changing an entry, verify the current behavior against the listed source files and keep recipes concrete.

## Fast Checks

- Use `/status` to inspect the active model, reasoning, context usage, rate limits, and instruction sources.
- Use `/debug-config` to inspect merged config, requirement sources, and config-layer provenance.
- Use `/mcp verbose`, `/skills`, `/plugins`, `/apps`, `/implement`, and `/model-router` to inspect the matching feature from the TUI.
- Use `codex tool-router tune` and `codex model-router tune` for CLI-only internal routing diagnostics.
- Use `codex api` or app-server `apiCatalog/read` to hand a machine-readable catalog of app-server methods, MCP tools, and workflow SDK APIs to IDEs or coding agents.
- When a guide section changes, update this file and the `/codex` config-mode snapshots in `codex-rs/tui/src/chatwidget/snapshots/`.

## Slash Commands

- Description: slash commands are parsed by the bottom-pane composer and dispatched by `ChatWidget` without involving the model unless the command explicitly submits a user turn or switches collaboration mode. Registered workflow aliases from `workflow.yaml.command` also appear here when workflows are enabled, and the popup can surface workflows by alias, id, title, or search terms so partial workflow names can still land on the right command. When the typed workflow alias is exact, the popup also shows dimmed workflow option hints cached from `usage.options` or `api.inputSchema` in `workflow.yaml`, filters those hints and live workflow suggestions from an optional `complete(ctx, input)` hook by the typed argument prefix, and treats cached option hints as display-only so `Tab` cannot commit placeholders like `<string>`. Live completion requests are debounced, stale requests are cancelled, runtime errors surface in the popup, and exactly one live suggestion collapses to an inline dimmed suffix. `Tab` accepts that preview text before submission while preserving multiline draft tails. `Enter` runs an unambiguous workflow search-term match without requiring an extra selection move. The shared workflow parser handles both `/<cmd>` and `codex <cmd>` while built-in slash commands keep precedence on collisions.
- Configuration: visibility is mostly feature-gated by `BuiltinCommandFlags`; side conversations and task-running state apply additional dispatch checks.
- Tuning: add names in `SlashCommand`, keep enum order intentional for popup ranking, add descriptions, and only mark `supports_inline_args` when arguments are parsed locally.
- Debug recipe: check `SlashCommand::from_str`, `builtins_for_input`, and `find_builtin_command`; then dispatch from a focused chatwidget test and assert history or app events.
- Source entrypoints: `codex-rs/tui/src/slash_command.rs`, `codex-rs/tui/src/bottom_pane/slash_commands.rs`, `codex-rs/tui/src/bottom_pane/chat_composer.rs`, `codex-rs/tui/src/chatwidget/slash_dispatch.rs`.
- Codex mode: bare `/codex` switches the current thread into Codex config mode; `/codex <request>` switches to that mode and submits the request. The turn embeds the Plan-mode guide plus this guide, generated slash-command registry context, and current `codex --help` output as hidden developer instructions; the visible user message remains only the user-authored prompt. Turns run from a scratch workspace under the system temp directory with `/tmp` writable, the target workspace read-only, and the Codex config directory writable. The model may inspect and run scripts, writing outputs only to scratch space, `/tmp`, or the Codex config directory. It can emit one complete `<proposed_plan>` when planning is appropriate, and the TUI opens the normal "Implement this plan?" prompt. Users can leave with `/codex off`, `/codex disable`, or `/codex cancel`.
- Codex config backup: entering Codex mode creates a backup of `${CODEX_HOME}/config.toml` in `${CODEX_HOME}`. When the user leaves Codex mode, Codex removes that backup if `config.toml` is unchanged; if `config.toml` changed, the backup remains for manual rollback.
- Codex config-edit mode: accepting the Codex plan starts the follow-up turn in an internal config-edit submode. That apply turn may modify only the Codex config directory, scratch workspace, and `/tmp`; the target workspace remains read-only. The apply turn should validate the change and reload config or state when possible, or describe any required restart and rollback path. It must not emit a new `<proposed_plan>`.
- Config history: config-edit apply turns currently rely on the model to inspect and validate the resulting `$CODEX_HOME` files; the Codex-mode backup is a plain `config.toml` backup, not a dedicated config-history bundle. Prefer supported config APIs, app-server APIs, routed config tools, or maintained CLI commands before direct file edits, and include manual rollback notes in the final response when editing config.
- Token impact: local commands like `/status` and `/debug-config` add no model context by themselves; bare `/codex` adds Codex config developer instructions to subsequent turns, and commands such as `/codex <instruction>`, `/init`, `/compact`, `/review`, `/plan <prompt>`, and `/repo-ci <task>` can submit model-visible input.

## Workflow Threads

- Description: when `[features].workflows = true`, `/workflow` enters Workflow collaboration mode. `/workflow <command>` starts executable workflows through app-server v2 `workflowRun/start` and handles workflow management commands through the existing app-server workflow command APIs. App-server owns run ids, in-memory run state, cancellation, progress forwarding, markdown handoff, and completion/failure notifications on the active app-server transport. JavaScript execution still uses the Bun-backed workflow host behind app-server; runs are serialized through that host and each workflow import is cache-busted per execution. Workflows are authored as a named default-export async function in `src/workflow.ts`, with optional named `complete(...)` for autocomplete and optional `WorkflowOutput.toTuiMarkdown(result)` for the host markdown formatter. `WorkflowContext.status({ workflowName, workflowStatus, threads? })` renders as `Workflow <workflowName>: <workflowStatus>`, adds `-> <threadName>: <threadStatus>` rows only when more than one thread is present, and treats `WorkflowContext.progress(message, data?)` as a legacy single-status shorthand. `WorkflowContext.reportToUserMarkdown(markdown)` still becomes a `Workflow Result` history cell that is carried forward as hidden follow-up context for the originating thread. `WorkflowContext.artifacts.cache.ensure(...)` is the workflow-facing artifact API: declare a file/glob scope, let Codex hash it, and build into the provided output directory only when the scope changes or output is missing. Omitted output directories are managed under `$CODEX_HOME/.tmp/workflow-artifacts` and cleaned when app-server initializes after a restart. The workflow context points at the invoking workspace through `ctx.cwd`, `ctx.currentWorkingDirectory`, `ctx.repoRoot`, and `ctx.workingDirectory`; `process.cwd()` remains the workflow package directory. Workflow authoring commands such as `develop`, `describe`, `docs`, `edit`, and `fix` stage edits in a session-private `.workflow-staging/sessions/<session-id>` tree when they run from a live workflow session; validation runs against the staged copy, and `/workflow done` publishes staged changes back to the live workflow. CLI and app-server workflow commands still default to the one-shot path, but they can opt into the same staged behavior with an explicit session id (`codex workflow --stage-session-id <id> ...` or app-server `stageSessionId`), then publish or discard that staged session explicitly with `publish` or `discard`. When no formatter exists, the TUI falls back to pretty-printed JSON from the canonical workflow result. `/workflow done` exits to Default mode.
- Configuration: add `[features].workflows = true` to `config.toml`, restart `codex`, then use `/workflow` as the specialist mode for workflow work, `/workflow list` to enumerate workflows, `/workflow show <id>` to inspect one, `/workflow where <id>` to locate a known workflow path, `/workflow develop --location project --id <id> --command <command> [--title <title>] <description>` to scaffold a project workflow, `/workflow repair <id>` to run the validation-guided repair loop, or `codex workflow repair <id>` / `codex workflow fix <id>`. Direct workflow execution and registered aliases accept top-level object input via `--key value`; bare boolean flags map to `true`, `--input '{...}'` or `--input @file.json` passes raw JSON when you need lower-level control, and aliases with published option hints reject unknown flags. Some workflows also expose convenience flags such as `--findings confirmed|filtered|both`, which the shared parser maps into their JSON input as a string field. For app-server-owned runs, the workflow runner sets `CODEX_WORKFLOW_RUN_ID`, `CODEX_WORKFLOW_ORIGIN_THREAD_ID`, `CODEX_WORKFLOW_APPROVALS=delegate`, and `CODEX_WORKFLOW_APP_SERVER_URL` so JavaScript workflow code can report back over the same app-server socket. `CODEX_WORKFLOW_RUNTIME_MODE=process` remains a CLI/debug compatibility switch, but the app-server socket is the workflow control plane. Runtime startup uses `node_modules/.bin/bun` when a workflow supplies one; otherwise Codex uses its pinned managed Bun at `CODEX_HOME/workflows/.bin/bun`. Codex-launched Bun processes get isolated Bun install, cache, global package, and runtime transpiler cache paths under `CODEX_HOME/workflows/.env`; Node/tsx workflow execution is not a fallback. When workflows are enabled, Codex prefetches the pinned Bun package into `CODEX_HOME/workflows/.bin/`; runtime startup, validation, API extraction, and dependency repair also download it on demand when the binary is missing, so scaffolded `bun build` and `bun test` commands do not require a user-installed Bun binary. Workflow API contracts are TS-first: export `WorkflowInput`, `WorkflowOutput`, and optional `WorkflowFormats` from `src/workflow.ts`, then let `/workflow validate <id>` extract and publish the contract after all checks pass. If a workflow sets `validation.contractSmoke`, live validation runs that command, parses stdout as JSON, and refuses to publish when the output violates the extracted contract. Discovery and command completion prefer that last validated contract over any live, unvalidated source edits. Workflow layout is strict: implementation code belongs in `src/`, tests belong in `src/tests/`, and persistent state or database files belong in `state/`; `.gitignore` must ignore `node_modules/`, `artifacts/`, and `state/*` while allowing `state/.gitkeep`, and runtime state files must not be tracked. Every workflow must carry a local `tsconfig.json`, non-empty required `README.md` and `DESIGN.md` sections, a private ESM `codex-workflow-*` package, and Bun-backed `build`, `test`, and `run` package scripts. Every object output schema must declare non-empty `properties` or an explicit `additionalProperties` policy. Every workflow should keep `README.md` and `DESIGN.md` current, keep `validation.coverage` aligned with the required `positive`, `load`, `autocomplete`, `negative`, and `recovery` markers, configure `validation.contractSmoke`, and annotate test files with `// workflow-covers: ...` markers so `/workflow validate <id>` can mechanically verify the coverage contract. Validation rejects unfinished scaffolds, including the default echo source, placeholder load/autocomplete/positive tests, raw develop flags in metadata/docs, and `latest` dependency versions. Workflows must not depend on globally installed third-party packages; built-in platform modules are fine, but external packages must come from the workflow directory's local `package.json` and `node_modules`, unused runtime dependencies, Node/tsx/npm runtime scripts, Node engine pins, Node-only imports such as `node:sqlite`, npm/yarn/pnpm lockfiles, and `latest` package versions are rejected, and `workflow.yaml` `dependencies.runtime` / `dependencies.development` must match `package.json` `dependencies` / `devDependencies`. Workflows should be as stable as possible and recover when correctness is preserved. Workflow implementation is staged: a persistent `workflow-architect` owns `DESIGN.md`, fresh `workflow-arch-reviewer` agents review each design round until `0 findings`, then a persistent `workflow-coder` implements the workflow while fresh `workflow-code-reviewer` agents review each coding round until `0 findings`. `DESIGN.md` may only be changed by the architect; coder-side design changes must be raised as `DESIGN.md requests` and routed back through the architect review loop before coding resumes. `/workflow validate <id>` checks docs, layout, runtime state ignore/tracking, Bun-only package shape, dependency metadata, unused runtime dependencies, coverage markers, local packages, loadability, autocomplete readiness, output schema shape, contract smoke metadata/output, and runs the workflow validation commands/tests before reporting readiness; invalid validation returns a non-zero command exit code, and `/workflow run <id>` refuses invalid workflows before launch. `/workflow repair <id>` now emits progress while resolving, validating, applying deterministic fixes, refreshing local Bun dependencies when package or missing-module repairs need it, running AI fallback, committing, and completing; it reports the applied fixes, remaining findings, blocked findings, unsupported findings, validation command results, stop reason, and repair cycle counters in a structured `repair` result, and the command output message summarizes the applied fixes or stop reason with failure context. Workflow roots are `$CODEX_HOME/workflows`, `.codex/workflows`, and `[workflows].search_paths`; `[workflows]` also supports `default_location`, `repair_mode`, `max_repair_cycles`, `dependency_update_policy`, `commit_policy`, and `validation_profile`.
- Artifact API guard: `/workflow validate <id>` rejects low-level artifact API usage in workflow source, including old `ctx.artifacts.readState`-style calls and direct `artifact/...` app-server RPC strings. Use `ctx.artifacts.cache.ensure(...)` so scope hashing, changed files, output paths, and hit tracking stay owned by the SDK.
- Authoring guard: workflow architect, coder, and reviewer agents are write-confined to the detected workflow directory, meaning the nearest ancestor containing `workflow.yaml`. For a new project workflow, use `/workflow develop --location project --id <id> --command <command> <description>` first, or `codex workflow develop --location project --id <id> --command <command> <description>` from shell tools, then verify `workflow.yaml` exists with `/workflow where <id>` or `codex workflow where <id>` before delegating. Agents must work inside the discovered workflow directory only, and project workflow implementation must not move, delete, or rewrite global workflow directories. If no workflow directory is detected, workflow agents lose workspace write access rather than creating `workflow.yaml`, `src/`, or package files in the caller workspace root.
- Workflow app-server socket: TUI-owned app-server workflow runs expose a private per-process Unix socket under the system temp directory, pass its `unix://...` URL through `CODEX_WORKFLOW_APP_SERVER_URL`, and remove the socket file when the embedded acceptor shuts down. The public `codex app-server --listen unix://` and `app-server proxy` paths still use `$CODEX_HOME/app-server-control/app-server-control.sock` unless an explicit socket path is provided.
- Mentions: typing `$` includes `[Workflow]` rows. Selecting one inserts `$<workflow-id>` and stores a `workflow://...` binding with the root path and ID, so duplicate IDs across roots stay unambiguous. Submitting a workflow mention injects read-only workflow metadata and README context into the turn; it does not grant write access outside Workflow Mode.
- Debug recipe: trace `/workflow` through `slash_dispatch.rs`, `codex-rs/tui/src/app/workflows.rs`, `codex-rs/tui/src/app/event_dispatch.rs`, and `codex-rs/tui/src/chatwidget/workflows.rs`; trace the JavaScript workflow runtime bridge through `codex-rs/workflows/src/execute.rs` and `codex-rs/workflows/src/workflow_runtime.rs`; inspect emitted stderr markers with `__CODEX_WORKFLOW_EVENT__`; trace registration-backed discovery through `codex-rs/workflows/src/publication.rs`, `codex-rs/workflows/src/registry.rs`, and `codex-rs/artifactory/src/workflow_tool_registration.rs`. For workflow API discovery, compare `codex api --mcp-detail tools-and-auth-only`, SDK `ctx.api.read()`, SDK `ctx.workflows.registry.list()`, and app-server `apiCatalog/read` output. To inspect workflow tool registrations read-only, use `sqlite3 -readonly "$CODEX_HOME/artifactory_1.sqlite" "SELECT id, scope_key, source_key, state_dir, metadata_json, created_at_unix_sec, updated_at_unix_sec, last_hit_at_unix_sec FROM artifact_states WHERE namespace = 'workflow-tools' ORDER BY updated_at_unix_sec DESC;"` and then `sqlite3 -readonly "$CODEX_HOME/artifactory_1.sqlite" "SELECT path, kind, sha256 FROM artifact_sources WHERE state_id = ?1 ORDER BY path;"` for the chosen `state_id`; use `json_extract(metadata_json, '$.workflowId')`, `$.toolName`, `$.sourceHook`, `$.sourceDigest`, `$.publishedAtUnixSec`, `$.updatedAtUnixSec`, `$.refreshAfterUnixSec`, and `$.expiresAtUnixSec` to inspect the durable record itself.
- Token impact: observing external workflow threads does not add model-visible context to the active TUI thread. Selecting or typing a workflow mention adds that workflow's metadata and README as user-context for the submitted turn, so keep workflow docs concise. Registration-backed discovery avoids rescanning all workflow trees on every turn, but it does read and refresh artifactory rows for active tools.

## Workflow Quality Validation

- Description: Codex no longer injects a built-in `PostToolUse` hook to validate workflows after `Bash` or `apply_patch`. Workflow authoring commands still validate staged edits before publish or commit, so the workflow being changed fails closed if validation does not pass.
- Configuration: no repo hook config is required, and app-server `hooks/list` only reports configured user, project, managed, and plugin hooks. Workflow checks run through `/workflow validate <id>`, `/workflow repair <id>`, `codex workflow validate <id>`, and the staged publish or commit paths.
- Completion gate: `/workflow validate <id>` is necessary for workflow packages, but it is not sufficient evidence for Codex source changes. For changes touching workflow infrastructure, app-server workflow APIs, TUI workflow surfaces, or Workflow mode instructions, run `just workflow-dev-check` from the repo root before completion; it builds a fresh `target/debug/codex` and runs the workflow, protocol, app-server workflow, and CLI workflow checks. Treat any failure as a hard blocker, report the failing command, exit code, and first failing source location, and do not claim completion or continue to TUI e2e. The workflow self-e2e is a live Rust integration test suite in `codex-e2e-tests`: run `just workflow-self-e2e` or `just workflow-self-real-world-e2e` with an OpenAI credential available from `OPENAI_API_KEY`, `[model_providers.openai].token`, or current Codex ChatGPT auth. Missing credentials fail the suite. Both targets build `target/debug/codex`, run the scenarios in parallel with separate temp `CODEX_HOME` and `CODEX_SQLITE_HOME` roots per test, ask live Codex to implement multiple project workflows, and then run the generated workflows against Cargo-backed fixture scenarios from simple file stats through todo-sweep, release audit, and the current global `workflows/code-review/README.md` contract; stale installed binaries or older debug binaries are invalid evidence.
- Tuning: treat returned `WF-*` items like workflow code-review findings and keep iterating until they reach `0 findings`; if the right fix needs a design change, raise a `DESIGN.md request` under `WF-015` instead of editing `DESIGN.md` directly.
- Debug recipe: trace validation through `codex-rs/workflows/src/execute.rs`, `codex-rs/workflows/src/quality_hook.rs`, `codex-rs/workflows/src/validation_runner.rs`, and `codex-rs/workflows/src/registry.rs`; trace the source gate through the repo-root `just workflow-dev-check` recipe. Use explicit workflow validation commands to reproduce a package block reason, then use `just workflow-dev-check` to catch current-source build or API drift; hook discovery in `codex-rs/hooks/src/engine/discovery.rs` intentionally does not install a workflow quality `PostToolUse` hook.
- Token impact: explicit workflow validation and publish checks do not add model-visible context on their own. The main cost is validation command execution when a workflow is validated or published.

## Plugins

- Description: plugins can contribute skills, MCP servers, app integrations, and UI metadata. The TUI exposes install, enable, detail, and marketplace flows through `/plugins`.
- Configuration: user config uses `[plugins]`, `[marketplaces]`, and `features.plugins`; installed plugin state lives under Codex home plugin storage.
- Tuning: keep plugin capability summaries small, ensure generated skills and MCP servers have stable names, and prefer plugin-owned metadata over hardcoded TUI special cases.
- Debug recipe: run `/plugins`, inspect marketplace and installed tabs, then trace `FetchPluginsList`, install, auth, and enable events in the app-server path.
- Source entrypoints: `codex-rs/core-plugins/src/`, `codex-rs/core/src/plugins/`, `codex-rs/tui/src/chatwidget/plugins.rs`, `codex-rs/app-server/src/codex_message_processor/plugins.rs`.
- Token impact: plugin-provided skills and plugin capability summaries can add developer instructions, tool names, and MCP schemas. Keep descriptions concise and lazy-load details where possible.
- Guide rule: do not single out whatever is installed in a local `$CODEX_HOME`; installed plugin inventory is user-specific. Use `/plugins` for live state, and document a concrete external plugin here only when its source or manifest schema changes in this repo.

### Investigating Plugin Activity

- Start with `/plugins` for user-visible catalog, installed, enabled, install policy, and auth policy state. If it is empty, check `features.plugins`, workspace/plugin auth gating, configured `[marketplaces]`, and remote catalog behavior under `features.remote_plugin`.
- Trace the TUI list path as `/plugins` -> `ChatWidget::add_plugins_output` -> `prefetch_plugins` -> `AppEvent::FetchPluginsList` -> `App::fetch_plugins_list` -> app-server `plugin/list` -> `CodexMessageProcessor::plugin_list` -> `PluginsLoaded` -> `ChatWidget::on_plugins_loaded`.
- Trace detail/install/uninstall as `FetchPluginDetail` -> `plugin/read`, `FetchPluginInstall` -> `plugin/install`, and `FetchPluginUninstall` -> `plugin/uninstall`. Auth handoff uses `PluginInstallAuthAdvance` or `PluginInstallAuthAbandon`, and successful auth can force connector refresh.
- To prove a plugin affects a turn, inspect `plugins_for_config`, `PluginLoadOutcome`, `capability_summaries`, and `AvailablePluginsInstructions::from_plugins`. Explicit `plugin://` mentions are resolved in `collect_explicit_plugin_mentions` and can pull plugin-specific MCP/app inventory into the turn.
- To prove a plugin contributed runtime tools, inspect its `.codex-plugin/plugin.json` paths for `skills`, `mcpServers`, and `apps`, then cross-check `/skills`, `/mcp verbose`, and `/apps`. Plugin MCP servers are merged into `Config::to_mcp_config`; plugin app metadata is loaded during plugin detail reads.
- When plugin state looks stale, compare the cwd used in `FetchPluginsList` with the active config cwd, then check `PluginsCacheState`, marketplace load errors, non-curated cache refresh, and duplicate plugin MCP server warnings.

### Internal Plugin-Like Surfaces

- `model-router`: routes normal chat turns plus internal model calls for subagents and modules. User surfaces are `/model-router enable|disable|inherit`, `codex model-router policy|lifecycle|shadows|promote|demote|tune`, and `codex model-router report show|apply`.
- `tool-router`: keeps tool dispatch and telemetry centralized while advertising the normal tool schemas directly. User-facing maintenance is mostly `codex tool-router tune` plus telemetry and state inspection.
- `implement`: owns targeted review/fix cycles after agent edits. User surfaces are `/implement enable|disable|inherit|implicit --max-cycles=N [task]` and `codex implement enable|disable|implicit --max-cycles=N`.
- `skills`: owns bundled and local `SKILL.md` discovery, plugin-provided skills, enablement rules, and the model-visible `<skills_instructions>` block.
- `mcp/apps`: owns configured MCP servers, connector-backed apps, plugin-provided `.mcp.json` and `.app.json` files, OAuth/auth status, and model-visible tool schemas.
- `memories`: owns memory instructions, thread memory mode, idle-thread extraction, consolidation, and `/memories` settings.
- `config/debug/status`: owns `/debug-config`, `/status`, feature flags, config provenance, token usage reporting, and rate-limit/status display.

### External Marketplace Catalog Snapshot

The temporary marketplace snapshot under `$CODEX_HOME/.tmp/plugins/plugins/` contains these catalog entries. Treat this as catalog data, not installed or enabled state. Surfaces are `skills`, `app`, and `mcp`; capabilities come from each manifest when present.

- `alpaca` (Alpaca) [Research, app]: market research and trading data access.
- `amplitude` (Amplitude) [Productivity, app]: product analytics and funnel exploration.
- `atlassian-rovo` (Atlassian Rovo) [Productivity, skills+app, Interactive/Write]: Jira and Confluence workflows.
- `attio` (Attio) [Productivity, app]: CRM records and customer relationship workflows.
- `binance` (Binance) [Research, app]: read-only Binance market data exploration.
- `biorender` (BioRender) [Design, app]: scientific figure creation workflows.
- `box` (Box) [Productivity, skills+app]: document search and reference workflows.
- `brand24` (Brand24) [Productivity, app]: brand mentions, sentiment, and media monitoring.
- `brex` (Brex) [Productivity, app]: company finance review through Brex data.
- `build-ios-apps` (Build iOS Apps) [Coding, skills+mcp, Interactive/Read/Write]: iOS app building, SwiftUI, App Intents, and Xcode workflows.
- `build-macos-apps` (Build macOS Apps) [Coding, skills, Interactive/Read/Write]: macOS app building, SwiftUI, AppKit, debugging, and instrumentation.
- `build-web-apps` (Build Web Apps) [Coding, skills, Interactive/Read/Write]: frontend web apps, browser testing, payments, databases, and generated assets.
- `canva` (Canva) [Productivity, skills+app]: search, create, and edit Canva designs.
- `carta-crm` (Carta CRM) [Productivity, app]: deal flow, company, and relationship tracking.
- `cb-insights` (CB Insights) [Research, app]: private markets research.
- `channel99` (Channel99) [Productivity, app]: go-to-market performance intelligence.
- `chatgpt-apps` (ChatGPT Apps) [Coding, skills, Interactive/Read/Write]: ChatGPT app development and submission preparation.
- `circleback` (Circleback) [Productivity, app]: meeting notes, action items, and conversation follow-up.
- `circleci` (CircleCI) [Coding, skills, Interactive/Write]: CI build, test, and deploy workflows.
- `clickup` (ClickUp) [Productivity, app]: ClickUp task and workspace workflows.
- `cloudflare` (Cloudflare) [Coding, skills+mcp, Interactive/Write]: Cloudflare platform guidance with official MCP support.
- `cloudinary` (Cloudinary) [Coding, app]: media library management, search, and transformations.
- `coderabbit` (CodeRabbit) [Coding, skills, Interactive/Write]: AI-powered code review for current changes.
- `codex-security` (Codex Security) [Engineering, skills, Interactive/Read/Write]: security scanning for codebases.
- `cogedim` (Cogedim) [Lifestyle, app]: French real-estate developer workflows.
- `common-room` (Common Room) [Productivity, app]: buyer intelligence and go-to-market context.
- `conductor` (Conductor) [Productivity, app]: search, visibility, sentiment, and performance metrics.
- `coupler-io` (Coupler.io) [Productivity, app]: analysis across marketing, finance, sales, ecommerce, and business data.
- `coveo` (Coveo) [Productivity, app]: enterprise content search.
- `cube` (Cube) [Research, app]: Cube metrics for actuals, budgets, forecasts, and variance analysis.
- `daloopa` (Daloopa) [Research, app]: fundamental data from filings, presentations, and public-company sources.
- `demandbase` (Demandbase) [Productivity, app]: B2B sales, marketing, and GTM data.
- `docket` (Docket) [Productivity, app]: sales knowledge retrieval.
- `domotz-preview` (Domotz Preview) [Productivity, app]: network infrastructure monitoring and management.
- `dovetail` (Dovetail) [Productivity, app]: customer-feedback research and decision support.
- `dow-jones-factiva` (Dow Jones Factiva) [Research, app]: premium news archive search.
- `egnyte` (Egnyte) [Productivity, app]: Egnyte file and document workflows.
- `expo` (Expo) [Coding, skills, Interactive/Read/Write]: Expo and React Native build, deploy, upgrade, and debugging workflows.
- `figma` (Figma) [Design, skills+app, Interactive/Read/Write]: design-to-code workflows from Figma context.
- `finn` (FINN) [Lifestyle, app]: flexible car subscription workflows.
- `fireflies` (Fireflies) [Productivity, app]: meeting and knowledge retrieval.
- `fyxer` (Fyxer) [Productivity, app]: email drafting in the user's voice.
- `game-studio` (Game Studio) [Coding, skills, Interactive/Write]: browser-game design, prototyping, and shipping workflows.
- `github` (GitHub) [Coding, skills+app, Interactive/Write]: PR, issue, CI, and publishing workflows.
- `gmail` (Gmail) [Productivity, skills+app, Interactive/Write]: Gmail reading, triage, and management.
- `google-calendar` (Google Calendar) [Productivity, skills+app, Interactive/Write]: calendar scheduling and event management.
- `google-drive` (Google Drive) [Productivity, skills+app, Interactive/Write]: Drive, Docs, Sheets, and Slides workflows.
- `govtribe` (GovTribe) [Research, app]: government contracts, awards, and vendor search.
- `granola` (Granola) [Productivity, app]: meeting-history context retrieval.
- `happenstance` (Happenstance) [Productivity, app]: professional-network search.
- `help-scout` (Help Scout) [Productivity, app]: Help Scout mailbox and conversation sync.
- `highlevel` (HighLevel) [Productivity, app]: CRM, automation, and client communication workflows.
- `hostinger` (Hostinger) [Coding, app]: website and app creation through Hostinger Horizons.
- `hubspot` (HubSpot) [Productivity, app]: HubSpot CRM analysis and record management.
- `hugging-face` (Hugging Face) [Coding, skills+app, Interactive/Read/Write]: model, dataset, Space, and research inspection.
- `hyperframes` (HyperFrames by HeyGen) [Design, skills, Read/Write]: HTML-driven video rendering.
- `jam` (Jam) [Productivity, app]: screen recording with context.
- `keybid-pulse` (KeyBid Pulse) [Productivity, app]: short-term rental ROI calculation.
- `life-science-research` (Life Science Research) [Research, skills, Interactive/Read/Write]: life-sciences research, evidence synthesis, and optional parallel analysis.
- `linear` (Linear) [Productivity, skills+app]: issue and project lookup.
- `marcopolo` (MarcoPolo) [Coding, app]: secure container workflows for data-backed analysis.
- `mem` (Mem) [Productivity, app]: Mem knowledge-base context.
- `minimal-plugin` (Minimal Plugin) [Coding, skills, Interactive/Write]: small valid plugin fixture for plugin-eval testing.
- `monday-com` (Monday.com) [Productivity, app]: monday.com workspace interaction.
- `moody-s` (Moody's) [Research, app]: credit and risk intelligence.
- `morningstar` (Morningstar) [Research, app]: investment and fund research.
- `motherduck` (MotherDuck) [Productivity, app]: MotherDuck data warehouse access.
- `mt-newswires` (MT Newswires) [Research, app]: real-time global financial news.
- `multi-skill-plugin` (Multi Skill Plugin) [Coding, skills, Interactive/Write]: fixture exposing two skills for plugin-eval testing.
- `myregistry-com` (MyRegistry.com) [Lifestyle, app]: gift registry workflows.
- `neon-postgres` (Neon Postgres) [Coding, skills+app, Interactive/Write]: Neon Serverless Postgres project and database management.
- `netlify` (Netlify) [Coding, skills+app, Interactive/Write]: deployment and release management.
- `network-solutions` (Network Solutions) [Productivity, app]: domain search and availability workflows.
- `notion` (Notion) [Productivity, skills+app, Interactive/Read/Write]: specs, research, meetings, and knowledge capture.
- `omni-analytics` (Omni Analytics) [Productivity, app]: querying Omni through the team's semantic model and permissions.
- `otter-ai` (Otter.ai) [Productivity, app]: meeting intelligence search and retrieval.
- `outlook-calendar` (Outlook Calendar) [Productivity, skills+app, Interactive/Write]: Outlook schedule and meeting changes.
- `outlook-email` (Outlook Email) [Productivity, skills+app, Interactive/Write]: Outlook inbox triage and draft replies.
- `particl-market-research` (Particl Market Research) [Research, app]: ecommerce market research.
- `pipedrive` (Pipedrive) [Productivity, app]: Pipedrive deal and contact sync.
- `pitchbook` (PitchBook) [Research, app]: private capital market data.
- `plugin-eval` (Plugin Eval) [Coding, skills, Interactive/Write]: local plugin evaluation and benchmarking.
- `policynote` (PolicyNote) [Research, app]: policy and regulatory intelligence.
- `pylon` (Pylon) [Productivity, app]: customer support search, management, and resolution.
- `quartr` (Quartr) [Research, app]: investor-relations data from public companies.
- `quicknode` (Quicknode) [Coding, app]: Quicknode infrastructure management.
- `ranked-ai` (Ranked AI) [Productivity, app]: SEO and PPC software workflows.
- `razorpay` (Razorpay) [Productivity, app]: Razorpay payment data access.
- `read-ai` (Read AI) [Productivity, app]: meeting intelligence workflows.
- `readwise` (Readwise) [Research, app]: Readwise and Reader access.
- `remotion` (Remotion) [Design, skills, Read/Write]: motion-graphics creation from prompts.
- `render` (Render) [Coding, skills, Interactive/Write]: deploy, debug, monitor, and migrate Render apps.
- `responsive` (Responsive) [Productivity, app]: organizational data workflows.
- `scite` (Scite) [Research, app]: peer-reviewed research answers with verifiable sources.
- `semrush` (Semrush) [Productivity, app]: SEO and traffic data for domains, keywords, backlinks, and competitors.
- `sendgrid` (SendGrid) [Coding, app]: SendGrid email API interaction.
- `sentry` (Sentry) [Productivity, skills, Interactive/Write]: recent Sentry issue and event inspection.
- `setu-bharat-connect-billpay` (Setu Bharat Connect BillPay) [Lifestyle, app]: utility bill payment workflows.
- `sharepoint` (SharePoint) [Productivity, skills+app, Interactive/Write]: SharePoint site and file summarization.
- `signnow` (SignNow) [Productivity, app]: document signing workflows.
- `skywatch` (SkyWatch) [Productivity, app]: satellite imagery search and exploration.
- `slack` (Slack) [Productivity, skills+app, Interactive/Write]: Slack reading, triage, and management.
- `statsig` (Statsig) [Coding, app]: Statsig workspace access.
- `streak` (Streak) [Productivity, app]: Gmail-native CRM tracking.
- `stripe` (Stripe) [Productivity, skills+app]: payments and business tools.
- `supabase` (Supabase) [Coding, skills+app, Read/Write]: Supabase project and database workflows.
- `superpowers` (Superpowers) [Coding, skills, Interactive/Read/Write]: planning, TDD, debugging, and delivery workflows for coding agents.
- `taxdown` (Taxdown) [Research, app]: tax Q&A for individuals and freelancers in Spain.
- `teams` (Teams) [Productivity, skills+app, Interactive/Write]: Teams summaries and follow-ups.
- `teamwork-com` (Teamwork.com) [Productivity, app]: Teamwork project and task sync.
- `temporal` (Temporal) [Coding, skills, Read/Write]: Temporal application development and platform lifecycle management.
- `test-android-apps` (Test Android Apps) [Coding, skills, Interactive/Read]: Android emulator issue reproduction, UI inspection, and performance evidence.
- `third-bridge` (Third Bridge) [Research, app]: financial and industry-expert research context.
- `tinman-ai` (Tinman AI) [Research, app]: home-financing underwriting scenarios.
- `united-rentals` (United Rentals) [Productivity, app]: equipment selection workflows.
- `vantage` (Vantage) [Coding, app]: cloud observability and cost optimization.
- `vercel` (Vercel) [Coding, skills+app, Interactive/Write]: web app and agent build and deploy workflows.
- `waldo` (Waldo) [Productivity, app]: agency and brand strategy workflows.
- `weatherpromise` (WeatherPromise) [Lifestyle, app]: trip rain-protection workflows.
- `windsor-ai` (Windsor.ai) [Productivity, app]: marketing and business data source analysis.
- `yepcode` (YepCode) [Coding, app]: custom AI tools backed by JSON Schema-defined code execution.

## Skills

- Description: skills are local `SKILL.md` instruction bundles. They are listed in model-visible skill instructions and can be explicitly invoked with `$skill` mentions or natural-language trigger rules.
- Configuration: `[skills] include_instructions`, `[skills.bundled] enabled`, and `[[skills.config]]` entries control automatic instructions and per-skill enablement.
- Tuning: keep `SKILL.md` trigger descriptions precise, read only the needed referenced files, and rely on scripts/assets inside the skill instead of copying large blocks into chat.
- Debug recipe: use `/skills` for the list and enable/disable UI, inspect `SkillsListLoaded`, and check context warnings for exceeded skill metadata budgets.
- Source entrypoints: `codex-rs/core-skills/src/`, `codex-rs/core/src/skills.rs`, `codex-rs/core/src/context/available_skills_instructions.rs`, `codex-rs/tui/src/chatwidget/skills.rs`, `codex-rs/tui/src/skills_helpers.rs`.
- Token impact: skill metadata is budgeted to 2 percent of the context window when known, otherwise an 8000-character fallback. Extra skills may have descriptions truncated or omitted.

## MCP And Apps

- Description: MCP servers expose tools/resources from configured local or remote servers. Apps are connector-backed integrations that appear to the model as MCP tools under the Codex apps server.
- Configuration: use `[mcp_servers.<name>]`, MCP OAuth settings, `features.connectors`, and app enablement state. HTTP bearer tokens should use `bearer_token_env_var`. Local stdio process reuse is controlled by `process_reuse_scope = "none" | "cwd" | "project" | "repo" | "user"`; omitted means `cwd`, HTTP servers only accept the default, and `user` requires an explicit absolute stdio `cwd`.
- Tuning: prefer `codex-rs/codex-mcp/src/connection_manager.rs` for MCP tool and tool-call mutation; keep tool names stable and schemas narrow. For process reuse, use `none` for mutable local state, unknown safety, or bad reuse behavior; `cwd` for workspace-sensitive filesystem/repo MCPs; `project` for project-root cache/index servers safe across subdirectories; `repo` for Git-root/repo-aware servers safe across cwd values in one checkout/worktree; and `user` for service-backed stdio MCPs such as Jenkins or GitHub that do not read workspace state.
- Debug recipe: run `codex mcp list --json`, `codex mcp get <name> --json`, and `/mcp verbose` for server/tool/auth status, `/apps` for connector access, and inspect `FetchMcpInventory` or connector refresh events when the TUI differs from config. When tuning `process_reuse_scope`, investigate every configured MCP, inspect config files, visible process trees, command/args/cwd/env var names, package docs, and local MCP source if the command points into a checkout before changing scope. Never print secret env values; reason from env var names and code paths.
- Source entrypoints: `codex-rs/codex-mcp/src/`, `codex-rs/codex-mcp/src/broker/mod.rs`, `codex-rs/config/src/mcp_types.rs`, `codex-rs/core/src/config/mod.rs`, `codex-rs/core/src/apps/`, `codex-rs/core/src/context/apps_instructions.rs`, `codex-rs/core/src/realtime_tool_context.rs`, `codex-rs/tui/src/history_cell.rs`, `codex-rs/tui/src/chatwidget.rs`.
- Token impact: MCP tool schemas and app instructions increase the available tool surface. Use `tool_search` and app lazy-loading paths when a full tool list would be too large. Realtime startup context includes only bounded MCP inventory summaries so voice can delegate app/MCP work without loading full schemas.

## Memories

- Description: memories summarize useful prior thread context and can inject memory usage instructions or generate new memory artifacts after idle threads.
- Configuration: `[memories]` supports `use_memories`, `generate_memories`, `disable_on_external_context`, rollout age/idle limits, and extraction/consolidation model overrides. `/memories` toggles use and generation from the TUI.
- Tuning: keep extraction and consolidation prompts focused, avoid generating memories from externally polluted context when that setting is enabled, and tune rollout limits before widening model prompts.
- Debug recipe: use `/memories`, inspect memory-related startup events and state DB rows, then check phase 1 extraction, storage, and phase 2 selection tests.
- Source entrypoints: `codex-rs/core/src/memories/`, `codex-rs/core/src/memories/README.md`, `codex-rs/state/src/runtime/memories.rs`, `codex-rs/tui/src/bottom_pane/memories_settings_view.rs`.
- Token impact: memory instructions and selected memories consume context. Prefer fewer high-signal memories over broad summaries.

## Repo CI

## Implement

- Description: implement runs targeted review/fix cycles after a regular agent turn changes files. It groups findings by owned file or module and applies bounded worker fixes before the turn finishes.
- User surfaces: `/implement enable|disable|inherit|implicit --max-cycles=N` changes the current thread. `/implement [--max-cycles=N] <task>` submits one turn with implement review/fix forced on. `codex implement enable|disable|implicit --max-cycles=N` persists user config under `[implement]`.
- Configuration: `[implement] enabled = true|false` controls the loop. `[implement] mode = "auto"` runs after normal agent edits; `mode = "implicit"` runs only for `/implement <task>` turns. `[implement] max_cycles = N` sets the review/fix cycle budget.
- Tuning: use `/implement disable` when validation should still run but review/fix should not. Use `/implement implicit --max-cycles=N` when review/fix should be opt-in per turn. Use `/implement enable --max-cycles=N` for automatic thread-local iteration budgets.
- Debug recipe: trace `effective_config` in `codex-rs/config/src/state.rs`, then inspect targeted review status events and the fix-worker labels.
- Source entrypoints: `codex-rs/config/src/state.rs`, `codex-rs/config/src/config_toml.rs`, `codex-rs/core/src/session/turn_context.rs`, `codex-rs/core/src/session/review.rs`, `codex-rs/cli/src/main.rs`, `codex-rs/tui/src/chatwidget/slash_dispatch.rs`, `codex-rs/tui/src/chatwidget/plan_implementation.rs`.
- Token impact: review prompts include diff context and selected findings. Lower `max_cycles` or narrow `review_issue_types` when the loop consumes too much context.

## Model Router

- Description: model-router is the internal model-selection and live-shadow evaluation surface. It applies to normal chat turns and internal model calls, using a task key from `ModelRouterSource` plus prompt size, discovery mode, hard eligibility rules, score biases, lifecycle promotion state, measured candidate metrics, pricing, context limits, failover exclusions, and state overlays. Normal chat task keys are `chat.default`, `chat.plan`, and `chat.codex`; direct internal calls include keys such as `subagent.review`, `subagent.compact`, `subagent.memory_consolidation`, `module.guardian.review`, `module.memories.extract`, and `module.tool_router.tune`. Latency-sensitive task classification treats `tool`, `mcp`, and `triage` keys ahead of generic `review`/`fix`/`commit` buckets. Candidate quality and latency come from explicit candidate metrics, tuned overlays, or live shadow/judge rows; model names are not used as quality or latency guesses.
- User surfaces: `/model-router enable|disable|inherit|status` changes or inspects only the current thread. `codex model-router policy --task-key <key>` inspects effective candidates, hard-rule eligibility, score bias, and lifecycle gates. `codex model-router usage --window 7d --group-by request-kind` reports production cost, counterfactual cost, overhead, savings, tokens, price confidence, and coverage gaps from `model_router_ledger`. `codex model-router lifecycle --events --window 30d --candidate-identity <key>` shows current lifecycle status, promotion/demotion/blocked counts, auto/manual splits, reasons, and event timeline rows; JSON includes `promotions`, `stats`, and `events`. `codex model-router shadows` lists shadow validation/monitoring samples, `codex model-router promote|demote` edits lifecycle state and appends manual lifecycle events, and `codex model-router tune` plus `report show|apply` manage metric overlays.
- Configuration: `[model_router] enabled = true` activates routing, automatic live shadow tests for discovered alternatives, and stateful lifecycle promotion gates by default. Provider entries use the Responses protocol by default; the built-in `deepseek` provider id uses the internal Chat Completions adapter and feeds the same model request/event abstraction before router accounting. DeepSeek reads `DEEPSEEK_API_KEY` by default, or `[model_providers.deepseek] token = "sk-..."` can provide a config-backed bearer token while preserving the built-in adapter. Amazon Bedrock is opt-in for curated discovery: set `[model_providers.amazon-bedrock] enabled = true`; only then do AWS profile/region config or AWS environment credentials make it ready. Built-in `ollama` and `lmstudio` now require an explicit address before curated discovery or tune will consider them ready: configure `[model_providers.ollama] base_url = "http://host:11434/v1"` or `[model_providers.lmstudio] base_url = "http://host:1234/v1"`, or provide the shared experimental `CODEX_OSS_BASE_URL` / `CODEX_OSS_PORT` env override before startup. With `discovery = "curated"`, every ready non-active provider in `model_providers` is discovered automatically and receives live shadow/judge samples after normal completed turns; no `[[model_router.candidates]]` entry is needed after a provider becomes ready. Ready means the provider passes config-readiness checks: a non-empty base URL plus the auth/config signal that provider type needs, such as a config token, populated `env_key`, command auth, configured HTTP headers, configured AWS auth, or an explicitly configured OSS address. Curated discovery also uses the incumbent, explicit candidates, and active-provider available models, expanding OpenAI-compatible providers through `/models` and static-catalog providers through their provider model manager. CLI diagnostic surfaces such as `codex model-router policy` and `codex model-router tune` refresh the active-provider model catalog with the normal `OnlineIfUncached` strategy, so a fresh `models_cache.json` entry can introduce candidates such as Spark even in a new process; live turn routing reads the already-loaded catalog to avoid adding catalog refresh latency to chat. `manual` uses only explicit candidates; `from_rules` infers candidates from policy and lifecycle model selectors, expanding regexes against discovered provider catalogs. `[[model_router.models.rules]]` applies `require`/`exclude`, `[[model_router.bias.rules]]` adds score bias, and `[model_router.lifecycle.defaults]` plus `[[model_router.lifecycle.rules]]` tune the shadow window, budgets, gates, sample-rate caps, and auto promote/demote behavior. Set `shadow_allowed = false` in lifecycle defaults or a matching rule only when a route should bypass live shadow promotion. `tasks`, `except_tasks`, `provider`, and `model` selectors accept exact strings or `/regex/` Rust regexes.
- Tuning: keep task-class inference, measured candidate metrics, pricing, account-pool behavior, hard rules, and failover exclusions aligned. Lifecycle rules inherit defaults and override only fields they set. Normal completed turns automatically test one under-sampled eligible alternative by replaying the final prompt without tools, judging it against the production answer, and persisting `model_router_shadow_evaluations` plus shadow/judge ledger rows. `codex model-router tune` remains useful for bulk historical replay; it uses lifecycle defaults for window and budget when CLI flags are omitted, prints the effective budget and discovered candidate list to stderr before replay starts, evaluates explicit and auto-discovered candidates, persists promotion samples, records replay/judge overhead, and can run dry-run/report flows before applying metric overlays. Use `codex model-router usage --group-by task|model|day|request-kind --json` to find savings regressions, missing prices, low confidence rows, zero-token rows, and production rows without counterfactual coverage.
- Debug recipe: start with `codex model-router policy --task-key chat.default --json` for regular turns, `--task-key subagent.review` for review helpers, `--task-key module.guardian.review` for guardian approval reviews, or `--task-key module.memories.extract` / `subagent.memory_consolidation` for memory pipelines, then trace `ModelRouterSource::task_key`, `RouterTaskClass::infer`, discovery expansion, `apply_model_router_policy`, filtered selectable routes, selected route, failover exclusion scope, lifecycle promotion state, metric overlays, and app-server `thread/modelRouterSessionConfig/set` notifications. If a review-adjacent path is not using Spark, check whether the task key resolves to `module.review.triage` versus `subagent.review`, then inspect `median_latency_ms` on the candidate or `model_router_metric_overlays`; there is no model-name latency fallback.
- SQLite recipe: state lives in `state_5.sqlite` under `CODEX_SQLITE_HOME` when that env var is set, otherwise under the Codex home directory used by config loading. Use `sqlite3 -readonly "$CODEX_SQLITE_HOME/state_5.sqlite"` when the env var is set, or copy the DB with `-wal` and `-shm` files before inspection. Useful reads: `SELECT task_key,status,candidate_identity,base_candidate_identity,updated_at_ms,reason FROM model_router_lifecycle_promotions ORDER BY updated_at_ms DESC LIMIT 20;`, `SELECT datetime(created_at_ms/1000,'unixepoch') created,event_type,source,task_key,candidate_identity,previous_status,next_status,reason,shadow_phase,shadow_evaluated_count,shadow_success_rate,shadow_average_confidence,shadow_latest_evaluation_id,failed_gates_json FROM model_router_lifecycle_events ORDER BY created_at_ms DESC,id DESC LIMIT 50;`, `SELECT task_key,request_kind,COUNT(*) requests,SUM(total_tokens) tokens,SUM(CASE WHEN request_kind='production' THEN actual_cost_usd_micros ELSE 0 END) production_cost,SUM(CASE WHEN request_kind='production' THEN counterfactual_cost_usd_micros ELSE 0 END) counterfactual_cost,SUM(CASE WHEN request_kind!='production' THEN actual_cost_usd_micros ELSE 0 END) overhead_cost,AVG(price_confidence) confidence FROM model_router_ledger GROUP BY task_key,request_kind ORDER BY task_key,request_kind;`, and `SELECT task_key,phase,candidate_identity,base_candidate_identity,COUNT(*) evals,AVG(confidence) confidence,AVG(success) success_rate,SUM(total_tokens) tokens,SUM(cost_usd_micros) cost_micros,MAX(id) latest_eval_id FROM model_router_shadow_evaluations GROUP BY task_key,phase,candidate_identity,base_candidate_identity ORDER BY task_key,phase;` Metric overlays remain in `model_router_metric_overlays`, tune reports in `model_router_tune_runs` and `model_router_tune_results`, and production/overhead accounting in `model_router_ledger`.
- Source entrypoints: `codex-rs/model-router/src/lib.rs`, `codex-rs/model-router/src/policy.rs`, `codex-rs/core/src/model_router.rs`, `codex-rs/core/src/model_router/`, `codex-rs/core/src/session/model_router_shadow.rs`, `codex-rs/core/src/model_router_tune.rs`, `codex-rs/core/src/client.rs`, `codex-rs/codex-api/src/endpoint/chat_completions.rs`, `codex-rs/codex-api/src/endpoint/responses.rs`, `codex-rs/state/migrations/0037_model_router_lifecycle.sql`, `codex-rs/state/migrations/0038_model_router_lifecycle_events.sql`, `codex-rs/state/src/runtime/model_router.rs`, `codex-rs/state/src/runtime/model_router/lifecycle.rs`, `codex-rs/config/src/config_toml.rs`, `codex-rs/cli/src/main.rs`.
- Live e2e: run `just model-router-live-e2e` from the repo root with an OpenAI credential available from `OPENAI_API_KEY`, `[model_providers.openai].token`, or current Codex ChatGPT auth, and a DeepSeek credential from `DEEPSEEK_API_KEY` or `[model_providers.deepseek].token`. The target builds a fresh `codex` binary and runs the `codex-e2e-tests --test model_router_live_e2e` Rust test. Missing credentials fail the suite.
- Token impact: prompt byte estimates and effective context windows directly affect candidate eligibility. Production ledger rows are written from routed chat and internal model responses; tune replay, judge, and shadow/monitoring evaluation rows are router overhead. Net savings is production counterfactual cost minus actual production cost minus model-router overhead.

## Tool Router

- Description: tool-router is the internal structured-tool dispatch and telemetry surface. With `features.tool_router` enabled, Codex advertises the normal tool schemas directly instead of a bundled model-visible `tool_router` function; `tool_search` is part of tool-router and remains advertised for search-capable models when deferred tools are discoverable. Runtime tool calls still pass through `ToolRouter`, which records the tool input, output, success/outcome, source, model response ordinal, prompt snapshot, previous prompt snapshot, and dialog locator metadata for replay-oriented investigations. Remembered tool selectors are persisted in `tool_router_remembered_tools` for diagnostics and tuning history, but they do not add extra model-visible schemas in direct-advertisement mode.
- User surfaces: there is no TUI slash command. `features.tool_router` controls the dispatch/accounting mode, `codex tool-router tune --introspect` analyzes telemetry with a model-router-selected introspection model, and `tool_router_ledger` is the main runtime debugging surface.
- Configuration: direct tool schemas come from the standard tool registry and MCP/app/dynamic tool loading. `tool_search` is controlled by `features.tool_router`, the active model's search-tool capability, and deferred tool inventory; there is no separate `tool_search` feature flag.
- Tuning: inspect repeated failures by `route_kind`, `selected_tools_json`, `tool_name`, `tool_call_source`, `outcome`, and prompt snapshots. Dynamic guidance applies only to legacy model-visible router prompt info, so direct-advertisement issues usually need tool schema, tool description, policy, or registry fixes.
- Debug recipe: inspect the raw tool input/output JSON, selected tool, invalid route errors, outcome breakdowns, toolset hash, model response ordinal, prompt snapshots, and dialog locator JSON. Use `thread_id`/`turn_id`/`call_id` plus `prompt_json` and `previous_prompt_json` to reconstruct the model request that produced a tool call.
- Source entrypoints: `codex-rs/tools/src/tool_router.rs`, `codex-rs/tools/src/tool_router_prompt.rs`, `codex-rs/tools/src/tool_discovery.rs`, `codex-rs/tools/src/tool_registry_plan.rs`, `codex-rs/core/src/tool_router_tune.rs`, `codex-rs/state/src/runtime/tool_router.rs`, `codex-rs/cli/src/main.rs`.
- Token impact: direct advertisement spends the normal visible tool-schema tokens while avoiding an extra router prompt. The ledger still records visible and hidden schema token estimates so changes in tool inventory can be measured over time.

## Config And Debug Surfaces

- Description: config is layered from defaults, files, requirements, CLI overrides, and session overrides; debug surfaces show the effective values and their sources.
- Configuration: `config.toml`, requirements files, feature flags, app-server session config, and command-line overrides all participate in the final `Config`.
- Tuning: when changing `ConfigToml` or nested config types, update schema with `just write-config-schema` and keep debug output useful for provenance questions.
- Debug recipe: run `/debug-config`, compare against `config.schema.json`, and inspect config loader tests for precedence or validation regressions.
- Source entrypoints: `codex-rs/config/src/config_toml.rs`, `codex-rs/core/src/config/`, `codex-rs/tui/src/debug_config.rs`, `codex-rs/core/config.schema.json`.
- Token impact: config changes can alter model, tools, instructions, sandboxing, memories, skills, and output budgets. Call out these effects in PRs.

## SQLite State Records

- Description: Codex mirrors rollout metadata and runtime diagnostics into SQLite. The main DB is `state_5.sqlite`; tracing logs live in `logs_2.sqlite`. Both are opened from resolved `Config.sqlite_home`, which defaults to `sqlite_home` in `config.toml`, then `CODEX_SQLITE_HOME`, then `CODEX_HOME`.
- Safety recipe: inspect live DBs read-only with `sqlite3 -readonly`. SQLite runs in WAL mode, so if you need a copy, use `.backup` or copy the `*.sqlite`, `*.sqlite-wal`, and `*.sqlite-shm` files together. Do not hand-edit production rows; add migrations and runtime APIs instead.
- Schema recipe: start with `.tables`, then `PRAGMA table_info(threads);`, `.schema tool_router_ledger`, or the source migrations under `codex-rs/state/migrations/` and `codex-rs/state/logs_migrations/`.
- Startup RPC hang recipe: TUI startup app-server calls (`account/read`, `model/list`, `thread/start`, `thread/resume`, `thread/fork`, `externalAgentConfig/detect`, and `externalAgentConfig/import`) time out after 30 seconds with the method-specific context. If startup appears wedged, inspect `logs_2.sqlite` for `app-server typed request` rows; the `method` and `request_id` fields identify the last startup RPC that entered the app-server path.
- Thread lookup recipe:

```sql
SELECT id, datetime(updated_at_ms / 1000, 'unixepoch') AS updated, cwd, title, tokens_used, rollout_path
FROM threads
ORDER BY updated_at_ms DESC
LIMIT 20;
```

- Regular sessions recipe: regular user sessions normally have `source IN ('cli', 'vscode', 'exec')` and no agent metadata. Keep `has_user_event = 1` if you want conversations the user actually started, and drop the `cwd` predicate when investigating across workspaces.

```sql
SELECT id,
       datetime(created_at_ms / 1000, 'unixepoch') AS created,
       datetime(updated_at_ms / 1000, 'unixepoch') AS updated,
       source, cwd, title, first_user_message, model, reasoning_effort, tokens_used, rollout_path
FROM threads
WHERE archived = 0
  AND has_user_event = 1
  AND source IN ('cli', 'vscode', 'exec')
  AND agent_nickname IS NULL
  AND agent_role IS NULL
  AND agent_path IS NULL
ORDER BY updated_at_ms DESC
LIMIT 50;
```

- Token usage recipe: `threads.tokens_used` is only the latest total mirrored from rollout `TokenCount` events. For full input/cached/output/reasoning breakdown, read the rollout at `threads.rollout_path` or inspect live `ThreadTokenUsageUpdated`. Router-specific token records are in `model_router_ledger` and `tool_router_ledger`.
- Model-router SQL recipe:

```sql
SELECT datetime(created_at_ms / 1000, 'unixepoch') AS created, task_key, request_kind, model_provider, model,
       total_tokens, input_tokens, cached_input_tokens, output_tokens, reasoning_output_tokens,
       actual_cost_usd_micros, counterfactual_cost_usd_micros, outcome
FROM model_router_ledger
ORDER BY created_at_ms DESC
LIMIT 20;
```

- Model-router lifecycle event recipe:

```sql
SELECT datetime(created_at_ms / 1000, 'unixepoch') AS created,
       event_type, source, task_key, candidate_identity,
       previous_status, next_status, reason,
       shadow_phase, shadow_evaluated_count, shadow_success_rate,
       shadow_average_confidence, shadow_latest_evaluation_id,
       failed_gates_json
FROM model_router_lifecycle_events
ORDER BY created_at_ms DESC, id DESC
LIMIT 50;
```

- Tool-router SQL recipe:

```sql
SELECT datetime(created_at_ms / 1000, 'unixepoch') AS created, thread_id, turn_id, route_kind,
       selected_tools_json, tool_name, tool_call_source, returned_output_tokens,
       truncated_output_tokens, outcome, model_response_ordinal, dialog_locator_json
FROM tool_router_ledger
ORDER BY created_at_ms DESC
LIMIT 20;
```

- Tool-router remembered-tools recipe:

```sql
SELECT repo_key, task_key, tool_namespace, tool_name,
       datetime(created_at_ms / 1000, 'unixepoch') AS created,
       datetime(updated_at_ms / 1000, 'unixepoch') AS updated,
       request_count
FROM tool_router_remembered_tools
WHERE repo_key = '<repo-key>'
  AND task_key = '<task-key>'
  AND updated_at_ms >= (strftime('%s', 'now') * 1000) - 2592000000
ORDER BY updated_at_ms DESC, request_count DESC, tool_namespace, tool_name
LIMIT 8;
```

- `tool_namespace = ''` is the plain-tool sentinel; namespace-prefixed rows represent MCP or dynamic tool namespaces.

- Plugin activity recipe: there is no dedicated plugin activity table. Prove plugin influence by combining `thread_dynamic_tools`, `logs_2.sqlite`, rollout items, and plugin files under `CODEX_HOME/plugins`. `thread_dynamic_tools` shows tool schemas captured at thread start; logs can show plugin list/read/install/uninstall paths when tracing captured them.

```sql
SELECT thread_id, position, namespace, name, defer_loading, substr(description, 1, 120) AS description
FROM thread_dynamic_tools
WHERE thread_id = '<thread-id>'
ORDER BY position;
```

```sql
SELECT datetime(ts, 'unixepoch') AS ts, level, target, thread_id, substr(feedback_log_body, 1, 240) AS body
FROM logs
WHERE feedback_log_body LIKE '%plugin%'
ORDER BY ts DESC, ts_nanos DESC, id DESC
LIMIT 50;
```

- Startup RPC log recipe:

```sql
SELECT datetime(ts, 'unixepoch') AS ts, level, target, substr(feedback_log_body, 1, 300) AS body
FROM logs
WHERE feedback_log_body LIKE '%app-server typed request%'
ORDER BY ts DESC, ts_nanos DESC, id DESC
LIMIT 50;
```

- Memory SQL recipe:

```sql
SELECT t.id, t.memory_mode, t.title, so.usage_count, so.last_usage, substr(so.raw_memory, 1, 160) AS raw_memory
FROM threads t
LEFT JOIN stage1_outputs so ON so.thread_id = t.id
WHERE t.memory_mode != 'enabled' OR so.thread_id IS NOT NULL
ORDER BY t.updated_at_ms DESC
LIMIT 50;
```

- Job/debug SQL recipe:

```sql
SELECT kind, job_key, status, retry_remaining, last_error, input_watermark, last_success_watermark
FROM jobs
WHERE kind LIKE 'memory_%'
ORDER BY kind, job_key;
```

- Source entrypoints: `codex-rs/state/src/runtime.rs`, `codex-rs/state/src/runtime/threads.rs`, `codex-rs/state/src/runtime/logs.rs`, `codex-rs/state/src/runtime/memories.rs`, `codex-rs/state/src/runtime/model_router.rs`, `codex-rs/state/src/runtime/tool_router.rs`, `codex-rs/state/src/lib.rs`, TUI startup RPCs in `codex-rs/tui/src/app_server_session.rs`, startup timeout handling in `codex-rs/tui/src/app_server_session/startup_request_timeout.rs`, and app-server request logging in `codex-rs/app-server/src/message_processor.rs`.

## Token Usage Reporting

- Description: token usage drives `/status`, footer/title/status-line context labels, auto-compaction decisions, and user-facing usage-limit nudges.
- Configuration: `model_context_window`, `model_auto_compact_token_limit`, `tool_output_token_limit`, status-line items, and terminal-title items affect what is counted or shown.
- Tuning: prefer context remaining for ambient status, context used for cleanup decisions, and compact token formatting for narrow TUI surfaces.
- Debug recipe: inject `TokenUsageInfo`, compare `/status` output with footer state, and check app-server `ThreadTokenUsageUpdated` replay when usage diverges after resume.
- Source entrypoints: `codex-rs/tui/src/status/card.rs`, `codex-rs/tui/src/bottom_pane/footer.rs`, `codex-rs/tui/src/bottom_pane/status_line_setup.rs`, `codex-rs/tui/src/bottom_pane/title_setup.rs`, `codex-rs/tui/src/chatwidget.rs`, `codex-rs/app-server/src/codex_message_processor/token_usage_replay.rs`.
- Token impact: large tool outputs, broad skill/app/tool surfaces, memories, and long pasted logs raise context pressure. Prefer summaries, truncation, and targeted follow-up reads.

### Investigating Token Usage

- Start with `/status`. It shows the active model, total usage, last-turn/context usage, context window, and rate-limit state. The status line uses `last_token_usage` for context remaining/used and `total_token_usage` for total session usage.
- Follow live updates from provider response usage through `Session::update_token_usage_info`, `TokenUsageInfo::new_or_append`, app-server `handle_token_count_event`, `ThreadTokenUsageUpdated`, and TUI `ChatWidget::set_token_info`.
- For resume or attach bugs, inspect `token_usage_replay.rs`: it reads persisted `TokenCount` rollout items, maps them back to a v2 turn id, and sends a connection-scoped `ThreadTokenUsageUpdated` notification.
- For counts that jump after compact or history rebuild, inspect `Session::recompute_token_usage`, `context_manager::history`, and `TokenUsageInfo::full_context_window`. Compare `last_token_usage` versus `total_token_usage` before blaming the status renderer.
- For unexpected growth, audit model-visible surfaces added to the next turn: plugin capability summaries, skill instructions, selected memories, app/MCP schemas, tool output, images, and pasted logs. Use `/skills`, `/plugins`, `/apps`, `/mcp verbose`, `/memories`, and `/debug-config` to isolate which surface is active.
- For model-router accounting, separate production usage from router overhead. Shadow, canary, benchmark, self-assessment, judge, and verifier requests are router overhead and should not be counted as production savings.
- Useful focused tests live near `token_usage_update_refreshes_status_line_with_runtime_context_window`, `recompute_token_usage_uses_session_base_instructions`, `recompute_token_usage_updates_model_context_window`, and `token_usage_info_new_or_append_updates_context_window_when_provided`.

## Maintaining This Guide

- Update this file when user-facing behavior changes for plugins, skills, MCP/apps, memories, model router, tool router, slash commands, config/debug surfaces, or token usage reporting.
- Verify behavior from source before editing, keep recipes executable, and link to source entrypoints instead of copying large docs.
- If the bare `/codex` human guide path changes from `codex-rs/tui/codex_guide.md`, ensure the TUI Bazel target still includes it in `compile_data`.
