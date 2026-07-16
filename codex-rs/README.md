# SpineCodex CLI (Rust implementation)

This workspace contains the native implementation distributed by the
independent `@spinejit/spine-codex` package.

```bash
npm install -g @spinejit/spine-codex
spine-codex
```

SpineCodex is derived from the open-source
[OpenAI Codex](https://github.com/openai/codex) project and adds Spine context
management. It is not the official OpenAI Codex CLI. Product releases and
issues are maintained in the
[SpineCodex repository](https://github.com/GhabiX/SpineCodex); upstream CLI
documentation remains available from
[OpenAI](https://developers.openai.com/codex/cli).

The npm entrypoints are `spine-codex` and the compatibility alias
`spinecodex`. The native workspace binary remains named `codex` as an internal
compatibility boundary.
