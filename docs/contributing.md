## Contributing

**External contributions are by invitation only**

At this time, the Codex team does not accept unsolicited code contributions.

If you would like to propose a new feature or a change in behavior, please open an issue describing the proposal or upvote an existing enhancement request. We prioritize new features based on community feedback, alignment with our roadmap, and consistency across all Codex surfaces (CLI, IDE extensions, web, etc.).

If you encounter a bug, please open a bug report or verify that an existing report already covers the issue. If you would like to help, we encourage you to contribute by sharing analysis, reproduction details, root-cause hypotheses, or a high-level outline of a potential fix directly in the issue thread.

The Codex team may invite an external contributor to submit a pull request when:

- the problem is well understood,
- the proposed approach aligns with the team’s intended solution, and
- the issue is deemed high-impact and high-priority.

Pull requests that have not been explicitly invited by a member of the Codex team will be closed without review.

**Why we do not generally accept external code contributions**

In the past, the Codex team accepted external pull requests for bug fixes. While we appreciated the effort and engagement from the community, this model did not scale well.

Many contributions were made without full visibility into the architectural context, system-level constraints, or near-term roadmap considerations that guide Codex development. Others focused on issues that were low priority or affected a very small subset of users. Reviewing and iterating on these PRs often took more time than implementing the fix directly, and diverted attention from higher-priority work.

The most valuable contributions consistently came from community members who demonstrated deep understanding of a problem domain. That expertise is most helpful when shared early -- through detailed bug reports, analysis, and design discussion in issues. Identifying the right solution is typically the hard part; implementing it is comparatively straightforward with the help of Codex itself.

For these reasons, we focus external contributions on discussion, analysis, and feedback, and reserve code changes for cases where a targeted invitation makes sense.

### Development workflow

If you are invited by a Codex team member to contribute a PR, keep the loop tight:

- Create a _topic branch_ from `main` - e.g. `feat/interactive-prompt`.
- Keep each branch focused on one logical change.
- Update the code, tests, docs, and any generated artifacts that the change touches in the same commit series.
- Run the local checks before you commit: `just fmt`, `just fix -p <crate>`, and the relevant tests (`cargo test -p ...` or `just test` when you need a broader sweep). Regenerate any derived schemas or lockfiles before rerunning the affected checks.
- Keep commits atomic. Each commit should build, stay lint-clean, and pass its tests.

### Guidance for invited code contributions

1. **Start with an issue.** Open a new one or comment on an existing discussion so we can agree on the solution before code is written.
2. **Add or update tests.** A bug fix should generally come with test coverage that fails before your change and passes afterwards. 100% coverage is not required, but aim for meaningful assertions.
3. **Document behavior.** If your change affects user-facing behavior, update the README, inline help (`codex --help`), or relevant example projects.

### Model metadata updates

When a change updates model catalogs or model metadata (`/models` payloads, presets, or fixtures):

- Set `input_modalities` explicitly for any model that does not support images.
- Keep compatibility defaults in mind: omitted `input_modalities` currently implies text + image support.
- Ensure client surfaces that accept images (for example, TUI paste/attach) consume the same capability signal.
- Add/update tests that cover unsupported-image behavior and warning paths.

### Opening a pull request (by invitation only)

- Fill in the PR template (or include similar information) - **What? Why? How?**
- Include a link to a bug report or enhancement request in the issue tracker
- Make sure your branch is up to date with `main`, has no merge conflicts, and still passes the local checks above.
- Mark the PR as **Ready for review** only when you believe it is in a merge-able state.

### Review process

1. One maintainer will be assigned as a primary reviewer.
2. If your invited PR introduces scope or behavior that was not previously discussed and approved, we may close the PR.
3. We may ask for changes. Please do not take this personally. We value the work, but we also value consistency and long-term maintainability.
4. When there is consensus that the PR meets the bar, a maintainer will squash-and-merge.

### Community values

- **Be kind and inclusive.** Treat others with respect; we follow the [Contributor Covenant](https://www.contributor-covenant.org/).
- **Assume good intent.** Written communication is hard - err on the side of generosity.
- **Teach & learn.** If you spot something confusing, open an issue or discussion with suggestions or clarifications.

### Getting help

If you run into problems setting up the project, would like feedback on an idea, or just want to say _hi_ - please open a Discussion topic or jump into the relevant issue. We are happy to help.

Together we can make Codex CLI an incredible tool. **Happy hacking!** :rocket:

### Contributor license agreement (CLA)

All contributors **must** accept the CLA. The process is lightweight:

1. Open your pull request.
2. Paste the following comment (or reply `recheck` if you've signed before):

   ```text
   I have read the CLA Document and I hereby sign the CLA
   ```

3. The CLA-Assistant bot records your signature in the repo and marks the status check as passed.

No special Git commands, email attachments, or commit footers required.

### Security & responsible AI

Have you discovered a vulnerability or have concerns about model output? Please e-mail **security@openai.com** and we will respond promptly.
