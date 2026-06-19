#!/usr/bin/env python3

import subprocess
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

        self.assertIn(
            "codex-rs/feature-specs/README.md lists `account-pool.md` more than once",
            failures,
        )
        self.assertIn(
            "codex-rs/feature-specs/README.md feature links must be sorted",
            failures,
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
            readme.write_text(text, encoding="utf-8")

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertIn(
            "codex-rs/feature-specs/README.md README test place `agent-e2e` "
            "`Name` must be `Agent E2E`",
            failures,
        )
        self.assertIn(
            "codex-rs/feature-specs/README.md README test place `agent-e2e` "
            "`Description` must be "
            f"`{verify_feature_specs.TEST_PLACES['agent-e2e'].long_description}`",
            failures,
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

        self.assertIn(
            "codex-rs/feature-specs/account-pool.md is missing `## Invariants`",
            failures,
        )
        self.assertIn(
            "codex-rs/feature-specs/account-pool.md Entry Points link "
            "`../login/src/auth/missing.rs` does not resolve inside the repository",
            failures,
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

        self.assertIn(
            "codex-rs/feature-specs/account-pool.md Test Places is missing `exec-server`",
            failures,
        )
        self.assertIn(
            "codex-rs/feature-specs/account-pool.md test place `otel` heading "
            "description must be `telemetry and export behavior`",
            failures,
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

        self.assertIn(
            "codex-rs/feature-specs/account-pool.md Test Places section must list test places",
            failures,
        )
        self.assertIn(
            "codex-rs/feature-specs/account-pool.md Test Places is missing `agent-e2e`",
            failures,
        )
        self.assertIn(
            "codex-rs/feature-specs/account-pool.md Test Places is missing `exec-server`",
            failures,
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

        self.assertIn(
            "codex-rs/feature-specs/account-pool.md test place `agent-e2e` "
            "must include `#### Test cases`",
            failures,
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

        self.assertIn(
            "codex-rs/feature-specs/account-pool.md test place `app-server-api` "
            "with Status `Not covered` must not include `#### Test cases`",
            failures,
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

        self.assertIn(
            "codex-rs/feature-specs/account-pool.md test place `agent-e2e` target "
            "`codex-rs/core/tests/suite/account_pool__missing.rs` does not exist",
            failures,
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

        self.assertIn(
            "codex-rs/feature-specs/account-pool.md test place `agent-e2e` target "
            "`codex-rs/core/tests/suite/account_pool__routing.rs` does not define "
            "`missing_test`",
            failures,
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

        self.assertIn(
            "codex-rs/feature-specs/account-pool.md test place `agent-e2e` target "
            "`codex-rs/core/tests/suite/rate_limits__routing.rs` maps to feature "
            "`rate-limits`, not `account-pool`",
            failures,
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

        self.assertIn(
            "codex-rs/feature-specs/account-pool.md test place `agent-e2e` "
            "target `codex-rs/cli/tests/account_pool__routing.rs` must be under "
            "codex-rs/core/tests/suite",
            failures,
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

        self.assertTrue(
            any(
                failure.startswith("codex-rs/feature-specs/account-pool.md:")
                and failure.endswith(
                    "must not include test links; test ownership is derived from filenames"
                )
                for failure in failures
            )
        )

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

        self.assertTrue(
            any(
                "must not include `## E2E Coverage`; use `## Test Places`" in failure
                for failure in failures
            )
        )
        self.assertIn(
            "codex-rs/feature-specs/account-pool.md subfeature `Routing` "
            "contains unexpected `#### E2E Coverage`",
            failures,
        )

    def test_spec_must_not_define_feature_id_field(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            spec = root / "codex-rs/feature-specs/account-pool.md"
            text = spec.read_text(encoding="utf-8")
            spec.write_text(text.replace("## Summary", "Feature ID: account-pool\n\n## Summary"))

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertIn(
            "codex-rs/feature-specs/account-pool.md:3 must not define a Feature ID field",
            failures,
        )

    def test_changed_mapped_test_requires_matching_spec_change(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)

            failures = verify_feature_specs.verify_feature_specs(
                root,
                changed_files=["codex-rs/core/tests/suite/account_pool__missing.rs"],
            )
            self.assertIn(
                "codex-rs/core/tests/suite/account_pool__missing.rs changed without "
                "matching feature spec `codex-rs/feature-specs/account-pool.md`",
                failures,
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

    def test_discovered_mapped_test_must_be_listed_in_feature_spec(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            self.write_file(
                root / "codex-rs/core/tests/suite/account_pool__unlisted.rs",
                "#[test]\nfn unlisted_test() {}\n",
            )

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertIn(
            "codex-rs/core/tests/suite/account_pool__unlisted.rs:unlisted_test maps "
            "to feature `account-pool` and test place `agent-e2e` but is not listed "
            "in `codex-rs/feature-specs/account-pool.md` Test cases",
            failures,
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

    def test_not_covered_place_cannot_have_discovered_mapped_tests(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.write_valid_repo(root)
            self.write_file(
                root / "codex-rs/cli/tests/account_pool__list.rs",
                "#[test]\nfn list_test() {}\n",
            )

            failures = verify_feature_specs.verify_feature_specs(root, changed_files=[])

        self.assertIn(
            "codex-rs/feature-specs/account-pool.md test place `cli` is "
            "`Not covered` but discovered mapped test "
            "`codex-rs/cli/tests/account_pool__list.rs:list_test`",
            failures,
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
