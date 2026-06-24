#!/usr/bin/env python3

"""Verify feature specs and filename-derived test ownership.

The feature spec framework treats files in ``codex-rs/feature-specs`` as the
source of truth for user-facing behavior and expected test coverage. This
script validates that the markdown remains deterministic enough for humans,
CI, and future generation tools to reason about it.

The verifier enforces three related contracts:

* The README is the canonical feature index and test-place catalog.
* Each feature spec follows a strict heading schema and only links to concrete
  implementation entry points.
* Rust test ownership is derived from filenames such as
  ``account_pool__routing.rs`` rather than from markdown links. Test cases in a
  spec may reference concrete repo-relative test targets, and those targets
  must point at existing files and functions.

The verifier intentionally accumulates all errors before exiting so a single CI
run can report every schema issue that needs to be fixed.
"""

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class TestPlace:
    """Catalog record for one supported place where feature behavior is tested.

    ``name`` is the human-readable label for the test place. ``short_description``
    is the stable phrase embedded in feature-spec headings. ``long_description``
    is the README guidance that tells spec authors which behavior belongs in
    that test place. ``paths`` are repo-relative roots used to map Rust test
    files back to this test place.
    """

    name: str
    short_description: str
    long_description: str
    paths: tuple[str, ...]


ROOT = Path(__file__).resolve().parents[2]
FEATURE_SPECS_DIR = Path("codex-rs/feature-specs")
README_NAME = "README.md"
TEMPLATE_NAME = "TEMPLATE.md"
REQUIRED_HEADINGS = (
    "Summary",
    "Behavior",
    "Entry Points",
    "Subfeatures",
    "Invariants",
    "Test Places",
    "Test Generation Notes",
)
README_REQUIRED_HEADINGS = (
    "Test Places",
    "Feature Index",
)
SPEC_NAME_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*\.md$")
HEADING_RE = re.compile(r"^(#{1,6})\s+(.+?)\s*#*\s*$")
MARKDOWN_LINK_RE = re.compile(r"\[([^\]]+)\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
FEATURE_ID_FIELD_RE = re.compile(
    r"^\s*(?:[-*]\s*)?(?:#+\s*)?(?:\*\*)?Feature ID(?:\*\*)?\s*(?::|\|?\s*$)",
    re.IGNORECASE,
)
E2E_STEM_RE = re.compile(r"^[a-z0-9]+(?:_[a-z0-9]+)*__[a-z0-9]+(?:_[a-z0-9]+)*$")
TEST_REF_RE = re.compile(
    r"^(codex-rs/[^\s:]+\.rs):"
    r"([A-Za-z_][A-Za-z0-9_]*(?:\s*,\s*[A-Za-z_][A-Za-z0-9_]*)*)$"
)
MISSING_TARGET_RE = re.compile(r"^missing(?::([a-z0-9]+(?:-[a-z0-9]+)*))?$")
BEHAVIOR_ID_WORD_RE = re.compile(r"[A-Za-z0-9]+")
RUST_FUNCTION_LINE_RE = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+"
    r"([A-Za-z_][A-Za-z0-9_]*)\s*\("
)
TEST_PLACES = {
    "agent-e2e": TestPlace(
        name="Agent E2E",
        short_description="agent behavior under core integration tests",
        long_description=(
            "Place test cases here when feature behavior must be exercised through the "
            "core agent loop: model turns, tool calls, model-visible context, "
            "approvals, resume or compaction, and user-visible agent state transitions."
        ),
        paths=("codex-rs/core/tests/suite",),
    ),
    "app-server-api": TestPlace(
        name="App-Server API",
        short_description="app-server API behavior",
        long_description=(
            "Place test cases here when clients observe or control the feature through "
            "app-server requests, responses, notifications, WebSocket flows, or v2 "
            "protocol payloads."
        ),
        paths=("codex-rs/app-server/tests/suite",),
    ),
    "cli": TestPlace(
        name="Main CLI",
        short_description="main CLI command behavior",
        long_description=(
            "Place test cases here when the feature changes the top-level codex command "
            "surface, command parsing, command output, or user-visible CLI error behavior."
        ),
        paths=("codex-rs/cli/tests",),
    ),
    "tui-e2e": TestPlace(
        name="TUI E2E",
        short_description="full terminal TUI behavior",
        long_description=(
            "Place test cases here when the behavior needs a running terminal UI, "
            "keyboard input, popup completion, screen rendering, or terminal state across "
            "an interactive TUI session."
        ),
        paths=("codex-rs/tui/tests/suite",),
    ),
    "tui-component": TestPlace(
        name="TUI Component",
        short_description="focused TUI component behavior",
        long_description=(
            "Place test cases here when the behavior is local to TUI rendering or state, "
            "including component layout, selection state, popups, status surfaces, and "
            "component interactions that do not need a full terminal session."
        ),
        paths=("codex-rs/tui/src",),
    ),
    "login-auth": TestPlace(
        name="Login Auth",
        short_description="auth and login behavior",
        long_description=(
            "Place test cases here when the feature changes login, logout, token refresh, "
            "credential selection, account storage, cached auth semantics, or auth error "
            "handling."
        ),
        paths=("codex-rs/login/tests/suite",),
    ),
    "mcp-server": TestPlace(
        name="MCP Server",
        short_description="Codex-as-MCP-server behavior",
        long_description=(
            "Place test cases here when external MCP clients invoke Codex as an MCP "
            "server, depend on Codex MCP tool schemas, or consume MCP result and error "
            "shapes."
        ),
        paths=("codex-rs/mcp-server/tests/suite",),
    ),
    "rmcp-client": TestPlace(
        name="RMCP Client",
        short_description="MCP client transport and resource behavior",
        long_description=(
            "Place test cases here when Codex acts as an MCP client and the feature "
            "changes server startup, streamable HTTP, OAuth recovery, resource listing, "
            "tool discovery, or process cleanup behavior."
        ),
        paths=("codex-rs/rmcp-client/tests",),
    ),
    "codex-api": TestPlace(
        name="Codex API",
        short_description="Codex API client and protocol behavior",
        long_description=(
            "Place test cases here when the feature changes the lower-level Codex API "
            "client, SSE handling, realtime WebSocket protocol, request construction, or "
            "model API integration behavior."
        ),
        paths=("codex-rs/codex-api/tests",),
    ),
    "exec-cli": TestPlace(
        name="Exec CLI",
        short_description="codex exec CLI behavior",
        long_description=(
            "Place test cases here when the feature changes non-interactive codex exec "
            "semantics, exec-mode sandbox or approval handling, process behavior, or exec "
            "output and error reporting."
        ),
        paths=("codex-rs/exec/tests/suite",),
    ),
    "otel": TestPlace(
        name="Telemetry",
        short_description="telemetry and export behavior",
        long_description=(
            "Place test cases here when the feature changes telemetry spans, metrics, "
            "event attributes, export routing, runtime summaries, or OTLP behavior."
        ),
        paths=("codex-rs/otel/tests/suite",),
    ),
    "exec-server": TestPlace(
        name="Exec Server",
        short_description="exec-server service boundary behavior",
        long_description=(
            "Place test cases here when the feature changes exec-server process, "
            "filesystem, health, HTTP, relay, or WebSocket service-boundary behavior."
        ),
        paths=("codex-rs/exec-server/tests",),
    ),
}
TEST_PLACE_IDS = tuple(TEST_PLACES)
TEST_PLACE_HEADING_RE = re.compile(
    r"^([a-z0-9-]+)\s+\((.+)\)$",
)
SUBFEATURE_HEADINGS = (
    "Entry Points",
    "Invariants",
)
COVERED_TEST_PLACE_HEADINGS = (
    "Description",
    "Test cases",
)
NOT_COVERED_TEST_PLACE_HEADINGS = (
    "Description",
    "Status",
)
NOT_COVERED_STATUS = "Not covered"


@dataclass(frozen=True)
class MarkdownSection:
    """A markdown child section extracted from a larger section body.

    ``title`` is the heading text without leading ``#`` markers. ``body`` is the
    raw markdown between that heading and the next heading of the same or higher
    level.
    """

    title: str
    body: str


@dataclass(frozen=True)
class MarkdownLink:
    """A markdown link parsed from ``[text](target)`` syntax."""

    text: str
    target: str


@dataclass(frozen=True)
class TestCase:
    """A parsed bullet from a feature spec ``#### Test cases`` section.

    ``description`` is the textual behavior expectation before the final
    ``: `` delimiter. ``target`` is ``missing``, ``missing:<stable-id>``, or a
    concrete ``repo/path.rs:test_fn[,test_fn]`` target. ``line`` preserves the
    original bullet text for actionable error messages.
    """

    description: str
    target: str
    line: str


@dataclass(frozen=True, order=True)
class MappedTestTarget:
    """A Rust test function whose owning feature is derived from its filename.

    ``test_place`` is the catalog id for the directory containing the test.
    ``feature_id`` is derived from the file stem, for example
    ``account_pool__routing.rs`` maps to ``account-pool``. ``path`` is the
    repo-relative Rust file path and ``method`` is the discovered test function.
    """

    test_place: str
    feature_id: str
    path: str
    method: str


@dataclass(frozen=True, order=True)
class CoverageReportRow:
    """One deterministic row in the generated feature coverage report."""

    feature_id: str
    test_place: str
    status: str
    discovered_mapped_test_count: int
    concrete_declared_target_count: int
    missing_test_case_count: int
    declared_scenarios: tuple[str, ...] = ()
    missing_backlog_ids: tuple[str, ...] = ()


COVERAGE_STATUS_COVERED = "covered"
COVERAGE_STATUS_MISSING_BACKLOG = "missing-backlog"
COVERAGE_STATUS_NOT_COVERED = "not-covered"
COVERAGE_STATUS_PARTIAL = "partial"


def main() -> int:
    """Run the verifier from the command line and print all collected errors."""

    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--base",
        help="Git revision to diff against when enforcing changed e2e ownership.",
    )
    parser.add_argument(
        "--base-ref",
        help=(
            "Git ref to compute a merge-base against when --base is omitted. "
            "Useful for local PR-style checks."
        ),
    )
    parser.add_argument(
        "--head",
        default="HEAD",
        help="Git revision to diff to when --base is provided.",
    )
    parser.add_argument(
        "--changed-file",
        action="append",
        default=[],
        help="Changed file path to validate. May be passed more than once.",
    )
    parser.add_argument(
        "--coverage-report",
        action="store_true",
        help="Print a deterministic feature coverage report after validation succeeds.",
    )
    parser.add_argument(
        "--include-working-tree",
        action="store_true",
        help="Include staged, unstaged, and untracked paths in changed-file checks.",
    )
    args = parser.parse_args()

    changed_files = list(args.changed_file)
    base = args.base
    if base is None and args.base_ref:
        base = merge_base(ROOT, args.base_ref, args.head)
    if base:
        changed_files.extend(changed_files_from_git(ROOT, base, args.head))
    if args.include_working_tree:
        changed_files.extend(changed_files_from_working_tree(ROOT))

    failures = verify_feature_specs(ROOT, changed_files=changed_files, base=base)
    if not failures:
        if args.coverage_report:
            print(format_coverage_report(feature_coverage_report(ROOT)), end="")
        return 0

    print("Feature specs must be indexed, deterministic, and linked to test ownership.")
    print()
    for failure in failures:
        print(f"- {failure}")
    return 1


def verify_feature_specs(
    root: Path, *, changed_files: list[str], base: str | None = None
) -> list[str]:
    """Return every feature-spec verification failure under ``root``.

    ``changed_files`` should contain repo-relative paths from either explicit
    ``--changed-file`` arguments or a git diff. Changed mapped test files are
    checked against their owning feature specs. ``base`` is currently accepted
    for CLI compatibility with earlier coverage checks but is not used.
    """

    _ = base
    failures: list[str] = []
    failures.extend(readme_failures(root))

    spec_files = feature_spec_files(root)
    for spec in spec_files:
        failures.extend(spec_failures(root, spec))

    discovered_targets = discovered_mapped_test_targets(root)
    failures.extend(not_covered_mapped_test_failures(spec_files, discovered_targets))
    failures.extend(unlisted_mapped_test_failures(root, spec_files, discovered_targets))
    failures.extend(missing_behavior_scenario_failures(root, spec_files))
    failures.extend(changed_e2e_failures(root, changed_files))
    return failures


def readme_failures(root: Path) -> list[str]:
    """Validate the feature-spec README index and test-place catalog.

    The README is required to list every feature spec exactly once, in sorted
    order, using link text that matches the feature id. It also owns the
    complete list of supported test places and their human-readable
    descriptions.
    """

    feature_dir = root / FEATURE_SPECS_DIR
    readme_path = feature_dir / README_NAME
    failures: list[str] = []
    if not readme_path.exists():
        return [f"{relative_path(root, readme_path)} is missing"]

    spec_files = feature_spec_files(root)
    expected_targets = {spec.name for spec in spec_files}
    readme_text = readme_path.read_text(encoding="utf-8")
    failures.extend(
        top_level_schema_failures(
            relative_path(root, readme_path),
            top_level_heading_titles(readme_text),
            README_REQUIRED_HEADINGS,
        )
    )
    failures.extend(readme_test_place_failures(root, readme_path, readme_text))

    links = markdown_links(readme_text)
    listed_targets: list[str] = []

    for link in links:
        target_path = resolve_link_target(root, readme_path, link.target)
        if target_path is None:
            failures.append(
                f"{relative_path(root, readme_path)} links to invalid target `{link.target}`"
            )
            continue

        if target_path.parent != feature_dir or target_path.name not in expected_targets:
            failures.append(
                f"{relative_path(root, readme_path)} should only link indexed feature specs "
                f"(found `{link.target}`)"
            )
            continue

        listed_targets.append(target_path.name)
        expected_text = target_path.stem
        if link.text != expected_text:
            failures.append(
                f"{relative_path(root, readme_path)} link text for `{target_path.name}` "
                f"must be `{expected_text}`"
            )

    duplicate_targets = sorted(
        {target for target in listed_targets if listed_targets.count(target) > 1}
    )
    for target in duplicate_targets:
        failures.append(f"{relative_path(root, readme_path)} lists `{target}` more than once")

    listed_set = set(listed_targets)
    for missing in sorted(expected_targets - listed_set):
        failures.append(f"{relative_path(root, readme_path)} is missing `{missing}`")
    for extra in sorted(listed_set - expected_targets):
        failures.append(f"{relative_path(root, readme_path)} lists unknown spec `{extra}`")

    if listed_targets != sorted(listed_targets):
        failures.append(f"{relative_path(root, readme_path)} feature links must be sorted")

    return failures


def spec_failures(root: Path, spec: Path) -> list[str]:
    """Validate one feature spec file against the framework schema.

    This checks the filename-derived feature id, required top-level headings,
    forbidden ``Feature ID`` metadata, forbidden test markdown links, entry
    point links, subfeature shape, and per-test-place coverage declarations.
    """

    failures: list[str] = []
    rel_spec = relative_path(root, spec)
    if not SPEC_NAME_RE.fullmatch(spec.name):
        failures.append(f"{rel_spec} filename must be kebab-case")

    text = spec.read_text(encoding="utf-8")
    failures.extend(
        top_level_schema_failures(
            rel_spec,
            top_level_heading_titles(text),
            REQUIRED_HEADINGS,
        )
    )

    for line_number, line in enumerate(text.splitlines(), start=1):
        if FEATURE_ID_FIELD_RE.match(line):
            failures.append(f"{rel_spec}:{line_number} must not define a Feature ID field")
        for link in markdown_links(line):
            if is_test_path_reference(
                root, spec, link.text
            ) or is_test_path_reference(root, spec, link.target):
                failures.append(
                    f"{rel_spec}:{line_number} must not include test links; "
                    "test ownership is derived from filenames"
                )

    for line_number, hashes, title in heading_entries(text):
        if title == "E2E Coverage":
            failures.append(
                f"{rel_spec}:{line_number} must not include `{hashes} E2E Coverage`; "
                "use `## Test Places`"
            )

    for section in sections_named(text, "Entry Points"):
        links = markdown_links(section)
        if not links:
            failures.append(f"{rel_spec} has an Entry Points section without links")
        for link in links:
            target_path = resolve_link_target(root, spec, link.target)
            if target_path is None or not target_path.exists():
                failures.append(
                    f"{rel_spec} Entry Points link `{link.target}` does not resolve "
                    "inside the repository"
                )

    failures.extend(subfeature_failures(root, spec, text))
    failures.extend(test_place_failures(root, spec, text))
    return failures


def top_level_schema_failures(
    rel_path: str,
    headings: list[str],
    expected_headings: tuple[str, ...],
) -> list[str]:
    """Validate exact top-level heading presence, uniqueness, and order.

    Extra ``##`` headings are rejected because feature specs must be
    mechanically parseable. Ordering is checked only after all required headings
    exist exactly once, which avoids noisy secondary ordering errors.
    """

    failures: list[str] = []
    for heading in expected_headings:
        count = headings.count(heading)
        if count == 0:
            failures.append(f"{rel_path} is missing `## {heading}`")
        elif count > 1:
            failures.append(f"{rel_path} lists `## {heading}` more than once")

    expected_set = set(expected_headings)
    for heading in headings:
        if heading not in expected_set:
            failures.append(f"{rel_path} contains unexpected `## {heading}`")

    if all(headings.count(heading) == 1 for heading in expected_headings) and set(
        headings
    ) == expected_set and headings != list(expected_headings):
        expected = "`, `".join(f"## {heading}" for heading in expected_headings)
        failures.append(f"{rel_path} top-level headings must be ordered as `{expected}`")

    return failures


def readme_test_place_failures(root: Path, readme_path: Path, text: str) -> list[str]:
    """Validate the README ``## Test Places`` catalog.

    Each test place must be a ``### test-place (description)`` heading with
    ``#### Name``, ``#### Short Description``, and ``#### Description`` child
    sections. All three values are checked against the in-script catalog so
    feature specs can use stable heading text and README readers get enough
    guidance to choose the right test place.
    """

    rel_readme = relative_path(root, readme_path)
    failures: list[str] = []
    sections = sections_named(text, "Test Places")
    if not sections:
        return failures
    if len(sections) > 1:
        failures.append(f"{rel_readme} lists `## Test Places` more than once")

    failures.extend(
        no_text_before_child_heading_failures(
            rel_readme,
            "Test Places",
            sections[0],
            3,
        )
    )
    place_sections = child_heading_sections(sections[0], 3)
    failures.extend(
        ordered_test_place_failures(
            rel_readme,
            place_sections,
            "README Test Places",
        )
    )

    for section in place_sections:
        heading = TEST_PLACE_HEADING_RE.fullmatch(section.title)
        if heading is None:
            failures.append(
                f"{rel_readme} README Test Places heading `### {section.title}` must use "
                "`### test-place (description)`"
            )
            continue
        test_place = heading.group(1)
        heading_description = heading.group(2)
        if test_place not in TEST_PLACE_IDS:
            failures.append(
                f"{rel_readme} README Test Places lists unknown test place `{test_place}` "
                f"(expected one of {', '.join(TEST_PLACE_IDS)})"
            )
        else:
            record = TEST_PLACES[test_place]
            expected_description = record.short_description
            if heading_description != expected_description:
                failures.append(
                    f"{rel_readme} README test place `{test_place}` heading description "
                    f"must be `{expected_description}`"
                )
        child_titles = [child.title for child in child_heading_sections(section.body, 4)]
        failures.extend(
            child_heading_schema_failures(
                rel_readme,
                f"README test place `{test_place}`",
                child_titles,
                ("Name", "Short Description", "Description", "Path Ownership Rules"),
            )
        )
        if test_place in TEST_PLACE_IDS:
            failures.extend(
                readme_test_place_record_failures(
                    rel_readme,
                    test_place,
                    section.body,
                    TEST_PLACES[test_place],
                )
            )

    listed_places = {
        heading.group(1)
        for section in place_sections
        if (heading := TEST_PLACE_HEADING_RE.fullmatch(section.title)) is not None
    }
    for missing in sorted(set(TEST_PLACE_IDS) - listed_places):
        failures.append(f"{rel_readme} README Test Places is missing `{missing}`")

    return failures


def readme_test_place_record_failures(
    rel_readme: str,
    test_place: str,
    markdown: str,
    record: TestPlace,
) -> list[str]:
    """Validate README catalog field values for one known test place."""

    failures: list[str] = []
    expected_fields = (
        ("Name", record.name),
        ("Short Description", record.short_description),
        ("Description", record.long_description),
    )
    for heading, expected in expected_fields:
        sections = sections_named(markdown, heading)
        if not sections:
            continue
        actual = normalized_markdown_text(sections[0])
        if actual != expected:
            failures.append(
                f"{rel_readme} README test place `{test_place}` `{heading}` must be "
                f"`{expected}`"
            )

    path_sections = sections_named(markdown, "Path Ownership Rules")
    if path_sections:
        expected_paths = [f"- `{path}`" for path in record.paths]
        actual_paths = nonempty_lines(path_sections[0])
        if actual_paths != expected_paths:
            expected = "`, `".join(record.paths)
            failures.append(
                f"{rel_readme} README test place `{test_place}` "
                f"`Path Ownership Rules` must list `{expected}`"
            )
    return failures


def subfeature_failures(root: Path, spec: Path, text: str) -> list[str]:
    """Validate the optional ``## Subfeatures`` section of a feature spec.

    A feature may declare ``None.`` or one or more ``###`` subfeatures.
    Subfeatures are intentionally local to the same file in this first version
    of the framework and may only include entry points and invariants.
    """

    rel_spec = relative_path(root, spec)
    failures: list[str] = []
    sections = sections_named(text, "Subfeatures")
    if not sections:
        return failures

    subfeatures = child_heading_sections(sections[0], 3)
    if not subfeatures:
        if nonempty_lines(sections[0]) != ["None."]:
            failures.append(
                f"{rel_spec} Subfeatures must list subfeatures or contain only `None.`"
            )
        return failures

    failures.extend(
        no_text_before_child_heading_failures(
            rel_spec,
            "Subfeatures",
            sections[0],
            3,
        )
    )

    for section in subfeatures:
        child_titles = [child.title for child in child_heading_sections(section.body, 4)]
        failures.extend(
            child_heading_schema_failures(
                rel_spec,
                f"subfeature `{section.title}`",
                child_titles,
                SUBFEATURE_HEADINGS,
            )
        )

        invariants = sections_named(section.body, "Invariants")
        if invariants and not invariants[0].strip():
            failures.append(f"{rel_spec} subfeature `{section.title}` must include invariants")

    return failures


def test_place_failures(root: Path, spec: Path, text: str) -> list[str]:
    """Validate all per-feature ``## Test Places`` entries.

    Every cataloged test place must appear exactly once. Covered places must use
    ``#### Description`` plus ``#### Test cases``. Non-applicable places must use
    only ``#### Description`` plus ``#### Status`` set to ``Not covered`` so the
    absence of test coverage is explicit and explained.
    """

    rel_spec = relative_path(root, spec)
    failures: list[str] = []
    test_places_sections = sections_named(text, "Test Places")
    if not test_places_sections:
        return failures
    failures.extend(
        no_text_before_child_heading_failures(
            rel_spec,
            "Test Places",
            test_places_sections[0],
            3,
        )
    )

    place_sections = child_heading_sections(test_places_sections[0], 3)
    if not place_sections:
        failures.append(f"{rel_spec} Test Places section must list test places")
        for missing in TEST_PLACE_IDS:
            failures.append(f"{rel_spec} Test Places is missing `{missing}`")
        return failures
    failures.extend(
        ordered_test_place_failures(
            rel_spec,
            place_sections,
            "Test Places",
        )
    )

    listed_places: set[str] = set()
    for section in place_sections:
        heading = TEST_PLACE_HEADING_RE.fullmatch(section.title)
        if heading is None:
            failures.append(
                f"{rel_spec} Test Places heading `### {section.title}` must use "
                "`### test-place (description)`"
            )
            continue

        test_place = heading.group(1)
        heading_description = heading.group(2)
        known_test_place = test_place in TEST_PLACE_IDS
        if test_place in listed_places:
            failures.append(f"{rel_spec} lists test place `{test_place}` more than once")
        if known_test_place:
            listed_places.add(test_place)

        if not known_test_place:
            failures.append(
                f"{rel_spec} lists unknown test place `{test_place}` "
                f"(expected one of {', '.join(TEST_PLACE_IDS)})"
            )
        else:
            expected_description = TEST_PLACES[test_place].short_description
            if heading_description != expected_description:
                failures.append(
                    f"{rel_spec} test place `{test_place}` heading description must be "
                    f"`{expected_description}`"
                )

        child_titles = [child.title for child in child_heading_sections(section.body, 4)]
        description_sections = sections_named(section.body, "Description")
        if not description_sections or not description_sections[0].strip():
            failures.append(f"{rel_spec} test place `{test_place}` must include a description")
        else:
            description = description_sections[0]
            if markdown_links(description) or "codex-rs/" in description:
                failures.append(
                    f"{rel_spec} test place `{test_place}` description must describe behavior "
                    "without file references"
                )

        test_case_sections = sections_named(section.body, "Test cases")
        current_sections = sections_named(section.body, "Current test coverage")
        missing_sections = sections_named(section.body, "Missing coverage ideas")
        status_sections = sections_named(section.body, "Status")
        if len(status_sections) > 1:
            failures.append(f"{rel_spec} test place `{test_place}` lists Status more than once")
        if status_sections:
            failures.extend(
                child_heading_schema_failures(
                    rel_spec,
                    f"test place `{test_place}`",
                    child_titles,
                    NOT_COVERED_TEST_PLACE_HEADINGS,
                )
            )
            status_lines = nonempty_lines(status_sections[0])
            if status_lines != [NOT_COVERED_STATUS]:
                failures.append(
                    f"{rel_spec} test place `{test_place}` Status must be "
                    f"`{NOT_COVERED_STATUS}`"
                )
            if current_sections:
                failures.append(
                    f"{rel_spec} test place `{test_place}` with Status "
                    f"`{NOT_COVERED_STATUS}` must not include "
                    "`#### Current test coverage`"
                )
            if test_case_sections:
                failures.append(
                    f"{rel_spec} test place `{test_place}` with Status "
                    f"`{NOT_COVERED_STATUS}` must not include "
                    "`#### Test cases`"
                )
            if missing_sections:
                failures.append(
                    f"{rel_spec} test place `{test_place}` with Status "
                    f"`{NOT_COVERED_STATUS}` must not include "
                    "`#### Missing coverage ideas`"
                )
            allowed_headings = {"Description", "Status"}
            for child_section in child_heading_sections(section.body, 4):
                if child_section.title not in allowed_headings:
                    failures.append(
                        f"{rel_spec} test place `{test_place}` with Status "
                        f"`{NOT_COVERED_STATUS}` must only include Description and Status"
                    )
            continue

        failures.extend(
            child_heading_schema_failures(
                rel_spec,
                f"test place `{test_place}`",
                child_titles,
                COVERED_TEST_PLACE_HEADINGS,
            )
        )
        if not test_case_sections:
            failures.append(
                f"{rel_spec} test place `{test_place}` must include `#### Test cases`"
            )
            continue

        failures.extend(
            test_case_failures(
                root,
                rel_spec,
                spec.stem,
                test_place,
                test_case_sections[0],
            )
        )

    for missing in sorted(set(TEST_PLACE_IDS) - listed_places):
        failures.append(f"{rel_spec} Test Places is missing `{missing}`")

    return failures


def nonempty_lines(markdown: str) -> list[str]:
    """Return stripped nonblank lines from a markdown fragment."""

    return [line.strip() for line in markdown.splitlines() if line.strip()]


def normalized_markdown_text(markdown: str) -> str:
    """Return a markdown fragment as one whitespace-normalized text value."""

    return " ".join(nonempty_lines(markdown))


def test_case_failures(
    root: Path,
    rel_spec: str,
    feature_id: str,
    test_place: str,
    markdown: str,
) -> list[str]:
    """Validate all bullets in one ``#### Test cases`` section.

    Each bullet must describe the expected behavior and end in ``missing``,
    ``missing:<stable-id>``, or a concrete Rust test target. Concrete targets
    are delegated to ``test_target_failures`` for path, filename, and method
    validation.
    """

    failures: list[str] = []
    test_cases = parse_test_cases(markdown)
    if not test_cases:
        return [f"{rel_spec} test place `{test_place}` Test cases must list test cases"]

    valid_backlog_or_target_count = 0
    missing_ids: set[str] = set()
    for test_case in test_cases:
        if not test_case.description:
            failures.append(
                f"{rel_spec} test place `{test_place}` test case `{test_case.line}` "
                "must describe expected behavior before the target"
            )

        if is_missing_target(test_case.target):
            valid_backlog_or_target_count += 1
            missing_id = missing_backlog_id_from_target(test_case.target)
            if missing_id is None:
                continue
            if missing_id in missing_ids:
                failures.append(
                    f"{rel_spec} test place `{test_place}` missing backlog id "
                    f"`{missing_id}` is used more than once"
                )
            else:
                missing_ids.add(missing_id)
            continue

        target = TEST_REF_RE.fullmatch(test_case.target)
        if target is None:
            failures.append(
                f"{rel_spec} test place `{test_place}` test case `{test_case.line}` "
                "must target `repo/path.rs:test_name[,test_name]`, `missing`, or "
                "`missing:kebab-case-id`"
            )
            continue

        test_path = target.group(1)
        methods = [method.strip() for method in target.group(2).split(",")]
        valid_backlog_or_target_count += len(methods)
        failures.extend(
            test_target_failures(
                root,
                rel_spec,
                feature_id,
                test_place,
                test_path,
                methods,
            )
        )

    if valid_backlog_or_target_count == 0:
        failures.append(
            f"{rel_spec} test place `{test_place}` Test cases must include at least "
            "one concrete target or `missing` backlog item"
        )

    return failures


def parse_test_cases(markdown: str) -> list[TestCase]:
    """Parse markdown bullets into structured test-case declarations.

    The parser keeps malformed lines as ``TestCase`` objects with empty
    descriptions or targets instead of dropping them. That lets validation
    report precise errors while still continuing through the rest of the file.
    """

    test_cases: list[TestCase] = []
    for line in markdown.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if not stripped.startswith("- "):
            test_cases.append(TestCase("", "", stripped))
            continue

        body = stripped.removeprefix("- ").strip()
        description, separator, target = body.rpartition(": ")
        if not separator:
            test_cases.append(TestCase("", body, stripped))
            continue
        test_cases.append(TestCase(description.strip(), target.strip(), stripped))
    return test_cases


def declared_mapped_test_targets(root: Path, spec_files: list[Path]) -> set[MappedTestTarget]:
    """Return concrete test targets declared by feature specs.

    Feature-prefixed targets must map back to the declaring spec. Legacy or mixed
    files without a feature-prefixed name are mapped to the declaring spec as an
    explicit fallback. Invalid declarations are already reported by
    ``test_case_failures`` and are excluded here so they cannot mask genuinely
    unlisted discovered tests or inflate coverage-report counts.
    """

    targets: set[MappedTestTarget] = set()
    for spec in spec_files:
        text = spec.read_text(encoding="utf-8")
        for section in test_place_sections(text):
            heading = TEST_PLACE_HEADING_RE.fullmatch(section.title)
            if heading is None:
                continue

            test_place = heading.group(1)
            if test_place not in TEST_PLACE_IDS:
                continue

            test_case_sections = sections_named(section.body, "Test cases")
            if len(test_case_sections) != 1:
                continue

            for test_case in parse_test_cases(test_case_sections[0]):
                target = TEST_REF_RE.fullmatch(test_case.target)
                if target is None:
                    continue

                test_path = normalize_changed_path(target.group(1))
                target_feature_id = feature_id_from_test_path(test_path)
                if target_feature_id is not None and target_feature_id != spec.stem:
                    continue
                if not is_test_place_path(test_place, test_path):
                    continue

                full_path = root / test_path
                if not full_path.exists():
                    continue

                test_methods = rust_test_function_names(full_path)
                for method in target.group(2).split(","):
                    method = method.strip()
                    if method in test_methods:
                        targets.add(
                            MappedTestTarget(
                                test_place=test_place,
                                feature_id=spec.stem,
                                path=test_path,
                                method=method,
                            )
                        )

    return targets


def discovered_mapped_test_targets(root: Path) -> list[MappedTestTarget]:
    """Discover Rust test functions in feature-prefixed test files.

    The scan is limited to the repo-relative roots owned by ``TEST_PLACES``.
    Files whose stem does not use ``feature_name__scenario.rs`` are treated as
    legacy tests and skipped. Within mapped files, only functions annotated with
    Rust test attributes are returned so helper functions do not become required
    feature-spec targets.
    """

    targets: list[MappedTestTarget] = []
    seen_paths: set[str] = set()
    for test_place in TEST_PLACE_IDS:
        for test_root in TEST_PLACES[test_place].paths:
            full_test_root = root / test_root
            if not full_test_root.exists():
                continue

            for path in sorted(full_test_root.rglob("*.rs")):
                rel_path = relative_path(root, path)
                if rel_path in seen_paths:
                    continue
                seen_paths.add(rel_path)

                if not is_test_place_path(test_place, rel_path):
                    continue

                feature_id = feature_id_from_test_path(rel_path)
                if feature_id is None:
                    continue

                for method in sorted(rust_test_function_names(path)):
                    targets.append(
                        MappedTestTarget(
                            test_place=test_place,
                            feature_id=feature_id,
                            path=rel_path,
                            method=method,
                        )
                    )

    return sorted(targets)


def not_covered_mapped_test_failures(
    spec_files: list[Path],
    discovered_targets: list[MappedTestTarget],
) -> list[str]:
    """Reject ``Status: Not covered`` when mapped tests already exist.

    A feature/test-place pair can only be marked not covered when discovery finds
    no mapped Rust tests for that pair. Once a mapped test exists, the feature
    spec must switch that test place to ``Test cases`` and describe the covered
    behavior there.
    """

    not_covered = not_covered_test_places(spec_files)
    failures: list[str] = []
    for target in discovered_targets:
        if (target.feature_id, target.test_place) not in not_covered:
            continue

        spec_rel = (FEATURE_SPECS_DIR / f"{target.feature_id}.md").as_posix()
        failures.append(
            f"{spec_rel} test place `{target.test_place}` is `Not covered` but "
            f"discovered mapped test `{target.path}:{target.method}`"
        )

    return failures


def not_covered_test_places(spec_files: list[Path]) -> set[tuple[str, str]]:
    """Return feature/test-place pairs explicitly marked ``Not covered``."""

    not_covered: set[tuple[str, str]] = set()
    for spec in spec_files:
        text = spec.read_text(encoding="utf-8")
        for section in test_place_sections(text):
            heading = TEST_PLACE_HEADING_RE.fullmatch(section.title)
            if heading is None:
                continue

            test_place = heading.group(1)
            if test_place not in TEST_PLACE_IDS:
                continue

            status_sections = sections_named(section.body, "Status")
            if status_sections and nonempty_lines(status_sections[0]) == [
                NOT_COVERED_STATUS
            ]:
                not_covered.add((spec.stem, test_place))

    return not_covered


def unlisted_mapped_test_failures(
    root: Path,
    spec_files: list[Path],
    discovered_targets: list[MappedTestTarget],
) -> list[str]:
    """Report discovered mapped tests absent from feature-spec ``Test cases``.

    The filename convention creates ownership, but the feature spec is still the
    reviewable source of truth for expected coverage. Every discovered mapped
    Rust test function must therefore appear as a concrete ``Test cases`` target
    in the owning feature spec.
    """

    declared_targets = declared_mapped_test_targets(root, spec_files)
    feature_ids = {spec.stem for spec in spec_files}
    failures: list[str] = []

    for target in discovered_targets:
        spec_rel = (FEATURE_SPECS_DIR / f"{target.feature_id}.md").as_posix()
        if target.feature_id not in feature_ids:
            failures.append(
                f"{target.path}:{target.method} maps to missing feature spec `{spec_rel}`"
            )
            continue

        if target not in declared_targets:
            failures.append(
                f"{target.path}:{target.method} maps to feature `{target.feature_id}` "
                f"and test place `{target.test_place}` but is not listed in "
                f"`{spec_rel}` Test cases"
            )

    return failures


def missing_behavior_scenario_failures(root: Path, spec_files: list[Path]) -> list[str]:
    """Reject ``missing`` items that duplicate an already-declared scenario id."""

    declared_scenarios = mapped_test_scenarios(declared_mapped_test_targets(root, spec_files))
    failures: list[str] = []
    for spec in spec_files:
        rel_spec = relative_path(root, spec)
        text = spec.read_text(encoding="utf-8")
        for section in test_place_sections(text):
            heading = TEST_PLACE_HEADING_RE.fullmatch(section.title)
            if heading is None:
                continue

            test_place = heading.group(1)
            if test_place not in TEST_PLACE_IDS:
                continue

            scenarios = set(declared_scenarios.get((spec.stem, test_place), ()))
            if not scenarios:
                continue

            test_case_sections = sections_named(section.body, "Test cases")
            if len(test_case_sections) != 1:
                continue

            for test_case in parse_test_cases(test_case_sections[0]):
                if not is_missing_target(test_case.target):
                    continue
                behavior_id = scenario_id_from_behavior_description(
                    test_case.description
                )
                if behavior_id not in scenarios:
                    continue
                failures.append(
                    f"{rel_spec} test place `{test_place}` missing test case "
                    f"`{test_case.line}` matches declared scenario `{behavior_id}`; "
                    "replace `missing` with a concrete test target or rename the "
                    "missing behavior"
                )

    return failures


def feature_coverage_report(root: Path) -> list[CoverageReportRow]:
    """Return deterministic coverage rows for every feature/test-place pair.

    ``missing`` test cases are counted as backlog items. A covered test place
    with only missing backlog is reported as ``missing-backlog``; one with both
    concrete targets and backlog is reported as ``partial``.
    """

    spec_files = feature_spec_files(root)
    discovered_counts = mapped_test_counts(discovered_mapped_test_targets(root))
    declared_targets = declared_mapped_test_targets(root, spec_files)
    concrete_counts = mapped_test_counts(declared_targets)
    declared_scenarios = mapped_test_scenarios(declared_targets)
    missing_id_values = missing_backlog_ids(spec_files)
    missing_counts = missing_test_case_counts(spec_files)
    not_covered = not_covered_test_places(spec_files)
    rows: list[CoverageReportRow] = []

    for spec in spec_files:
        feature_id = spec.stem
        for test_place in TEST_PLACE_IDS:
            key = (feature_id, test_place)
            concrete_count = concrete_counts.get(key, 0)
            missing_count = missing_counts.get(key, 0)
            rows.append(
                CoverageReportRow(
                    feature_id=feature_id,
                    test_place=test_place,
                    status=coverage_status(
                        key in not_covered,
                        concrete_count,
                        missing_count,
                    ),
                    discovered_mapped_test_count=discovered_counts.get(key, 0),
                    concrete_declared_target_count=concrete_count,
                    missing_test_case_count=missing_count,
                    declared_scenarios=declared_scenarios.get(key, ()),
                    missing_backlog_ids=missing_id_values.get(key, ()),
                )
            )

    return rows


def mapped_test_counts(
    targets: list[MappedTestTarget] | set[MappedTestTarget],
) -> dict[tuple[str, str], int]:
    """Count mapped Rust test functions by feature and test place."""

    counts: dict[tuple[str, str], int] = {}
    for target in targets:
        key = (target.feature_id, target.test_place)
        counts[key] = counts.get(key, 0) + 1
    return counts


def mapped_test_scenarios(
    targets: list[MappedTestTarget] | set[MappedTestTarget],
) -> dict[tuple[str, str], tuple[str, ...]]:
    """Return normalized scenario ids from feature-prefixed test filenames."""

    scenarios: dict[tuple[str, str], set[str]] = {}
    for target in targets:
        scenario_id = scenario_id_from_test_path(target.path)
        if scenario_id is None:
            continue
        key = (target.feature_id, target.test_place)
        scenarios.setdefault(key, set()).add(scenario_id)
    return {key: tuple(sorted(values)) for key, values in scenarios.items()}


def missing_test_case_counts(spec_files: list[Path]) -> dict[tuple[str, str], int]:
    """Count ``missing`` backlog entries by feature and test place."""

    counts: dict[tuple[str, str], int] = {}
    for spec in spec_files:
        text = spec.read_text(encoding="utf-8")
        for section in test_place_sections(text):
            heading = TEST_PLACE_HEADING_RE.fullmatch(section.title)
            if heading is None:
                continue

            test_place = heading.group(1)
            if test_place not in TEST_PLACE_IDS:
                continue

            test_case_sections = sections_named(section.body, "Test cases")
            if len(test_case_sections) != 1:
                continue

            missing_count = sum(
                1
                for test_case in parse_test_cases(test_case_sections[0])
                if is_missing_target(test_case.target)
            )
            if missing_count:
                counts[(spec.stem, test_place)] = missing_count

    return counts


def missing_backlog_ids(spec_files: list[Path]) -> dict[tuple[str, str], tuple[str, ...]]:
    """Return declared stable ids for missing backlog entries."""

    ids: dict[tuple[str, str], set[str]] = {}
    for spec in spec_files:
        text = spec.read_text(encoding="utf-8")
        for section in test_place_sections(text):
            heading = TEST_PLACE_HEADING_RE.fullmatch(section.title)
            if heading is None:
                continue

            test_place = heading.group(1)
            if test_place not in TEST_PLACE_IDS:
                continue

            test_case_sections = sections_named(section.body, "Test cases")
            if len(test_case_sections) != 1:
                continue

            for test_case in parse_test_cases(test_case_sections[0]):
                missing_id = missing_backlog_id_from_target(test_case.target)
                if missing_id is None:
                    continue
                ids.setdefault((spec.stem, test_place), set()).add(missing_id)

    return {key: tuple(sorted(values)) for key, values in ids.items()}


def coverage_status(
    is_not_covered: bool,
    concrete_declared_target_count: int,
    missing_test_case_count: int,
) -> str:
    """Classify one feature/test-place coverage row from declared data."""

    if is_not_covered:
        return COVERAGE_STATUS_NOT_COVERED
    if concrete_declared_target_count > 0 and missing_test_case_count > 0:
        return COVERAGE_STATUS_PARTIAL
    if concrete_declared_target_count > 0:
        return COVERAGE_STATUS_COVERED
    if missing_test_case_count > 0:
        return COVERAGE_STATUS_MISSING_BACKLOG
    return COVERAGE_STATUS_MISSING_BACKLOG


def format_coverage_report(rows: list[CoverageReportRow]) -> str:
    """Format coverage rows as a stable markdown table."""

    lines = [
        "Feature coverage report",
        "",
        "| Feature | Test place | Status | Declared scenarios | Missing backlog IDs | Discovered mapped tests | Concrete declared targets | Missing test cases |",
        "| --- | --- | --- | --- | --- | ---: | ---: | ---: |",
    ]
    for row in rows:
        declared_scenarios = (
            ", ".join(row.declared_scenarios) if row.declared_scenarios else "-"
        )
        missing_ids = (
            ", ".join(row.missing_backlog_ids) if row.missing_backlog_ids else "-"
        )
        lines.append(
            f"| {row.feature_id} | {row.test_place} | {row.status} | "
            f"{declared_scenarios} | "
            f"{missing_ids} | "
            f"{row.discovered_mapped_test_count} | "
            f"{row.concrete_declared_target_count} | "
            f"{row.missing_test_case_count} |"
        )
    lines.append("")
    return "\n".join(lines)


def test_target_failures(
    root: Path,
    rel_spec: str,
    feature_id: str,
    test_place: str,
    test_path: str,
    methods: list[str],
) -> list[str]:
    """Validate one concrete test target from a feature spec.

    A valid target is repo-relative, lives under the directory owned by the
    declared test place, points to an existing Rust file, and lists Rust test
    functions defined in that file. Feature-prefixed filenames must map back to
    ``feature_id``; legacy or mixed files without a feature prefix are allowed as
    explicit fallback targets.
    """

    failures: list[str] = []
    normalized_test_path = normalize_changed_path(test_path)
    path = Path(normalized_test_path)
    if path.is_absolute() or normalized_test_path != test_path:
        failures.append(
            f"{rel_spec} test place `{test_place}` target `{test_path}` "
            "must be repo-relative"
        )
        return failures

    if not is_test_place_path(test_place, normalized_test_path):
        allowed = ", ".join(TEST_PLACES[test_place].paths)
        failures.append(
            f"{rel_spec} test place `{test_place}` target `{test_path}` "
            f"must be under {allowed}"
        )

    target_feature_id = feature_id_from_test_path(normalized_test_path)
    if target_feature_id is not None and target_feature_id != feature_id:
        failures.append(
            f"{rel_spec} test place `{test_place}` target `{test_path}` "
            f"maps to feature `{target_feature_id}`, not `{feature_id}`"
        )

    full_path = root / path
    if not full_path.exists():
        failures.append(
            f"{rel_spec} test place `{test_place}` target `{test_path}` does not exist"
        )
        return failures

    function_names = rust_test_function_names(full_path)
    for method in methods:
        if method not in function_names:
            failures.append(
                f"{rel_spec} test place `{test_place}` target `{test_path}` "
                f"does not define test function `{method}`"
            )

    return failures


def rust_test_function_names(path: Path) -> set[str]:
    """Return Rust functions in ``path`` that are annotated as tests.

    Discovery should not require helper functions to be listed in feature specs,
    so it tracks contiguous Rust attributes and records a function only when one
    of those attributes is ``#[test]`` or a macro path ending in ``::test`` such
    as ``#[tokio::test]``. The parser is intentionally lightweight but covers the
    ordinary one-line test attributes used by this repository.
    """

    names: set[str] = set()
    pending_attributes: list[str] = []

    for line in path.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("#[") and stripped.endswith("]"):
            pending_attributes.append(stripped)
            continue

        function = RUST_FUNCTION_LINE_RE.match(line)
        if function:
            if any(is_rust_test_attribute(attribute) for attribute in pending_attributes):
                names.add(function.group(1))
            pending_attributes = []
            continue

        if stripped and not stripped.startswith("//"):
            pending_attributes = []

    return names


def is_rust_test_attribute(attribute: str) -> bool:
    """Return whether one Rust attribute marks the following function as a test."""

    content = attribute.strip().removeprefix("#[").removesuffix("]").strip()
    attribute_path = content.split("(", maxsplit=1)[0].strip()
    return (
        attribute_path == "test"
        or attribute_path.endswith("::test")
        or attribute_path == "rstest"
    )


def no_text_before_child_heading_failures(
    rel_path: str,
    section_name: str,
    markdown: str,
    heading_level: int,
) -> list[str]:
    """Reject free text before required child headings in a section.

    Strict section bodies keep the schema unambiguous: once a parent heading is
    present, content must be grouped under the expected child headings instead
    of floating before them.
    """

    prefix_lines: list[str] = []
    for line in markdown.splitlines():
        heading = HEADING_RE.match(line)
        if heading and len(heading.group(1)) == heading_level:
            break
        prefix_lines.append(line)

    if any(line.strip() for line in prefix_lines):
        marker = "#" * heading_level
        return [
            f"{rel_path} {section_name} must not contain text before its "
            f"`{marker}` child headings"
        ]
    return []


def child_heading_schema_failures(
    rel_path: str,
    scope: str,
    headings: list[str],
    expected_headings: tuple[str, ...],
) -> list[str]:
    """Validate exact child heading presence, uniqueness, and order.

    ``scope`` is preformatted context used in error messages, such as
    ``test place agent-e2e`` or ``subfeature Routing``.
    """

    failures: list[str] = []
    for heading in expected_headings:
        count = headings.count(heading)
        if count == 0:
            failures.append(f"{rel_path} {scope} must include `#### {heading}`")
        elif count > 1:
            failures.append(f"{rel_path} {scope} lists `#### {heading}` more than once")

    expected_set = set(expected_headings)
    for heading in headings:
        if heading not in expected_set:
            failures.append(f"{rel_path} {scope} contains unexpected `#### {heading}`")

    if all(headings.count(heading) == 1 for heading in expected_headings) and set(
        headings
    ) == expected_set and headings != list(expected_headings):
        expected = "`, `".join(f"#### {heading}" for heading in expected_headings)
        failures.append(f"{rel_path} {scope} child headings must be ordered as `{expected}`")

    return failures


def ordered_test_place_failures(
    rel_path: str,
    place_sections: list[MarkdownSection],
    scope: str,
) -> list[str]:
    """Ensure a complete test-place list uses catalog order.

    Order errors are reported only when the known place set is complete. Missing
    or unknown places are handled by their callers, which keeps diagnostics
    focused.
    """

    actual_places: list[str] = []
    for section in place_sections:
        heading = TEST_PLACE_HEADING_RE.fullmatch(section.title)
        if heading is None:
            continue
        test_place = heading.group(1)
        if test_place in TEST_PLACE_IDS:
            actual_places.append(test_place)

    expected_places = list(TEST_PLACE_IDS)
    if (
        sorted(actual_places) == sorted(expected_places)
        and actual_places != expected_places
    ):
        expected = "`, `".join(expected_places)
        return [f"{rel_path} {scope} entries must be ordered as `{expected}`"]
    return []


def test_place_sections(markdown: str) -> list[MarkdownSection]:
    """Return ``###`` test-place sections from a markdown document.

    This helper is shared by tests and future callers that need to inspect the
    structured Test Places body without reimplementing heading parsing.
    """

    sections = sections_named(markdown, "Test Places")
    if not sections:
        return []
    return child_heading_sections(sections[0], 3)


def child_heading_sections(markdown: str, heading_level: int) -> list[MarkdownSection]:
    """Split a markdown fragment into sections at a specific heading level.

    A section ends before the next heading of the same or higher level. Lower
    level headings remain inside the current section body.
    """

    lines = markdown.splitlines()
    sections: list[MarkdownSection] = []
    for index, line in enumerate(lines):
        heading = HEADING_RE.match(line)
        if not heading or len(heading.group(1)) != heading_level:
            continue

        end = len(lines)
        for next_index in range(index + 1, len(lines)):
            next_heading = HEADING_RE.match(lines[next_index])
            if next_heading and len(next_heading.group(1)) <= heading_level:
                end = next_index
                break
        sections.append(
            MarkdownSection(
                title=heading.group(2).strip(),
                body="\n".join(lines[index + 1 : end]),
            )
        )
    return sections


def feature_id_from_test_path(path: str) -> str | None:
    """Map a feature-prefixed Rust test filename to a feature id.

    ``account_pool__routing.rs`` maps to ``account-pool``. Legacy files without
    the ``feature_name__scenario.rs`` shape return ``None`` and are treated as
    unmapped until they are renamed.
    """

    stem = Path(path).stem
    if not E2E_STEM_RE.fullmatch(stem):
        return None
    feature_prefix = stem.split("__", maxsplit=1)[0]
    return feature_prefix.replace("_", "-")


def scenario_id_from_test_path(path: str) -> str | None:
    """Return the normalized scenario suffix from a feature-prefixed test path."""

    stem = Path(path).stem
    if not E2E_STEM_RE.fullmatch(stem):
        return None
    scenario_suffix = stem.split("__", maxsplit=1)[1]
    return scenario_suffix.replace("_", "-")


def scenario_id_from_behavior_description(description: str) -> str | None:
    """Return a low-noise scenario id candidate from test-case behavior text."""

    words = BEHAVIOR_ID_WORD_RE.findall(description.lower())
    if not words:
        return None
    return "-".join(words)


def is_missing_target(target: str) -> bool:
    """Return whether ``target`` is a valid missing-backlog target."""

    return MISSING_TARGET_RE.fullmatch(target) is not None


def missing_backlog_id_from_target(target: str) -> str | None:
    """Return the optional stable id from a missing-backlog target."""

    match = MISSING_TARGET_RE.fullmatch(target)
    if match is None:
        return None
    return match.group(1)


def changed_e2e_failures(root: Path, changed_files: list[str]) -> list[str]:
    """Validate changed mapped test files against their owning spec.

    Only files under known test-place directories participate. Legacy test
    filenames that do not map to a feature id are ignored so existing suites can
    be migrated incrementally.
    """

    failures: list[str] = []
    normalized_changed = {normalize_changed_path(path) for path in changed_files}

    for changed_path in sorted(normalized_changed):
        if test_place_for_test_path(changed_path) is None:
            continue

        feature_id = feature_id_from_test_path(changed_path)
        if feature_id is None:
            continue

        spec_path = FEATURE_SPECS_DIR / f"{feature_id}.md"
        spec_rel = spec_path.as_posix()
        full_spec_path = root / spec_path
        if not full_spec_path.exists():
            failures.append(f"{changed_path} maps to missing feature spec `{spec_rel}`")
            continue

        if spec_rel not in normalized_changed:
            failures.append(
                f"{changed_path} changed without matching feature spec `{spec_rel}`"
            )

    return failures


def changed_files_from_git(root: Path, base: str, head: str) -> list[str]:
    """Return changed repo paths from ``git diff --name-status``.

    Rename and copy records report their destination path because the verifier
    cares about the final filename that determines feature ownership.
    """

    result = subprocess.run(
        [
            "git",
            "diff",
            "--name-status",
            "--find-renames",
            "--diff-filter=ACMR",
            base,
            head,
        ],
        cwd=root,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(result.stderr.strip() or result.stdout.strip())
    return changed_files_from_name_status(result.stdout)


def changed_files_from_working_tree(root: Path) -> list[str]:
    """Return staged, unstaged, and untracked repo paths from the working tree."""

    diff_result = subprocess.run(
        [
            "git",
            "diff",
            "--name-status",
            "--find-renames",
            "--diff-filter=ACMR",
            "HEAD",
        ],
        cwd=root,
        check=False,
        capture_output=True,
        text=True,
    )
    if diff_result.returncode != 0:
        raise SystemExit(diff_result.stderr.strip() or diff_result.stdout.strip())

    untracked_result = subprocess.run(
        [
            "git",
            "ls-files",
            "--others",
            "--exclude-standard",
        ],
        cwd=root,
        check=False,
        capture_output=True,
        text=True,
    )
    if untracked_result.returncode != 0:
        raise SystemExit(untracked_result.stderr.strip() or untracked_result.stdout.strip())

    changed_files = changed_files_from_name_status(diff_result.stdout)
    changed_files.extend(
        line.strip() for line in untracked_result.stdout.splitlines() if line.strip()
    )
    return changed_files


def merge_base(root: Path, base_ref: str, head: str) -> str:
    """Return the merge-base between ``base_ref`` and ``head``."""

    result = subprocess.run(
        [
            "git",
            "merge-base",
            head,
            base_ref,
        ],
        cwd=root,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(result.stderr.strip() or result.stdout.strip())
    return result.stdout.strip()


def changed_files_from_name_status(output: str) -> list[str]:
    """Parse ``git diff --name-status`` output into changed destination paths."""

    changed_files: list[str] = []
    for line in output.splitlines():
        parts = line.split("\t")
        if not parts:
            continue
        status = parts[0]
        if status.startswith(("R", "C")) and len(parts) == 3:
            changed_files.append(parts[2])
        elif len(parts) >= 2:
            changed_files.append(parts[1])
    return changed_files


def feature_spec_files(root: Path) -> list[Path]:
    """Return feature spec markdown files, excluding README and template files."""

    feature_dir = root / FEATURE_SPECS_DIR
    if not feature_dir.exists():
        return []
    ignored = {README_NAME, TEMPLATE_NAME}
    return sorted(
        path for path in feature_dir.glob("*.md") if path.name not in ignored
    )


def heading_entries(markdown: str) -> list[tuple[int, str, str]]:
    """Return all markdown headings with line number, marker, and title."""

    entries: list[tuple[int, str, str]] = []
    for line_number, line in enumerate(markdown.splitlines(), start=1):
        heading = HEADING_RE.match(line)
        if heading:
            entries.append((line_number, heading.group(1), heading.group(2).strip()))
    return entries


def top_level_heading_titles(markdown: str) -> list[str]:
    """Return titles for all ``##`` headings in document order."""

    headings: list[str] = []
    for _line_number, hashes, title in heading_entries(markdown):
        if len(hashes) == 2:
            headings.append(title)
    return headings


def sections_named(markdown: str, title: str) -> list[str]:
    """Return bodies for every heading whose title exactly matches ``title``.

    The heading level is determined from each match, so the helper works for
    top-level feature sections and nested child sections alike.
    """

    lines = markdown.splitlines()
    sections: list[str] = []
    for index, line in enumerate(lines):
        heading = HEADING_RE.match(line)
        if not heading or heading.group(2).strip() != title:
            continue

        level = len(heading.group(1))
        end = len(lines)
        for next_index in range(index + 1, len(lines)):
            next_heading = HEADING_RE.match(lines[next_index])
            if next_heading and len(next_heading.group(1)) <= level:
                end = next_index
                break
        sections.append("\n".join(lines[index + 1 : end]))
    return sections


def markdown_links(markdown: str) -> list[MarkdownLink]:
    """Return markdown links found in a markdown fragment."""

    return [
        MarkdownLink(match.group(1), match.group(2))
        for match in MARKDOWN_LINK_RE.finditer(markdown)
    ]


def is_test_path_reference(root: Path, source_path: Path, value: str) -> bool:
    """Return whether a markdown link text or target appears to reference tests."""

    path_text = value.split("#", maxsplit=1)[0]
    if not path_text.endswith(".rs"):
        return False

    target_path = resolve_link_target(root, source_path, path_text)
    if target_path is None:
        rel_path = normalize_changed_path(path_text)
    else:
        rel_path = relative_path(root, target_path)
    if "/tests/" in rel_path:
        return True
    if test_place_for_test_path(rel_path) is None:
        return False
    stem = Path(rel_path).stem
    return feature_id_from_test_path(rel_path) is not None or stem.endswith("_tests")


def resolve_link_target(root: Path, source_path: Path, target: str) -> Path | None:
    """Resolve a markdown link target and require it to stay under ``root``.

    Repo-relative links, root-relative links, and links relative to the source
    file are supported. External links and empty fragment-only links are
    rejected because feature specs should point at concrete in-repo entry
    points.
    """

    if "://" in target or target.startswith(("mailto:", "tel:")):
        return None

    path_text = target.split("#", maxsplit=1)[0]
    if not path_text:
        return None

    if path_text.startswith("/"):
        candidate = root / path_text.lstrip("/")
    elif (root / path_text).exists():
        candidate = root / path_text
    else:
        candidate = source_path.parent / path_text

    root_resolved = root.resolve()
    candidate_resolved = candidate.resolve()
    try:
        candidate_resolved.relative_to(root_resolved)
    except ValueError:
        return None
    return candidate_resolved


def is_test_place_path(test_place: str, path: str) -> bool:
    """Return whether ``path`` is a Rust test file owned by ``test_place``."""

    roots = TEST_PLACES[test_place].paths
    path_parts = Path(path).parts
    if Path(path).suffix != ".rs" or Path(path).name == "mod.rs":
        return False
    return any(path_parts[: len(Path(root).parts)] == Path(root).parts for root in roots)


def test_place_for_test_path(path: str) -> str | None:
    """Return the owning test place for a repo-relative Rust test path."""

    for test_place in TEST_PLACE_IDS:
        if is_test_place_path(test_place, path):
            return test_place
    return None


def normalize_changed_path(path: str) -> str:
    """Normalize git-style paths to simple repo-relative POSIX paths."""

    return Path(path).as_posix().removeprefix("./")


def relative_path(root: Path, path: Path) -> str:
    """Return ``path`` relative to ``root`` when possible for diagnostics."""

    try:
        return path.relative_to(root).as_posix()
    except ValueError:
        return path.as_posix()


if __name__ == "__main__":
    sys.exit(main())
