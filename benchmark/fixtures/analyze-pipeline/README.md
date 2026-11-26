# Analyze pipeline

`analyze.py` reads `data/records.jsonl` and writes `out/report.json`.

Report rules (normative):

- Consider only records with `status == "active"`; `archived` and
  `pending` records are excluded everywhere below.
- `counts`: number of active records per `category`, as an object.
- `top`: the three active records with the highest `score`, sorted by
  score descending; ties are broken by `id` ascending. Each entry is an
  object with exactly the keys `id` and `score`.
- `avg_score`: the mean `score` over all active records, rounded half up
  to two decimals (e.g. 85.125 -> 85.13).

The current `out/report.json` does not conform to these rules. Diagnose
the divergence and fix `analyze.py`. Do not modify `data/` or this
README; both are hash-verified after the run.

Run the pipeline with:

```
python3 analyze.py
```
