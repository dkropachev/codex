# TUI Chat Composer

The chat composer is the bottom-pane state machine for text input, popup routing, paste handling,
and submit preparation. Keep this doc aligned with `codex-rs/tui/src/bottom_pane/chat_composer.rs`
and `codex-rs/tui/src/bottom_pane/paste_burst.rs`.

## What It Handles

- Plain text editing plus placeholder elements for attachments and large pastes.
- Popup routing for slash commands, mentions, and workflow aliases.
- Enter submission, Tab queueing while a task is running, and draft recovery after command dispatch.
- Burst detection for terminal paste streams when `disable_paste_burst` is not set.

## Workflow Aliases

- Workflows can register `workflow.yaml.command`.
- The alias appears in the slash popup as `/<cmd>` when workflows are enabled.
- The same alias is accepted by the shared workflow parser used by `codex workflow`, `codex <cmd>`, and `/<cmd>`.
- Built-in slash commands still win on name collisions.
- If `command` is omitted, workflows with simple ids that do not contain `/` fall back to an alias from the last id segment.

## Paste and IME Behavior

- Large pastes insert placeholders and keep the full content in `pending_pastes` until submit.
- If `disable_paste_burst` is toggled on, any held burst state is flushed or cleared so it cannot leak into later input.
- ASCII bursts briefly hold the first character for flicker suppression; non-ASCII input does not get that hold so IME typing is not dropped.

## Debugging

- Start with `codex-rs/tui/src/bottom_pane/chat_composer.rs` for state transitions and `codex-rs/tui/src/bottom_pane/command_popup.rs` for popup rendering.
- For slash dispatch, inspect `codex-rs/tui/src/chatwidget/slash_dispatch.rs`.
- For shared workflow alias parsing, inspect `codex-rs/workflows/src/command.rs` and `codex-rs/workflows/src/registry.rs`.

