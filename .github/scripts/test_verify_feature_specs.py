#!/usr/bin/env python3

import subprocess
import sys
import textwrap
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

import verify_feature_specs


class VerifyFeatureSpecsTest(unittest.TestCase):
    def test_valid_feature_specs_pass(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)

            self.assertEqual(
                verify_feature_specs.verify_feature_specs(
                    root,
                    changed_files=[
                        "codex-rs/feature-specs/account-pool.md",
                        "codex-rs/core/tests/suite/account_pool__routing.rs",
                    ],
                ),
                [],
            )

    def test_readme_must_list_each_spec_once_in_sorted_order(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            self.write_file(
                root / "codex-rs/feature-specs/rate-limits.md",
                self.spec_text("rate-limits"),
            )
            self.write_file(
                root / "codex-rs/core/tests/suite/rate_limits__routing.rs",
                "#[test]\nfn routing_test() {}\n",
            )
            self.write_file(
                root / "codex-rs/feature-specs/README.md",
                """
                # Feature Specs

                ## Feature Index

                - [rate-limits](rate-limits.md)
                - [account-pool](account-pool.md)
                - [account-pool](account-pool.md)
                """,
            )

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/README.md is missing `## Test Places`",
                "codex-rs/feature-specs/README.md lists `account-pool.md` more than once",
                "codex-rs/feature-specs/README.md feature links must be sorted",
            ],
        )

    def test_readme_test_place_records_must_match_catalog(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            readme = root / "codex-rs/feature-specs/README.md"
            text = readme.read_text(encoding="utf-8")
            text = text.replace("Agent E2E", "Agent Flow", 1)
            text = text.replace(
                verify_feature_specs.TEST_PLACES["agent-e2e"].long_description,
                "Vague agent tests.",
                1,
            )
            text = text.replace(
                "- `codex-rs/core/tests/suite`",
                "- `codex-rs/core/tests`",
                1,
            )
            readme.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/README.md README test place `agent-e2e` "
                "`Name` must be `Agent E2E`",
                "codex-rs/feature-specs/README.md README test place `agent-e2e` "
                "`Description` must be "
                f"`{verify_feature_specs.TEST_PLACES['agent-e2e'].long_description}`",
                "codex-rs/feature-specs/README.md README test place `agent-e2e` "
                "`Path Ownership Rules` must list `codex-rs/core/tests/suite`",
            ],
        )

    def test_readme_test_place_records_must_include_path_ownership_rules(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            readme = root / "codex-rs/feature-specs/README.md"
            text = readme.read_text(encoding="utf-8")
            text = text.replace(
                "\n\n#### Path Ownership Rules\n\n- `codex-rs/core/tests/suite`",
                "",
                1,
            )
            readme.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/README.md README test place `agent-e2e` "
                "must include `#### Path Ownership Rules`",
            ],
        )

    def test_spec_must_use_required_sections_and_valid_entry_points(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            text = text.replace(
                "## Invariants\n\n"
                "- Invariant.\n\n",
                "",
            )
            text = text.replace(
                "../login/src/auth/account_pool.rs",
                "../login/src/auth/missing.rs",
            )
            spec.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md is missing `## Invariants`",
                "codex-rs/feature-specs/account-pool.md Entry Points link "
                "`../login/src/auth/missing.rs` does not resolve inside the repository",
            ],
        )

    def test_test_places_must_list_each_catalog_place(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            start = text.index("### exec-server (exec-server service boundary behavior)")
            end = text.index("## Test Generation Notes")
            text = text[:start] + text[end:]
            text = text.replace(
                "### otel (telemetry and export behavior)",
                "### otel (wrong description)",
            )
            spec.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md test place `otel` heading "
                "description must be `telemetry and export behavior`",
                "codex-rs/feature-specs/account-pool.md Test Places is missing `exec-server`",
            ],
        )

    def test_test_places_reports_all_missing_catalog_places(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            start = text.index("## Test Places")
            end = text.index("## Test Generation Notes")
            text = text[:start] + "## Test Places\n\n" + text[end:]
            spec.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md Test Places section must list test places",
                *[
                    f"codex-rs/feature-specs/account-pool.md Test Places is missing `{test_place}`"
                    for test_place in verify_feature_specs.TEST_PLACE_IDS
                ],
                "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test maps "
                "to feature `account-pool` and test place `agent-e2e` but is not listed "
                "in `codex-rs/feature-specs/account-pool.md` Test cases",
            ],
        )

    def test_test_place_block_reports_multiple_missing_subsections(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            text = text.replace("#### Test cases", "#### Cases removed", 1)
            spec.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md test place `agent-e2e` "
                "must include `#### Test cases`",
                "codex-rs/feature-specs/account-pool.md test place `agent-e2e` "
                "contains unexpected `#### Cases removed`",
                "codex-rs/feature-specs/account-pool.md test place `agent-e2e` "
                "must include `#### Test cases`",
                "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test maps "
                "to feature `account-pool` and test place `agent-e2e` but is not listed "
                "in `codex-rs/feature-specs/account-pool.md` Test cases",
            ],
        )

    def test_not_covered_status_must_not_include_coverage_sections(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            text = text.replace(
                "#### Status\n\n"
                "Not covered",
                "#### Status\n\n"
                "Not covered\n\n"
                "#### Test cases\n\n"
                "- Add coverage: missing",
                1,
            )
            spec.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md test place `app-server-api` "
                "contains unexpected `#### Test cases`",
                "codex-rs/feature-specs/account-pool.md test place `app-server-api` "
                "with Status `Not covered` must not include `#### Test cases`",
                "codex-rs/feature-specs/account-pool.md test place `app-server-api` "
                "with Status `Not covered` must only include Description and Status",
            ],
        )

    def test_test_case_targets_must_exist(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            text = text.replace(
                "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test",
                "codex-rs/core/tests/suite/account_pool__missing.rs:routing_test",
                1,
            )
            spec.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md test place `agent-e2e` target "
                "`codex-rs/core/tests/suite/account_pool__missing.rs` does not exist",
                "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test maps "
                "to feature `account-pool` and test place `agent-e2e` but is not listed "
                "in `codex-rs/feature-specs/account-pool.md` Test cases",
            ],
        )

    def test_test_case_targets_must_define_methods(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            text = text.replace("routing_test", "missing_test", 1)
            spec.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md test place `agent-e2e` target "
                "`codex-rs/core/tests/suite/account_pool__routing.rs` does not define "
                "test function `missing_test`",
                "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test maps "
                "to feature `account-pool` and test place `agent-e2e` but is not listed "
                "in `codex-rs/feature-specs/account-pool.md` Test cases",
            ],
        )

    def test_test_case_targets_must_use_matching_feature_filename(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            self.write_file(
                root / "codex-rs/core/tests/suite/rate_limits__routing.rs",
                "#[test]\nfn routing_test() {}\n",
            )
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            text = text.replace(
                "codex-rs/core/tests/suite/account_pool__routing.rs",
                "codex-rs/core/tests/suite/rate_limits__routing.rs",
                1,
            )
            spec.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md test place `agent-e2e` target "
                "`codex-rs/core/tests/suite/rate_limits__routing.rs` maps to feature "
                "`rate-limits`, not `account-pool`",
                "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test maps "
                "to feature `account-pool` and test place `agent-e2e` but is not listed "
                "in `codex-rs/feature-specs/account-pool.md` Test cases",
                "codex-rs/core/tests/suite/rate_limits__routing.rs:routing_test maps "
                "to missing feature spec `codex-rs/feature-specs/rate-limits.md`",
            ],
        )

    def test_test_case_targets_must_be_in_test_place_path(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            self.write_file(
                root / "codex-rs/cli/tests/account_pool__routing.rs",
                "#[test]\nfn routing_test() {}\n",
            )
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            text = text.replace(
                "codex-rs/core/tests/suite/account_pool__routing.rs",
                "codex-rs/cli/tests/account_pool__routing.rs",
                1,
            )
            spec.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md test place `agent-e2e` "
                "target `codex-rs/cli/tests/account_pool__routing.rs` must be under "
                "codex-rs/core/tests/suite",
                "codex-rs/feature-specs/account-pool.md test place `cli` is "
                "`Not covered` but discovered mapped test "
                "`codex-rs/cli/tests/account_pool__routing.rs:routing_test`",
                "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test maps "
                "to feature `account-pool` and test place `agent-e2e` but is not listed "
                "in `codex-rs/feature-specs/account-pool.md` Test cases",
                "codex-rs/cli/tests/account_pool__routing.rs:routing_test maps "
                "to feature `account-pool` and test place `cli` but is not listed "
                "in `codex-rs/feature-specs/account-pool.md` Test cases",
            ],
        )

    def test_specs_must_not_list_test_links(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            text = text.replace(
                "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test",
                "[account_pool__routing](../core/tests/suite/account_pool__routing.rs)",
            )
            spec.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md:34 must not include test links; "
                "test ownership is derived from filenames",
                "codex-rs/feature-specs/account-pool.md test place `agent-e2e` "
                "test case `- Routing behavior is covered: "
                "[account_pool__routing](../core/tests/suite/account_pool__routing.rs)` "
                "must target `repo/path.rs:test_name[,test_name]` or `missing`",
                "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test maps "
                "to feature `account-pool` and test place `agent-e2e` but is not listed "
                "in `codex-rs/feature-specs/account-pool.md` Test cases",
            ],
        )

    def test_specs_must_not_link_tui_component_test_files(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            text = text.replace(
                "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test",
                "[account_pool__status_tests](../tui/src/status/account_pool__status_tests.rs)",
            )
            spec.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md:34 must not include test links; "
                "test ownership is derived from filenames",
                "codex-rs/feature-specs/account-pool.md test place `agent-e2e` "
                "test case `- Routing behavior is covered: "
                "[account_pool__status_tests](../tui/src/status/account_pool__status_tests.rs)` "
                "must target `repo/path.rs:test_name[,test_name]` or `missing`",
                "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test maps "
                "to feature `account-pool` and test place `agent-e2e` but is not listed "
                "in `codex-rs/feature-specs/account-pool.md` Test cases",
            ],
        )

    def test_specs_can_link_tui_implementation_entry_points(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            self.write_file(root / "codex-rs/tui/src/chatwidget/plugins.rs", "")
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            text = text.replace(
                "- [codex-rs/login/src/auth/account_pool.rs](../login/src/auth/account_pool.rs)",
                "- [codex-rs/login/src/auth/account_pool.rs](../login/src/auth/account_pool.rs)\n"
                "- [codex-rs/tui/src/chatwidget/plugins.rs](../tui/src/chatwidget/plugins.rs)",
                1,
            )
            spec.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(failures, [])

    def test_e2e_coverage_sections_are_rejected(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            text = text.replace(
                "None.",
                "### Routing\n\n"
                "#### Entry Points\n\n"
                "- [codex-rs/login/src/auth/account_pool.rs](../login/src/auth/account_pool.rs)\n\n"
                "#### Invariants\n\n"
                "- Routing is stable.\n\n"
                "#### E2E Coverage\n\n"
                "- Obsolete.",
                1,
            )
            text = text.replace(
                "## Test Generation Notes",
                "## E2E Coverage\n\n"
                "- Obsolete.\n\n"
                "## Test Generation Notes",
            )
            spec.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md contains unexpected "
                "`## E2E Coverage`",
                "codex-rs/feature-specs/account-pool.md:27 must not include "
                "`#### E2E Coverage`; use `## Test Places`",
                "codex-rs/feature-specs/account-pool.md:159 must not include "
                "`## E2E Coverage`; use `## Test Places`",
                "codex-rs/feature-specs/account-pool.md subfeature `Routing` "
                "contains unexpected `#### E2E Coverage`",
            ],
        )

    def test_spec_must_not_define_feature_id_field(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            spec.write_text(text.replace("## Summary", "Feature ID: account-pool\n\n## Summary"))

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md:3 must not define a Feature ID field",
            ],
        )

    def test_changed_mapped_test_requires_matching_spec_change(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)

            failures = verify_feature_specs.verify_feature_specs(
                root,
                changed_files=["codex-rs/core/tests/suite/account_pool__missing.rs"],
            )
        self.assertEqual(
            failures,
            [
                "codex-rs/core/tests/suite/account_pool__missing.rs changed without "
                "matching feature spec `codex-rs/feature-specs/account-pool.md`",
            ],
        )

    def test_changed_legacy_test_names_are_unmapped(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)

            failures = verify_feature_specs.verify_feature_specs(
                root,
                changed_files=["codex-rs/app-server/tests/suite/v2/account.rs"],
            )

        self.assertEqual(failures, [])

    def test_explicit_legacy_target_counts_as_declared_coverage(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            self.write_file(
                root / "codex-rs/core/tests/suite/mixed.rs",
                "#[test]\nfn legacy_test() {}\n",
            )
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            spec.write_text(
                text.replace(
                    "- Missing routing edge case: missing",
                    "- Legacy mixed test is explicit: codex-rs/core/tests/suite/mixed.rs:legacy_test\n"
                    "- Missing routing edge case: missing",
                ),
                encoding="utf-8",
            )

            failures = verify_feature_specs.verify_feature_specs(
                root,
                changed_files=["codex-rs/core/tests/suite/mixed.rs"],
            )
            rows = verify_feature_specs.feature_coverage_report(root)

        self.assertEqual(failures, [])
        self.assertEqual(
            rows[0],
            verify_feature_specs.CoverageReportRow(
                feature_id="account-pool",
                test_place="agent-e2e",
                status=verify_feature_specs.COVERAGE_STATUS_PARTIAL,
                discovered_mapped_test_count=1,
                concrete_declared_target_count=2,
                missing_test_case_count=1,
            ),
        )

    def test_explicit_legacy_target_must_be_a_rust_test_function(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            self.write_file(
                root / "codex-rs/core/tests/suite/mixed.rs",
                "fn helper_only() {}\n",
            )
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            spec.write_text(
                text.replace(
                    "- Missing routing edge case: missing",
                    "- Legacy helper is not coverage: codex-rs/core/tests/suite/mixed.rs:helper_only\n"
                    "- Missing routing edge case: missing",
                ),
                encoding="utf-8",
            )

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md test place `agent-e2e` "
                "target `codex-rs/core/tests/suite/mixed.rs` does not define "
                "test function `helper_only`",
            ],
        )

    def test_discovered_mapped_test_must_be_listed_in_feature_spec(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            self.write_file(
                root / "codex-rs/core/tests/suite/account_pool__unlisted.rs",
                "#[test]\nfn unlisted_test() {}\n",
            )

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/core/tests/suite/account_pool__unlisted.rs:unlisted_test maps "
                "to feature `account-pool` and test place `agent-e2e` but is not listed "
                "in `codex-rs/feature-specs/account-pool.md` Test cases",
            ],
        )

    def test_applicable_test_place_requires_concrete_or_missing_backlog(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            text = text.replace(
                "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test",
                "todo",
                1,
            )
            text = text.replace(
                "- Missing routing edge case: missing",
                "- Missing routing edge case: todo",
                1,
            )
            spec.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md test place `agent-e2e` "
                "test case `- Routing behavior is covered: todo` must target "
                "`repo/path.rs:test_name[,test_name]` or `missing`",
                "codex-rs/feature-specs/account-pool.md test place `agent-e2e` "
                "test case `- Missing routing edge case: todo` must target "
                "`repo/path.rs:test_name[,test_name]` or `missing`",
                "codex-rs/feature-specs/account-pool.md test place `agent-e2e` "
                "Test cases must include at least one concrete target or `missing` "
                "backlog item",
                "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test maps "
                "to feature `account-pool` and test place `agent-e2e` but is not listed "
                "in `codex-rs/feature-specs/account-pool.md` Test cases",
            ],
        )

    def test_feature_coverage_report_counts_targets_and_missing_backlog(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            self.write_file(
                root / "codex-rs/cli/tests/account_pool__list.rs",
                "#[test]\nfn list_test() {}\n",
            )
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            text = self.replace_test_place_block(
                text,
                "cli",
                """
                ### cli (main CLI command behavior)

                #### Description

                CLI routing behavior should be tested through the main CLI path.

                #### Test cases

                - Account list behavior is covered: codex-rs/cli/tests/account_pool__list.rs:list_test
                """,
            )
            text = self.replace_test_place_block(
                text,
                "tui-e2e",
                """
                ### tui-e2e (full terminal TUI behavior)

                #### Description

                Full terminal TUI behavior still needs coverage.

                #### Test cases

                - Live account list behavior is covered: missing
                """,
            )
            spec.write_text(text, encoding="utf-8")

            rows = verify_feature_specs.feature_coverage_report(root)

        expected_rows_by_place = {
            "agent-e2e": (
                verify_feature_specs.COVERAGE_STATUS_PARTIAL,
                1,
                1,
                1,
            ),
            "cli": (
                verify_feature_specs.COVERAGE_STATUS_COVERED,
                1,
                1,
                0,
            ),
            "tui-e2e": (
                verify_feature_specs.COVERAGE_STATUS_MISSING_BACKLOG,
                0,
                0,
                1,
            ),
        }

        def expected_row(test_place: str) -> verify_feature_specs.CoverageReportRow:
            status, discovered_count, concrete_count, missing_count = (
                expected_rows_by_place.get(
                    test_place,
                    (
                        verify_feature_specs.COVERAGE_STATUS_NOT_COVERED,
                        0,
                        0,
                        0,
                    ),
                )
            )
            return verify_feature_specs.CoverageReportRow(
                feature_id="account-pool",
                test_place=test_place,
                status=status,
                discovered_mapped_test_count=discovered_count,
                concrete_declared_target_count=concrete_count,
                missing_test_case_count=missing_count,
            )

        self.assertEqual(
            rows,
            [expected_row(test_place) for test_place in verify_feature_specs.TEST_PLACE_IDS],
        )
        self.assertEqual(
            verify_feature_specs.format_coverage_report(rows),
            textwrap.dedent(
                """
                Feature coverage report

                | Feature | Test place | Status | Discovered mapped tests | Concrete declared targets | Missing test cases |
                | --- | --- | --- | ---: | ---: | ---: |
                | account-pool | agent-e2e | partial | 1 | 1 | 1 |
                | account-pool | app-server-api | not-covered | 0 | 0 | 0 |
                | account-pool | cli | covered | 1 | 1 | 0 |
                | account-pool | tui-e2e | missing-backlog | 0 | 0 | 1 |
                | account-pool | tui-component | not-covered | 0 | 0 | 0 |
                | account-pool | login-auth | not-covered | 0 | 0 | 0 |
                | account-pool | mcp-server | not-covered | 0 | 0 | 0 |
                | account-pool | rmcp-client | not-covered | 0 | 0 | 0 |
                | account-pool | codex-api | not-covered | 0 | 0 | 0 |
                | account-pool | exec-cli | not-covered | 0 | 0 | 0 |
                | account-pool | otel | not-covered | 0 | 0 | 0 |
                | account-pool | exec-server | not-covered | 0 | 0 | 0 |
                """
            ).lstrip(),
        )

    def test_feature_coverage_report_excludes_invalid_declared_targets(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            test_file = root / "codex-rs/core/tests/suite/account_pool__routing.rs"
            test_file.write_text("fn helper_only() {}\n", encoding="utf-8")
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            spec.write_text(
                text.replace(
                    "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test",
                    "codex-rs/core/tests/suite/account_pool__routing.rs:helper_only",
                ),
                encoding="utf-8",
            )

            rows = verify_feature_specs.feature_coverage_report(root)

        self.assertEqual(
            rows[0],
            verify_feature_specs.CoverageReportRow(
                feature_id="account-pool",
                test_place="agent-e2e",
                status=verify_feature_specs.COVERAGE_STATUS_MISSING_BACKLOG,
                discovered_mapped_test_count=0,
                concrete_declared_target_count=0,
                missing_test_case_count=1,
            ),
        )

    def test_cli_coverage_report_prints_after_successful_validation(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)

            result = self.run_feature_specs_cli(root, "--coverage-report")

        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stderr, "")
        self.assertEqual(
            result.stdout,
            textwrap.dedent(
                """
                Feature coverage report

                | Feature | Test place | Status | Discovered mapped tests | Concrete declared targets | Missing test cases |
                | --- | --- | --- | ---: | ---: | ---: |
                | account-pool | agent-e2e | partial | 1 | 1 | 1 |
                | account-pool | app-server-api | not-covered | 0 | 0 | 0 |
                | account-pool | cli | not-covered | 0 | 0 | 0 |
                | account-pool | tui-e2e | not-covered | 0 | 0 | 0 |
                | account-pool | tui-component | not-covered | 0 | 0 | 0 |
                | account-pool | login-auth | not-covered | 0 | 0 | 0 |
                | account-pool | mcp-server | not-covered | 0 | 0 | 0 |
                | account-pool | rmcp-client | not-covered | 0 | 0 | 0 |
                | account-pool | codex-api | not-covered | 0 | 0 | 0 |
                | account-pool | exec-cli | not-covered | 0 | 0 | 0 |
                | account-pool | otel | not-covered | 0 | 0 | 0 |
                | account-pool | exec-server | not-covered | 0 | 0 | 0 |
                """
            ).lstrip(),
        )

    def test_cli_coverage_report_is_not_printed_after_validation_failure(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            spec.write_text(text.replace("routing_test", "missing_test", 1))

            result = self.run_feature_specs_cli(root, "--coverage-report")

        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr, "")
        self.assertNotIn("Feature coverage report", result.stdout)
        self.assertIn(
            "Feature specs must be indexed, deterministic, and linked to test ownership.",
            result.stdout,
        )
        self.assertIn(
            "does not define test function `missing_test`",
            result.stdout,
        )

    def test_discovered_helper_functions_do_not_require_spec_targets(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            test_file = root / "codex-rs/core/tests/suite/account_pool__routing.rs"
            text = test_file.read_text(encoding="utf-8")
            test_file.write_text(
                text + "\nfn helper_function() {}\n",
                encoding="utf-8",
            )

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(failures, [])

    def test_declared_target_must_be_a_rust_test_function(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            test_file = root / "codex-rs/core/tests/suite/account_pool__routing.rs"
            test_file.write_text("fn helper_only() {}\n", encoding="utf-8")
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            spec.write_text(
                text.replace(
                    "codex-rs/core/tests/suite/account_pool__routing.rs:routing_test",
                    "codex-rs/core/tests/suite/account_pool__routing.rs:helper_only",
                ),
                encoding="utf-8",
            )

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md test place `agent-e2e` "
                "target `codex-rs/core/tests/suite/account_pool__routing.rs` "
                "does not define test function `helper_only`",
            ],
        )

    def test_not_covered_place_cannot_have_discovered_mapped_tests(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            self.write_file(
                root / "codex-rs/cli/tests/account_pool__list.rs",
                "#[test]\nfn list_test() {}\n",
            )

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertEqual(
            failures,
            [
                "codex-rs/feature-specs/account-pool.md test place `cli` is "
                "`Not covered` but discovered mapped test "
                "`codex-rs/cli/tests/account_pool__list.rs:list_test`",
                "codex-rs/cli/tests/account_pool__list.rs:list_test maps "
                "to feature `account-pool` and test place `cli` but is not listed "
                "in `codex-rs/feature-specs/account-pool.md` Test cases",
            ],
        )

    def test_name_status_parses_renamed_target(self) -> None:
        self.assertEqual(
            verify_feature_specs.changed_files_from_name_status(
                "M\tcodex-rs/feature-specs/account-pool.md\n"
                "R100\told.rs\tcodex-rs/core/tests/suite/account_pool__routing.rs\n"
            ),
            [
                "codex-rs/feature-specs/account-pool.md",
                "codex-rs/core/tests/suite/account_pool__routing.rs",
            ],
        )

    def test_working_tree_changed_files_include_tracked_and_untracked(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.run_git(root, "init")
            self.write_file(root / "tracked.rs", "fn main() {}\n")
            self.run_git(root, "add", "tracked.rs")
            self.run_git(
                root,
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            )

            self.write_file(root / "tracked.rs", "fn changed() {}\n")
            self.write_file(root / "untracked.rs", "fn new_file() {}\n")

            changed_files = verify_feature_specs.changed_files_from_working_tree(root)

        self.assertEqual(sorted(changed_files), ["tracked.rs", "untracked.rs"])

    def run_feature_specs_cli(
        self,
        root: Path,
        *args: str,
    ) -> subprocess.CompletedProcess[str]:
        script = textwrap.dedent(
            """
            import sys
            from pathlib import Path

            import verify_feature_specs

            verify_feature_specs.ROOT = Path(sys.argv[1])
            sys.argv = ["verify_feature_specs.py", *sys.argv[2:]]
            raise SystemExit(verify_feature_specs.main())
            """
        )
        return subprocess.run(
            [sys.executable, "-c", script, str(root), *args],
            cwd=Path(__file__).parent,
            check=False,
            capture_output=True,
            text=True,
        )

    def replace_test_place_block(self, text: str, test_place: str, replacement: str) -> str:
        start = text.index(f"### {test_place} (")
        next_starts = []
        for catalog_test_place in verify_feature_specs.TEST_PLACE_IDS:
            next_start = text.find(f"\n### {catalog_test_place} (", start + 1)
            if next_start != -1:
                next_starts.append(next_start)
        test_generation_notes = text.find("\n## Test Generation Notes", start)
        if test_generation_notes != -1:
            next_starts.append(test_generation_notes)
        end = min(next_starts)
        return text[:start] + textwrap.dedent(replacement).strip() + text[end:]

    def write_valid_repo(self, root: Path) -> None:
        self.write_file(
            root / "codex-rs/feature-specs/README.md",
            self.readme_text(),
        )
        self.write_file(root / "codex-rs/feature-specs/TEMPLATE.md", "# Template\n")
        self.write_file(
            root / "codex-rs/feature-specs/account-pool.md",
            self.spec_text("account-pool"),
        )
        self.write_file(root / "codex-rs/login/src/auth/account_pool.rs", "")
        self.write_file(
            root / "codex-rs/core/tests/suite/account_pool__routing.rs",
            "#[test]\nfn routing_test() {}\n",
        )

    def readme_text(self) -> str:
        test_places = "\n\n".join(
            textwrap.dedent(
                f"""
                ### {test_place} ({verify_feature_specs.TEST_PLACES[test_place].short_description})

                #### Name

                {verify_feature_specs.TEST_PLACES[test_place].name}

                #### Short Description

                {verify_feature_specs.TEST_PLACES[test_place].short_description}

                #### Description

                {verify_feature_specs.TEST_PLACES[test_place].long_description}

                #### Path Ownership Rules

                {self.readme_path_rules(test_place)}
                """
            ).strip()
            for test_place in verify_feature_specs.TEST_PLACE_IDS
        )
        return (
            "# Feature Specs\n\n"
            "## Test Places\n\n"
            f"{test_places}\n\n"
            "## Feature Index\n\n"
            "- [account-pool](account-pool.md)\n"
        )

    def readme_path_rules(self, test_place: str) -> str:
        return "\n".join(
            f"- `{path}`" for path in verify_feature_specs.TEST_PLACES[test_place].paths
        )

    def spec_text(self, name: str) -> str:
        title = name.replace("-", " ").title()
        snake_name = name.replace("-", "_")
        return textwrap.dedent(
            f"""
            # {title}

            ## Summary

            Summary.

            ## Behavior

            Behavior.

            ## Entry Points

            - [codex-rs/login/src/auth/account_pool.rs](../login/src/auth/account_pool.rs)

            ## Subfeatures

            None.

            ## Invariants

            - Invariant.

            ## Test Places

            {self.indented_feature_test_places_text(snake_name)}

            ## Test Generation Notes

            Notes.
            """
        ).lstrip()

    def indented_feature_test_places_text(self, snake_name: str) -> str:
        return "\n" + textwrap.indent(
            self.feature_test_places_text(snake_name),
            "            ",
        )

    def feature_test_places_text(self, snake_name: str) -> str:
        covered = textwrap.dedent(
            f"""
            ### agent-e2e (agent behavior under core integration tests)

            #### Description

            Routing behavior should be tested through the agent path.

            #### Test cases

            - Routing behavior is covered: codex-rs/core/tests/suite/{snake_name}__routing.rs:routing_test
            - Missing routing edge case: missing
            """
        ).strip()
        not_covered = [
            (
                "app-server-api",
                "app-server API behavior",
                "This fixture feature has no app-server API surface.",
            ),
            ("cli", "main CLI command behavior", "This fixture feature has no CLI surface."),
            (
                "tui-e2e",
                "full terminal TUI behavior",
                "This fixture feature has no full terminal TUI flow.",
            ),
            (
                "tui-component",
                "focused TUI component behavior",
                "This fixture feature has no focused TUI component behavior.",
            ),
            (
                "login-auth",
                "auth and login behavior",
                "This fixture feature has no auth or login behavior.",
            ),
            (
                "mcp-server",
                "Codex-as-MCP-server behavior",
                "This fixture feature has no Codex-as-MCP-server behavior.",
            ),
            (
                "rmcp-client",
                "MCP client transport and resource behavior",
                "This fixture feature has no MCP client transport behavior.",
            ),
            (
                "codex-api",
                "Codex API client and protocol behavior",
                "This fixture feature has no Codex API behavior.",
            ),
            (
                "exec-cli",
                "codex exec CLI behavior",
                "This fixture feature has no exec CLI behavior.",
            ),
            (
                "otel",
                "telemetry and export behavior",
                "This fixture feature has no telemetry behavior.",
            ),
            (
                "exec-server",
                "exec-server service boundary behavior",
                "This fixture feature has no exec-server behavior.",
            ),
        ]
        blocks = [covered]
        blocks.extend(
            textwrap.dedent(
                f"""
                ### {test_place} ({description})

                #### Description

                {reason}

                #### Status

                Not covered
                """
            ).strip()
            for test_place, description, reason in not_covered
        )
        return "\n\n".join(blocks)

    def run_git(self, root: Path, *args: str) -> None:
        subprocess.run(
            ["git", *args],
            cwd=root,
            check=True,
            capture_output=True,
            text=True,
        )

    def write_file(self, path: Path, text: str) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(textwrap.dedent(text).lstrip(), encoding="utf-8")


if __name__ == "__main__":
    unittest.main()
