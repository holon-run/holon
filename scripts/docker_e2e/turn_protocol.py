"""Canonical model Turn evidence in runtime database snapshots."""

from __future__ import annotations

from typing import Any


def agent_binding(
    agent_id: str, *, work_item_id: str | None = None, interaction_id: str | None = None
) -> dict[str, Any]:
    attribution = {
        key: value for key, value in (
            ("work_item_id", work_item_id), ("interaction_id", interaction_id)
        ) if value is not None
    }
    return {"kind": "agent", "agent_id": agent_id, "attribution": attribution}


def agent_attribution(binding: dict[str, Any], agent_id: str) -> dict[str, str] | None:
    attribution = binding.get("attribution")
    if (
        isinstance(attribution, dict)
        and attribution.keys() <= {"work_item_id", "interaction_id"}
        and all(isinstance(value, str) and value for value in attribution.values())
        and binding == agent_binding(agent_id, **attribution)
    ):
        return attribution
    return None


def turn_settlement(
    outcome: dict[str, Any], effect: dict[str, Any] | None = None
) -> dict[str, Any]:
    return {
        "owner": "turn",
        "outcome": {"outcome": outcome, "work_item_effect": effect},
    }
