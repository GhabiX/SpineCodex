# Contributing to SpineCodex

Thanks for your interest in SpineCodex! This project is an enhanced, independently maintained version of [OpenAI Codex](https://github.com/openai/codex). We welcome contributions that improve long-horizon coding agent performance, context management, and overall reliability.

## Table of contents

- [Code of conduct](#code-of-conduct)
- [Getting started](#getting-started)
- [Development setup](#development-setup)
- [Building and testing](#building-and-testing)
- [Code style](#code-style)
- [How to contribute](#how-to-contribute)
  - [Reporting bugs](#reporting-bugs)
  - [Proposing features](#proposing-features)
  - [Submitting pull requests](#submitting-pull-requests)
- [Compatibility with upstream Codex](#compatibility-with-upstream-codex)

## Code of conduct

Be respectful, constructive, and inclusive. This project is maintained in the open — every contributor is a volunteer. Harassment or hostile behavior is not tolerated.

## Getting started

The Rust codebase lives in `codex-rs/`. The repository uses both [Bazel](https://bazel.build) (primary) and Cargo (workspace under `codex-rs/`).

To run the CLI locally:

```bash
npm install -g @spinejit/spine-codex@latest
spine-codex
```

For development, build from source:

```bash
cd codex-rs
cargo build          # or: cargo build --release
```

## Building and testing

```bash
cd codex-rs
just fmt             # format (run after every change)
just test            # full test suite (respects repo defaults)
just test -p codex-config   # test a single crate
just fix -p codex-config    # lint + fix a single crate
```

Notes:

- Do **not** run `cargo test` directly — use `just test` so tests follow the repo defaults.
- After changing Rust dependencies, run `just bazel-lock-update` from the repo root and commit the lockfile update — CI verifies lockfile drift.
- If you change `ConfigToml` or nested config types, run `just write-config-schema` to regenerate `codex-rs/core/config.schema.json` and commit it.

## Code style

The repo's `AGENTS.md` (in `codex-rs/`) is authoritative. Highlights:

- Collapse `if` statements where clippy suggests (`collapsible_if`).
- Inline `format!` args where possible (`uninlined_format_args`).
- Prefer method references over closures (`redundant_closure_for_method_calls`).
- Make `match` statements exhaustive; avoid wildcard arms.
- Avoid small helper methods referenced only once.
- Keep modules focused (target < 500 LoC excluding tests).
- Use `/*param_name*/` comments before opaque positional literals (`None`, booleans, numbers).

## How to contribute

### Reporting bugs

Open an issue with:

1. **Environment**: SpineCodex version, upstream Codex baseline version (if relevant), OS/arch.
2. **Minimal reproduction**: a small config snippet or command sequence that triggers the bug.
3. **Expected vs actual** behavior.

Config-loading bugs are especially valuable — upstream Codex evolves quickly, and inherited user configs must keep working.

### Proposing features

Open an issue describing the motivation, the user-visible behavior you want, and — if applicable — how it fits the existing architecture (SpineTree, SpineJIT, recursive subagents).

### Submitting pull requests

1. Fork the repository and create a feature branch (`git checkout -b fix/your-fix`).
2. Make your change. Run `just fmt` (in `codex-rs/`) after editing.
3. Run tests for the crate you changed: `just test -p <crate>`.
4. Update `config.schema.json` if you touched config types; update `Cargo.lock`-adjacent lockfiles if you touched dependencies.
5. Open the PR against `main`. Reference the issue it fixes (e.g. `Fixes #6`).
6. CI runs the full test suite on Linux/macOS/Windows — a green CI check is required for merge.

## Compatibility with upstream Codex

SpineCodex inherits upstream Codex user configurations. When upstream adds or renames config fields, SpineCodex must keep accepting them. If you change config parsing, add a regression test that deserializes the upstream-shaped TOML — that is the contract that protects existing users.

---

Thanks for contributing to making long-horizon coding agents better. 🌲
