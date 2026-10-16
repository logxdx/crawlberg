"""Keep terminal color escapes out of published native linker metadata."""

from __future__ import annotations

import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]


class NativeLinkColorTests(unittest.TestCase):
    def test_native_archive_producers_disable_cargo_color(self) -> None:
        workflow = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())
        producers = {
            "go-ffi-libraries": "xberg-io/actions/build-go-ffi@",
            "c-ffi-libraries": "xberg-io/actions/build-rust-ffi@",
        }
        for job_name, action in producers.items():
            with self.subTest(job=job_name):
                job = workflow["jobs"][job_name]
                steps = [step for step in job["steps"] if step.get("uses", "").startswith(action)]
                assert len(steps) == 1
                producer = steps[0]
                assert producer.get("env", {}).get("CARGO_TERM_COLOR") == "never"
                for scope in (workflow, job, producer):
                    env = scope.get("env", {})
                    assert env.get("CARGO_BUILD_WARNINGS", "deny") == "deny"
                    assert "RUSTFLAGS" not in env
                    assert "CARGO_ENCODED_RUSTFLAGS" not in env


if __name__ == "__main__":
    unittest.main()
