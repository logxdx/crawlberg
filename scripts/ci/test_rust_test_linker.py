"""Keep the macOS test-only unwind notice exemption narrower than the warning gate."""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/ci/rust/run-unit-tests.sh"
NOTICE_FLAG = "-C link-arg=-Wl,-no_warn_eh_frame_too_large"
BASE_FLAGS = "-D warnings -C link-arg=-undefined -C link-arg=dynamic_lookup"
CARGO_STUB = r"""#!/usr/bin/env python3
import json
import os
import pathlib
import sys
path = pathlib.Path(os.environ["CALL_LOG"])
rows = path.read_text().splitlines() if path.exists() else []
with path.open("a") as output:
    output.write(json.dumps({
        "args": sys.argv[1:],
        "rustflags": os.environ.get("RUSTFLAGS", ""),
        "warnings": os.environ.get("CARGO_BUILD_WARNINGS"),
    }) + "\n")
sys.exit(37 if len(rows) + 1 == int(os.environ["FAIL_CALL"]) else 0)
"""


class RustTestLinkerTests(unittest.TestCase):
    def run_case(self, platform: str, fail_call: int = 0) -> tuple[subprocess.CompletedProcess[str], list[dict]]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cargo = root / "cargo"
            cargo.write_text(CARGO_STUB)
            cargo.chmod(0o755)
            uname = root / "uname"
            uname.write_text(f"#!/bin/sh\nprintf '%s\\n' '{platform}'\n")
            uname.chmod(0o755)
            log = root / "calls.jsonl"
            env = dict(
                os.environ,
                PATH=f"{root}:{os.environ['PATH']}",
                REPO_ROOT=str(ROOT),
                CALL_LOG=str(log),
                FAIL_CALL=str(fail_call),
                RUSTFLAGS=BASE_FLAGS,
                CARGO_BUILD_WARNINGS="deny",
            )
            result = subprocess.run(["bash", str(SCRIPT)], env=env, capture_output=True, text=True, check=False)
            rows = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
            assert len(rows) == 3, result.stdout + result.stderr
            assert rows[0]["args"][:3] == ["test", "-p", "crawlberg"]
            assert rows[1]["args"][:2] == ["test", "--workspace"]
            assert rows[2]["args"][:3] == ["test", "-p", "crawlberg-cli"]
            return result, rows

    def test_darwin_core_tests_suppress_only_the_unwind_size_notice(self) -> None:
        result, rows = self.run_case("Darwin")
        assert result.returncode == 0, result.stdout + result.stderr
        assert [row["rustflags"] for row in rows] == [f"{BASE_FLAGS} {NOTICE_FLAG}", BASE_FLAGS, BASE_FLAGS]
        assert [row["warnings"] for row in rows] == ["deny", "deny", "deny"]

    def test_linux_keeps_all_compiler_and_linker_warning_flags(self) -> None:
        result, rows = self.run_case("Linux")
        assert result.returncode == 0, result.stdout + result.stderr
        assert [row["rustflags"] for row in rows] == [BASE_FLAGS, BASE_FLAGS, BASE_FLAGS]
        assert [row["warnings"] for row in rows] == ["deny", "deny", "deny"]

    def test_any_cargo_failure_still_fails_after_all_three_invocations(self) -> None:
        for platform in ("Darwin", "Linux"):
            for fail_call in (1, 2, 3):
                with self.subTest(platform=platform, fail_call=fail_call):
                    result, rows = self.run_case(platform, fail_call)
                    assert result.returncode != 0
                    assert len(rows) == 3
                    assert "=== Test execution failed ===" in result.stdout


if __name__ == "__main__":
    unittest.main()
