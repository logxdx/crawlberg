"""Ensure release rehearsals cannot move tags or bypass the real-tag guard."""

from __future__ import annotations

import os
import re
import subprocess
import unittest
from pathlib import Path

import yaml
from test_plugin_dispatch import evaluate_condition

ROOT = Path(__file__).resolve().parents[2]


def release_condition(expression: str, values: dict[str, bool | str | None]) -> bool:
    source = expression.replace("!cancelled()", "true")
    for name, value in values.items():
        source = source.replace(name, repr(value))
    source = re.sub(r"(?<!['\"])\b(true|false)\b(?!['\"])", lambda match: match[0].title(), source)
    return evaluate_condition(source, event="workflow_dispatch", plugins_only=False, ref="refs/heads/main")


class ReleaseDryRunTests(unittest.TestCase):
    def setUp(self) -> None:
        self.jobs = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())["jobs"]

    def test_go_subdirectory_tags_require_a_real_release(self) -> None:
        for dry_run, is_tag, expected in (
            (True, True, False),
            (True, False, False),
            (False, True, True),
            (False, False, False),
        ):
            with self.subTest(dry_run=dry_run, is_tag=is_tag):
                values = {
                    "needs.prepare.outputs.release_go": "true",
                    "needs.prepare.outputs.is_tag": str(is_tag).lower(),
                    "needs.prepare.outputs.dry_run": str(dry_run).lower(),
                    "needs.validate-versions.result": "success",
                    "needs.upload-go-release.result": "success",
                    "needs.publish-swift-checksums.result": "skipped",
                }
                assert release_condition(self.jobs["push-go-subdir-tag"]["if"], values) is expected

    def check_republish_mode(self, channel: str) -> None:
        retag = next(step for step in self.jobs["prepare"]["steps"] if step.get("name") == "Retag for republish")
        for republish, dry_run, expected in (
            (True, True, False),
            (True, "true", False),
            (True, False, True),
            (True, "false", True),
            (True, None, True),
            (False, True, False),
            (False, False, False),
        ):
            with self.subTest(channel=channel, republish=republish, dry_run=dry_run):
                values = {
                    "inputs.republish": False,
                    "inputs.dry_run": False,
                    "github.event.client_payload.republish": False,
                    "github.event.client_payload.dry_run": False,
                }
                values[f"{channel}.republish"] = republish
                values[f"{channel}.dry_run"] = dry_run
                assert release_condition(retag["if"], values) is expected

    def test_manual_dry_run_cannot_retag_for_republish(self) -> None:
        self.check_republish_mode("inputs")

    def test_repository_dispatch_dry_run_cannot_retag_for_republish(self) -> None:
        self.check_republish_mode("github.event.client_payload")

    def test_real_tag_guard_still_rejects_non_tags_including_dry_runs(self) -> None:
        guard = next(
            step for step in self.jobs["prepare"]["steps"] if step.get("name") == "Assert the run can publish at all"
        )
        for dry_run in (False, True):
            for is_tag in (False, True):
                with self.subTest(dry_run=dry_run, is_tag=is_tag):
                    result = subprocess.run(
                        ["bash", "-c", guard["run"]],
                        cwd=ROOT,
                        env={
                            **os.environ,
                            "IS_TAG": str(is_tag).lower(),
                            "DRY_RUN": str(dry_run).lower(),
                            "RESOLVED_REF": "refs/tags/v1.11.2" if is_tag else "refs/heads/main",
                            "TARGETS": "all",
                        },
                        capture_output=True,
                        text=True,
                        check=False,
                    )
                    assert result.returncode == (0 if is_tag else 1), result.stderr
                    if not is_tag:
                        assert "is not a tag" in result.stderr


if __name__ == "__main__":
    unittest.main()
