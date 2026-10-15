# SpineCodex 0.4.1

Hotfix on 0.4.0 for the GPT-6 models. The code baseline stays upstream Codex
`0.153.4` (`rust-v0.153.4`, commit `3d2ee51ca2d5db578f328aa75e20aa22c0197c9a`).

- Add the GPT-6 model metadata from upstream Codex `0.162.1`: `gpt-6.1-sol`, `gpt-6-sol` and `gpt-6-luna`, and update `gpt-6-astra` to the same source. Without these entries the client used generic fallback metadata for these models. Fields that this baseline does not read are omitted.
- Raise the upstream HTTP compatibility identity from `0.153.4` to `0.160.0`. With a ChatGPT account the server lists models by client version: at `0.153.4` none of the GPT-6 models are listed and requests fail with 400; `0.160.0` is the first upstream release that ships `gpt-6.1-sol`. The public CLI version now reports `0.160.0`.

The product/package version is `0.4.1`.
