<p align="center"><strong>Codex CLI</strong> is a coding agent from OpenAI that runs locally on your computer.
<p align="center">
  <img src="https://github.com/openai/codex/blob/main/.github/codex-cli-splash.png" alt="Codex CLI splash" width="80%" />
</p>
</br>
If you want Codex in your code editor (VS Code, Cursor, Windsurf), <a href="https://developers.openai.com/codex/ide">install in your IDE.</a>
</br>If you want the desktop app experience, run <code>codex app</code> or visit <a href="https://chatgpt.com/codex?app-landing-page=true">the Codex App page</a>.
</br>If you are looking for the <em>cloud-based agent</em> from OpenAI, <strong>Codex Web</strong>, go to <a href="https://chatgpt.com/codex">chatgpt.com/codex</a>.</p>

---

## Quickstart

### Installing and running Codex CLI

Run the following on Mac or Linux to install Codex CLI:

```shell
curl -fsSL https://github.com/dkropachev/codex/releases/latest/download/install.sh | sh
```

Fork-managed installs start with version `0.150.0` and support macOS and Linux
on x64 and arm64. Windows and package-manager installs are not distributed or
self-updated by this fork.

Then simply run `codex` to get started.

<details>
<summary>You can also go to the <a href="https://github.com/dkropachev/codex/releases/latest">latest GitHub Release</a> and download the package for your platform.</summary>

Managed GitHub Releases provide one package for each supported target:

- macOS
  - Apple Silicon/arm64: `codex-package-aarch64-apple-darwin.tar.gz`
  - x86_64 (older Mac hardware): `codex-package-x86_64-apple-darwin.tar.gz`
- Linux
  - x86_64: `codex-package-x86_64-unknown-linux-musl.tar.gz`
  - arm64: `codex-package-aarch64-unknown-linux-musl.tar.gz`

Each archive contains the canonical standalone package layout, including the
Codex executable and its bundled runtime helpers.

</details>

### Using Codex with your ChatGPT plan

Run `codex` and select **Sign in with ChatGPT**. We recommend signing into your ChatGPT account to use Codex as part of your Plus, Pro, Business, Edu, or Enterprise plan. [Learn more about what's included in your ChatGPT plan](https://help.openai.com/en/articles/11369540-codex-in-chatgpt).

You can also use Codex with an API key, but this requires [additional setup](https://developers.openai.com/codex/auth#sign-in-with-an-api-key).

## Docs

- [**Codex Documentation**](https://developers.openai.com/codex)
- [**Contributing**](./docs/contributing.md)
- [**Installing & building**](./docs/install.md)
- [**Open source fund**](./docs/open-source-fund.md)

This repository is licensed under the [Apache-2.0 License](LICENSE).
