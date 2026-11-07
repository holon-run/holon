#!/usr/bin/env python3
"""Fail-closed public history oracles for the previous-release Docker gate."""

from __future__ import annotations

import copy
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from tests.test_docker_e2e_runner import runner, stub, stub_batch_request


class UpgradeHistoryTests(unittest.TestCase):
    def test_previous_release_completes_five_old_turns_before_upgrade(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            harness = Mock(evidence=Path(temp), image="candidate", previous_image="old")
            harness.agent_id = "main"
            harness.request.return_value = {"receipt": {"created": True}}
            harness.prompt_scope.side_effect = [
                {"turn_id": f"turn-{index}", "message_id": f"msg-{index}"}
                for index in range(5)
            ]
            harness.upgrade_db_snapshot.return_value = {
                "integrity_check": "ok",
                "schema_revision": 25,
                "agent_ids": ["main"],
                "messages": [{"message_id": f"msg-{index}"} for index in range(5)],
                "briefs": [
                    {
                        "evidence_id": f"brief-{index}",
                        "payload_json": json.dumps({"text": f"UPGRADE-V030-OLD-abcd{index:02x}"}),
                    }
                    for index in range(5)
                ],
            }
            harness.restart_with_image.side_effect = RuntimeError("upgrade boundary")
            case = {"id": "runtime-upgrade-v030", "phases": [{"prompt": "{marker}"}]}
            with patch.object(runner.secrets, "token_hex", return_value="abcd"):
                with self.assertRaisesRegex(RuntimeError, "upgrade boundary"):
                    runner.run_runtime_upgrade_previous_release_case(harness, case)
            self.assertEqual(harness.prompt.call_count, 5)
            self.assertEqual(
                [call.args[1] for call in harness.prompt.call_args_list],
                [f"UPGRADE-V030-OLD-abcd{index:02x}" for index in range(5)],
            )
            harness.restart_with_image.assert_called_once_with("candidate")

    def test_required_lifecycle_profile_selects_all_three_strict_stub_cases(self) -> None:
        manifest = json.loads(runner.DEFAULT_MANIFEST.read_text())
        profile = runner.resolve_profile(manifest, "runtime-lifecycle-required")
        self.assertEqual(profile["gate_kind"], "required")
        self.assertEqual(profile["provider_mode"], "stub")
        self.assertEqual(profile["tool_assertion_mode"], "strict")
        expected = {
            "runtime-agent-lifecycle",
            "runtime-shared-credential-delete",
            "runtime-upgrade-v030",
        }
        self.assertEqual(set(profile["case_ids"]), expected)
        self.assertEqual(set(profile["required_coverage_ids"]), expected)
        selected = runner.select_cases(
            manifest, requested=profile["case_ids"], suite="core", tags=[]
        )
        self.assertEqual({case["id"] for case in selected}, expected)
        self.assertTrue(all(case["provider_mode"] == "stub" for case in selected))
        self.assertTrue(next(
            case for case in selected if case["id"] == "runtime-upgrade-v030"
        )["requires_previous_image"])

    def history(self) -> tuple[list[dict], dict[str, dict[str, str]]]:
        expected = {
            f"turn-{index}": {
                "message_id": f"msg-{index}",
                "marker": f"UPGRADE-V030-OLD-{index:08x}",
            }
            for index in range(5)
        }
        turns = [
            {
                "turn_id": turn_id,
                "settled": True,
                "inputs": [{"message_id": item["message_id"]}],
                "brief_ids": [f"brief-{turn_id}"],
            }
            for turn_id, item in reversed(list(expected.items()))
        ]
        pages = [
            {
                "turns": turns[offset:offset + 2],
                "active_turns": [],
                "next_before_cursor": f"cursor/{offset + 2}" if offset < 4 else None,
                "coverage": {
                    "has_older": offset < 4,
                    "reaches_oldest": offset == 4,
                },
            }
            for offset in range(0, 5, 2)
        ]
        return pages, expected

    def read(self, pages: list[dict], expected: dict, *, missing_result: bool = False):
        with tempfile.TemporaryDirectory() as temp:
            harness = Mock(evidence=Path(temp))
            harness.agent_path.side_effect = lambda suffix: f"/api/agents/old/{suffix}"
            harness.request.side_effect = pages
            harness.brief.side_effect = lambda brief_id, label: {
                "text": (
                    "Accepted, not completed." if missing_result
                    else expected[brief_id.removeprefix("brief-")]["marker"]
                )
            }
            report = runner.require_upgrade_history(harness, "history", expected)
            self.assertEqual(harness.request.call_count, 3)
            self.assertIn(
                "before=cursor%2F2", harness.request.call_args_list[1].args[1]
            )
            self.assertEqual(harness.brief.call_count, 5)
            self.assertEqual(len(list(Path(temp).glob("history-page-*.json"))), 3)
            return report

    def test_three_pages_resolve_all_old_inputs_and_results(self) -> None:
        pages, expected = self.history()
        report = self.read(pages, expected)
        self.assertEqual(report["pages"], 3)
        self.assertTrue(report["reaches_oldest"])
        self.assertEqual(set(report["old_turns"]), set(expected))

    def test_cold_read_preserves_old_result_identity(self) -> None:
        pages, expected = self.history()
        warm = self.read(pages, expected)
        cold = self.read(copy.deepcopy(pages), expected)
        self.assertEqual(warm["old_turns"], cold["old_turns"])

    def test_repeated_or_missing_cursor_is_rejected(self) -> None:
        for cursor in ("cursor/2", None, ""):
            with self.subTest(cursor=cursor):
                pages, expected = self.history()
                pages[1]["next_before_cursor"] = cursor
                with self.assertRaisesRegex(AssertionError, "history cursor"):
                    self.read(pages, expected)

    def test_missing_page_or_early_oldest_is_rejected(self) -> None:
        pages, expected = self.history()
        pages[1] = pages[2]
        with self.assertRaisesRegex(AssertionError, "omitted old history"):
            self.read(pages, expected)

    def test_duplicate_turn_message_or_brief_is_rejected(self) -> None:
        for field in ("turn_id", "inputs", "brief_ids"):
            with self.subTest(field=field):
                pages, expected = self.history()
                pages[1]["turns"][0][field] = copy.deepcopy(pages[0]["turns"][0][field])
                with self.assertRaisesRegex(AssertionError, "repeated"):
                    self.read(pages, expected)

    def test_accepted_without_durable_result_is_rejected(self) -> None:
        pages, expected = self.history()
        with self.assertRaisesRegex(AssertionError, "cannot read old Result"):
            self.read(pages, expected, missing_result=True)

    def test_missing_input_brief_or_settlement_is_rejected(self) -> None:
        for field, value in (("inputs", []), ("brief_ids", []), ("settled", False)):
            with self.subTest(field=field):
                pages, expected = self.history()
                pages[0]["turns"][0][field] = value
                with self.assertRaisesRegex(AssertionError, "lost a settled old input"):
                    self.read(pages, expected)

    def test_history_marker_cannot_consume_a_new_stub_phase(self) -> None:
        scenario = stub.Scenario("runtime-upgrade-v030")
        status, _ = scenario.consume(stub_batch_request(
            ({}, "Unrelated current input."),
            recent_turns="UPGRADE-V030-OLD-0011aabb",
        ))
        self.assertEqual(status, 409)
        self.assertEqual(scenario.phase, 0)
        self.assertEqual(scenario.status()["extra_requests"], 1)

    def test_repeated_marker_is_not_an_additional_old_turn(self) -> None:
        scenario = stub.Scenario("runtime-upgrade-v030")
        request = stub_batch_request(({}, "UPGRADE-V030-OLD-0011aabb"))
        self.assertEqual(scenario.consume(request)[0], 200)
        self.assertEqual(scenario.consume(request)[0], 409)
        self.assertEqual(scenario.phase, 1)
        self.assertEqual(scenario.status()["extra_requests"], 1)


if __name__ == "__main__":
    unittest.main()
