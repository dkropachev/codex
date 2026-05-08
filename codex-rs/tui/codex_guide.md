# Codex Guide

This guide is a source map for user-facing Codex surfaces. Before changing an entry, verify the current behavior against the listed source files and keep recipes concrete.

## Fast Checks

- Use `/status` to inspect the active model, reasoning, context usage, rate limits, and instruction sources.
- Use `/debug-config` to inspect merged config, requirement sources, and config-layer provenance.
- Use `/mcp verbose`, `/skills`, `/plugins`, `/apps`, `/repo-ci`, `/implement`, and `/model-router` to inspect the matching feature from the TUI.
- Use `codex tool-router tune` and `codex model-router tune` for CLI-only internal routing diagnostics.
- When a guide section changes, update this file and the `/codex` config-mode snapshots in `codex-rs/tui/src/chatwidget/snapshots/`.

## Slash Commands

- Description: slash commands are parsed by the bottom-pane composer and dispatched by `ChatWidget` without involving the model unless the command explicitly submits a user turn or switches collaboration mode.
- Configuration: visibility is mostly feature-gated by `BuiltinCommandFlags`; side conversations and task-running state apply additional dispatch checks.
- Tuning: add names in `SlashCommand`, keep enum order intentional for popup ranking, add descriptions, and only mark `supports_inline_args` when arguments are parsed locally.
- Debug recipe: check `SlashCommand::from_str`, `builtins_for_input`, and `find_builtin_command`; then dispatch from a focused chatwidget test and assert history or app events.
- Source entrypoints: `codex-rs/tui/src/slash_command.rs`, `codex-rs/tui/src/bottom_pane/slash_commands.rs`, `codex-rs/tui/src/bottom_pane/chat_composer.rs`, `codex-rs/tui/src/chatwidget/slash_dispatch.rs`.
- Codex config mode: bare `/codex` switches the current thread into Codex config mode. The mode embeds this guide, generated slash-command registry context, and current `codex --help` output as developer instructions. Normal user messages in this mode run from a scratch workspace under the system temp directory; the target workspace is model-readable but must not be written. The model should use supported tools, app-server APIs, and CLI commands before direct config-file edits, ask for clarification when the target is ambiguous, and put `<codex_config_done>` on its own line when the TUI should ask whether to leave config mode. Users can leave with `/codex off`, `/codex disable`, or `/codex cancel`.
- One-shot config requests: `/codex <request>` submits a single AI-backed config turn with the same guide, slash-command registry context, and `codex --help` context. It does not enter persistent config mode; ambiguous targets should be resolved through the normal user-input request flow before writing config.
- Token impact: local commands like `/status` and `/debug-config` add no model context by themselves; bare `/codex` adds Codex config-mode developer instructions to subsequent turns, and commands such as `/codex <instruction>`, `/init`, `/compact`, `/review`, `/plan <prompt>`, and `/repo-ci <task>` can submit model-visible input.

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

- `model-router`: routes internal model calls for subagents and modules. User surfaces are `/model-router enable|disable|inherit`, `codex model-router tune`, and `codex model-router report show|apply`.
- `tool-router`: exposes one structured model-visible `tool_router` function that routes to internal tools. User-facing maintenance is mostly `codex tool-router tune` plus telemetry and state inspection.
- `repo-ci`: owns repo validation tools, shell-command guarding, and local/remote workflow learning. User surfaces are `/repo-ci`, `codex repo-ci`, and the `repo_ci.*` tools.
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
- `keybid-puls` (KeyBid Puls) [Productivity, app]: short-term rental ROI calculation.
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
- Configuration: use `[mcp_servers.<name>]`, MCP OAuth settings, `features.connectors`, and app enablement state. HTTP bearer tokens should use `bearer_token_env_var`.
- Tuning: prefer `codex-rs/codex-mcp/src/mcp_connection_manager.rs` for MCP tool and tool-call mutation; keep tool names stable and schemas narrow.
- Debug recipe: run `/mcp verbose` for server/tool/auth status, `/apps` for connector access, and inspect `FetchMcpInventory` or connector refresh events when the TUI differs from config.
- Source entrypoints: `codex-rs/codex-mcp/src/`, `codex-rs/core/src/config/mod.rs`, `codex-rs/core/src/apps/`, `codex-rs/core/src/context/apps_instructions.rs`, `codex-rs/tui/src/history_cell.rs`, `codex-rs/tui/src/chatwidget.rs`.
- Token impact: MCP tool schemas and app instructions increase the available tool surface. Use `tool_search` and app lazy-loading paths when a full tool list would be too large.

## Memories

- Description: memories summarize useful prior thread context and can inject memory usage instructions or generate new memory artifacts after idle threads.
- Configuration: `[memories]` supports `use_memories`, `generate_memories`, `disable_on_external_context`, rollout age/idle limits, and extraction/consolidation model overrides. `/memories` toggles use and generation from the TUI.
- Tuning: keep extraction and consolidation prompts focused, avoid generating memories from externally polluted context when that setting is enabled, and tune rollout limits before widening model prompts.
- Debug recipe: use `/memories`, inspect memory-related startup events and state DB rows, then check phase 1 extraction, storage, and phase 2 selection tests.
- Source entrypoints: `codex-rs/core/src/memories/`, `codex-rs/core/src/memories/README.md`, `codex-rs/state/src/runtime/memories.rs`, `codex-rs/tui/src/bottom_pane/memories_settings_view.rs`.
- Token impact: memory instructions and selected memories consume context. Prefer fewer high-signal memories over broad summaries.

## Repo CI

- Description: repo-ci is the internal validation subsystem. It learns repository commands, runs cached local checks, can push remote CI workflows, and guards duplicate shell CI commands.
- User surfaces: `/repo-ci setup|learn|retry`, `/repo-ci instruction show|set|clear|edit`, `/repo-ci <options> [task]`, `/codex <config-request>`, `codex repo-ci enable|learn|workflow`, `codex repo-ci instruction show|set|clear|edit`, and routed tools `repo_ci.status`, `repo_ci.learn`, `repo_ci.run`, `repo_ci.result`, and `repo_ci.instruction`.
- Configuration: `features.repo_ci` gates the feature. `[repo_ci.defaults]`, `[repo_ci.directories]`, `[repo_ci.github_repos]`, and `[repo_ci.github_orgs]` accept `enabled`, `automation`, `local_test_time_budget_sec`, `long_ci`, local/remote fix rounds, `learning_instruction`, `review_issue_types`, and legacy `max_review_fix_rounds` fallback values for implement. Legacy `learning_instructions` arrays are read and collapsed into the singular blob.
- Tuning: use slash-command session overrides for thread-local mode, issue types, legacy rounds, and long-CI behavior. Use `/codex` or `/codex <config-request>` as the generic AI-backed config path: Codex receives this guide, generated slash-command registry context, current `codex --help` output, and scratch-workspace guidance, then chooses the relevant module/API/CLI from runtime context. It should ask a clarifying question for ambiguous targets and complete the requested config change end to end. Use `codex repo-ci instruction set --cwd --instruction <text>` for direct non-interactive repo-ci learner-instruction writes. Use CLI learning when the manifest/runner is missing or stale, and prefer artifact IDs over pasted logs when handing failures back to the model.
- Debug recipe: start with `repo_ci.status`, then inspect the learned manifest, runner artifact, cache key, failing step ID, and compact `error_output`. For TUI issues, trace `/repo-ci` parsing in `slash_dispatch.rs` and app-server session config events.
- Source entrypoints: `codex-rs/repo-ci/src/`, `codex-rs/core/src/repo_ci_automation.rs`, `codex-rs/core/src/tools/handlers/repo_ci.rs`, `codex-rs/core/src/tools/ci_command_guard.rs`, `codex-rs/cli/src/repo_ci_learn.rs`, `codex-rs/cli/src/repo_ci_exec.rs`, `codex-rs/tui/src/chatwidget/slash_dispatch.rs`.
- Token impact: repo-ci logs can be large. Keep model input to brief failures, step IDs, and artifact IDs; request detailed artifacts only when the compact output is insufficient.

## Implement

- Description: implement runs targeted review/fix cycles after a regular agent turn changes files. It uses the repo-ci diff snapshot and issue-type selection, groups findings by owned file or module, and applies bounded worker fixes before the turn finishes.
- User surfaces: `/implement enable|disable|inherit|implicit --max-cycles=N` changes the current thread. `/implement [--max-cycles=N] <task>` submits one turn with implement review/fix forced on. `codex implement enable|disable|implicit --max-cycles=N` persists user config under `[implement]`.
- Configuration: `[implement] enabled = true|false` controls the loop independently from repo-ci checks. `[implement] mode = "auto"` runs after normal agent edits; `mode = "implicit"` runs only for `/implement <task>` turns. `[implement] max_cycles = N` sets the review/fix cycle budget. Legacy repo-ci `max_review_fix_rounds` values remain a fallback when implement settings are absent.
- Tuning: use `/implement disable` when validation should still run but review/fix should not. Use `/implement implicit --max-cycles=N` when review/fix should be opt-in per turn. Use `/implement enable --max-cycles=N` for automatic thread-local iteration budgets. Keep `review_issue_types` narrow for noisy repositories; `review_issue_types = []` disables review regardless of implement enablement.
- Debug recipe: trace `effective_config` in `repo_ci_automation.rs`, then inspect targeted review status events, subagent labels beginning with `repo_ci_fix_`, and `thread/repoCiSessionConfig/set` fields `implementEnabled`, `implementMode`, and `implementMaxCycles` for app-server clients.
- Source entrypoints: `codex-rs/core/src/repo_ci_automation.rs`, `codex-rs/config/src/config_toml.rs`, `codex-rs/cli/src/main.rs`, `codex-rs/tui/src/chatwidget/slash_dispatch.rs`, `codex-rs/app-server-protocol/src/protocol/v2.rs`.
- Token impact: review prompts include diff context and selected findings. Lower `max_cycles` or narrow `review_issue_types` when the loop consumes too much context.

## Model Router

- Description: model-router is the internal model-selection surface. It applies only to internal model calls, using a task key from `ModelRouterSource` plus prompt size, candidate metrics, pricing, context limits, failover exclusions, and state overlays.
- User surfaces: `/model-router enable|disable|inherit` changes only the current thread. `codex model-router tune` replays historical turns and can write recommendations; `codex model-router report show|apply` inspects or applies a saved report.
- Configuration: `[model_router] enabled = true` activates routing. `discovery`, `subscription_pricing`, and `savings_reference` control discovery and cost accounting. `[[model_router.candidates]]` can set `id`, `model`, `model_provider`, `service_tier`, `reasoning_effort`, `account_pool`, `account`, quality/success/latency metrics, and input/output/reasoning prices.
- Tuning: keep task-class inference, candidate metrics, pricing, account-pool behavior, and failover exclusions aligned. `codex model-router tune` defaults to a recent historical window and can run dry-run/report flows before applying metric overlays.
- Debug recipe: trace `ModelRouterSource::task_key`, the candidate set, filtered selectable routes, selected score/class, failover exclusion scope, state metric overlays, and app-server `thread/modelRouterSessionConfig/set` notifications.
- Source entrypoints: `codex-rs/model-router/src/lib.rs`, `codex-rs/core/src/model_router.rs`, `codex-rs/core/src/model_router/`, `codex-rs/core/src/model_router_tune.rs`, `codex-rs/state/src/runtime/model_router.rs`, `codex-rs/config/src/config_toml.rs`, `codex-rs/cli/src/main.rs`.
- Token impact: prompt byte estimates and effective context windows directly affect candidate eligibility. Router overhead from shadow, benchmark, judge, verifier, or self-assessment calls must be counted separately from production savings.

## Tool Router

- Description: tool-router is the internal structured-tool surface. The model calls one `tool_router` function with intent, target metadata, a domain, and an action; Codex then routes to shell, filesystem, git, repo-ci, MCP, app, image, agent, memory, config, or direct internal tools.
- User surfaces: there is no TUI slash command. `features.tool_router` controls model visibility, `codex tool-router tune` analyzes telemetry, and the raw `tool_router` call/result is the main runtime debugging surface.
- Configuration: the router schema requires `request`, `where.kind`, `targets`, and `action.kind`. `verbosity` can be `auto`, `brief`, `normal`, or `full`. Default guidance version is 2, schema version is 1, default guidance cap is 600 tokens, and the hard cap is 1200 tokens.
- Tuning: prefer exact `action.tool` or deterministic `action.kind`, typed targets, concrete payload keys, and `batch` for independent read-only reads. Dynamic guidance should stay small, sanitized, and keyed to repeated routing failures rather than request-specific paths.
- Debug recipe: inspect the raw JSON payload, selected tool, fallback tool, invalid route errors, outcome breakdowns, toolset hash, visible router-schema tokens, hidden tool-schema tokens, and persisted dynamic guidance.
- Source entrypoints: `codex-rs/tools/src/tool_router.rs`, `codex-rs/tools/src/tool_router_prompt.rs`, `codex-rs/tools/src/tool_discovery.rs`, `codex-rs/tools/src/tool_registry_plan.rs`, `codex-rs/core/src/tool_router_tune.rs`, `codex-rs/state/src/runtime/tool_router.rs`, `codex-rs/cli/src/main.rs`.
- Token impact: tool-router reduces prompt cost by hiding full tool schemas behind a compact router schema and catalog. Bad routing, verbose outputs, or over-broad actions can erase those savings.

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

- Tool-router SQL recipe:

```sql
SELECT datetime(created_at_ms / 1000, 'unixepoch') AS created, thread_id, turn_id, route_kind,
       selected_tools_json, net_tokens_saved, returned_output_tokens, truncated_output_tokens, outcome
FROM tool_router_ledger
ORDER BY created_at_ms DESC
LIMIT 20;
```

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

- Source entrypoints: `codex-rs/state/src/runtime.rs`, `codex-rs/state/src/runtime/threads.rs`, `codex-rs/state/src/runtime/logs.rs`, `codex-rs/state/src/runtime/memories.rs`, `codex-rs/state/src/runtime/model_router.rs`, `codex-rs/state/src/runtime/tool_router.rs`, `codex-rs/state/src/lib.rs`.

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

- Update this file when user-facing behavior changes for plugins, skills, MCP/apps, memories, repo-ci, model router, tool router, slash commands, config/debug surfaces, or token usage reporting.
- Verify behavior from source before editing, keep recipes executable, and link to source entrypoints instead of copying large docs.
- If the bare `/codex` human guide path changes from `codex-rs/tui/codex_guide.md`, ensure the TUI Bazel target still includes it in `compile_data`.
