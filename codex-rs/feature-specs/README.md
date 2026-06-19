# Feature Specs

This directory contains internal feature descriptions intended to support AI-assisted test
generation and implementation review. These files are not user-facing product documentation.

Add one Markdown file per feature. A feature spec should be stable enough that a test generator can
derive behavioral test cases without reading the full implementation.

Recommended sections:

- Metadata: owner area, primary crates, and related code paths.
- Summary: the feature's purpose and user-visible contract.
- Default behavior: what happens without special configuration or override state.
- State model: durable, cached, and transient state that affects behavior.
- API surface: CLI, TUI/app-server, protocol, and library entry points.
- Invariants: behavior that must remain true across refactors.
- Test generation notes: positive, negative, edge, and regression cases.

Do not put public docs, release notes, or general usage guides here. Public documentation belongs in
the appropriate external documentation surface.
