#!/usr/bin/env python3
"""Exercise npm staging without publishing or cleaning the retained fixtures."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class SpineReleaseBehaviorTest(unittest.TestCase):
    def test_staged_root_metadata_is_plain_and_discoverable(self) -> None:
        output_root = ROOT / "temp" / "null" / "npm-metadata"
        output_root.mkdir(parents=True, exist_ok=True)
        stage = Path(tempfile.mkdtemp(dir=output_root)) / "stage"
        subprocess.run(
            [
                sys.executable,
                str(ROOT / "codex-cli/scripts/build_npm_package.py"),
                "--package",
                "codex",
                "--version",
                "0.4.0",
                "--staging-dir",
                str(stage),
            ],
            check=True,
        )
        package = json.loads((stage / "package.json").read_text())
        self.assertEqual(package["name"], "@spinejit/spine-codex")
        self.assertEqual(package["version"], "0.4.0")
        self.assertEqual(package["bin"], {"spine-codex": "bin/codex.js"})
        self.assertTrue((stage / package["bin"]["spine-codex"]).is_file())
        self.assertTrue(package["keywords"])
        self.assertTrue(
            all(isinstance(word, str) and word.strip() for word in package["keywords"])
        )
        description = package["description"]
        self.assertTrue(description.strip())
        self.assertNotRegex(description, r"<[^>]*>")
        self.assertNotIn("\n", description)
        self.assertEqual(len(package["optionalDependencies"]), 6)
        self.assertTrue(
            all(
                value.startswith("npm:@spinejit/spine-codex@0.4.0-")
                for value in package["optionalDependencies"].values()
            )
        )


if __name__ == "__main__":
    unittest.main()
