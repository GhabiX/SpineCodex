<p align="center"><code>npm install -g @spinejit/spine-codex</code></p>
<p align="center"><strong>SpineCodex</strong> is an independently maintained, local coding agent based on OpenAI Codex, with Spine context management.</p>
<p align="center">
  <img src="https://github.com/GhabiX/SpineCodex/blob/main/.github/codex-cli-splash.png" alt="SpineCodex CLI splash" width="80%" />
</p>

---

## Quickstart

Install SpineCodex globally with npm:

```bash
npm install -g @spinejit/spine-codex
spine-codex
```

The compatibility command `spinecodex` invokes the same CLI. SpineCodex does
not install a `codex` command, so it can coexist with the official
`@openai/codex` npm package.

SpineJIT is enabled by default. For one run, disable it with
`spine-codex --disable spine_jit`. To disable it persistently, set:

```toml
[features]
spine_jit = false
```

Release artifacts are published from the
[SpineCodex releases page](https://github.com/GhabiX/SpineCodex/releases).

## Project identity and attribution

SpineCodex is an independent fork and derivative of the open-source
[OpenAI Codex](https://github.com/openai/codex) repository. It adds Spine
context management and is not the official OpenAI Codex CLI or the official
`@openai/codex` npm package. The current product line began from OpenAI Codex
`release/0.144` at commit
`d82b7e5d4c1c274bee0eb55f92ec12d017e78634`.

Report SpineCodex issues in the
[fork issue tracker](https://github.com/GhabiX/SpineCodex/issues). For upstream
Codex documentation and behavior unrelated to Spine, refer to the
[OpenAI Codex repository](https://github.com/openai/codex).

## Docs

- [**SpineCodex source**](https://github.com/GhabiX/SpineCodex)
- [**Upstream Codex documentation**](https://developers.openai.com/codex)
- [**Contributing**](./docs/contributing.md)
- [**Installing & building**](./docs/install.md)

SpineCodex is licensed under the [Apache-2.0 License](LICENSE). OpenAI Codex
and other derived components retain their attribution in [NOTICE](NOTICE).
