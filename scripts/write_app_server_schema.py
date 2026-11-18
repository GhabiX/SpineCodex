#!/usr/bin/env python3
"""Regenerate app-server fixtures through the protocol crate's test-only exporter."""

import argparse
import os
from pathlib import Path
import subprocess


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--experimental", action="store_true")
    parser.add_argument("--prettier", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    env = os.environ.copy()
    env["CODEX_APP_SERVER_SCHEMA_ROOT"] = str(
        root / "codex-rs/app-server-protocol/schema"
    )
    env["CODEX_APP_SERVER_SCHEMA_EXPERIMENTAL"] = "1" if args.experimental else "0"
    if args.prettier is not None:
        env["CODEX_APP_SERVER_SCHEMA_PRETTIER"] = str(args.prettier.resolve())
    subprocess.run(
        [
            "just",
            "test",
            "-p",
            "codex-app-server-protocol",
            "--lib",
            "--run-ignored",
            "only",
            "-E",
            "test(=schema_fixtures_tests::write_schema_fixtures_from_env)",
        ],
        cwd=root / "codex-rs",
        env=env,
        check=True,
    )


if __name__ == "__main__":
    main()
