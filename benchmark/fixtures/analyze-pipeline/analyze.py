#!/usr/bin/env python3
import json
from pathlib import Path


def load_records(path):
    records = []
    for line in Path(path).read_text().splitlines():
        if line.strip():
            records.append(json.loads(line))
    return records


def build_report(records):
    active = [r for r in records if r["status"] != "archived"]

    counts = {}
    for record in active:
        counts[record["category"]] = counts.get(record["category"], 0) + 1

    top = sorted(active, key=lambda r: r["score"])[:3]

    average = round(sum(r["score"] for r in active) / len(active), 2)

    return {
        "counts": counts,
        "top": [{"id": r["id"], "score": r["score"]} for r in top],
        "avg_score": average,
    }


if __name__ == "__main__":
    records = load_records("data/records.jsonl")
    report = build_report(records)
    out_dir = Path("out")
    out_dir.mkdir(exist_ok=True)
    (out_dir / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print("wrote out/report.json")
