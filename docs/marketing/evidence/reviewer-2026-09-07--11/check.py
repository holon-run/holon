"""Recompute the published review-window totals from sanitized evidence."""

import csv
import json
from collections import Counter
from pathlib import Path


def main():
    root = Path(__file__).resolve().parent
    data = json.loads((root / "audit.json").read_text())
    start = data["window"]["start_inclusive"]
    end = data["window"]["end_exclusive"]
    account = data["public_activity_account"]
    prs = data["prs"]
    assert len(prs) == len({pr["pr"] for pr in prs}) == 61
    assert len(data["local_actions"]) == 213

    def in_window(at):
        return at is not None and start <= at < end

    def count(events):
        return sum(e["account"] == account and in_window(e["at"]) for e in events)

    candidates = {
        row["pr"]
        for row in data["local_summary"]
        if in_window(min(filter(None, [row["first_review"], row["first_comment"]]), default=None))
    }
    assert candidates == {pr["pr"] for pr in prs}
    assert all(in_window(pr["local_first_attempt_at"]) for pr in prs)
    assert all(in_window(pr["nearest_public_event"]["at"]) for pr in prs)
    assert all(pr["state"] == "MERGED" and in_window(pr["merged_at"]) for pr in prs)
    assert sum(count(pr["reviews"]) for pr in prs) == 74
    assert sum(count(pr["comments"]) for pr in prs) == 42
    assert Counter(pr["local_first_attempt_kind"] for pr in prs) == {"review": 60, "comment": 1}
    assert Counter(
        action["kind"] for action in data["local_actions"] if in_window(action["at"])
    ) == {"review": 69, "comment": 16, "merge": 66}
    for pr in prs:
        for key in ("reviews", "comments"):
            assert len(pr[key]) == len({event["id"] for event in pr[key]})
            assert all(event["url"].startswith(pr["url"] + "#") for event in pr[key])

    with (root / "prs.csv").open(newline="") as stream:
        rows = list(csv.DictReader(stream))
    assert len(rows) == len(prs)
    for row, pr in zip(rows, prs):
        assert int(row["pr"]) == pr["pr"]
        assert row["local_first_attempt_at"] == pr["local_first_attempt_at"]
        assert row["local_first_attempt_kind"] == pr["local_first_attempt_kind"]
        assert row["nearest_public_event_at"] == pr["nearest_public_event"]["at"]
        assert row["nearest_public_event_url"] == pr["nearest_public_event"]["url"]
        assert int(row["account_reviews_in_window"]) == count(pr["reviews"])
        assert int(row["account_comments_in_window"]) == count(pr["comments"])
        assert row["state"] == pr["state"]
        assert row["merged_at"] == pr["merged_at"]
        assert row["outcome_classification"] == "未分类"
    print("通过：61 个 PR，61 个窗口内合并；共享账号 74 次 review、42 条评论；CSV 与证据一致。")


if __name__ == "__main__":
    main()
