# GHX Context Collection

Collect GitHub context into `${GITHUB_CONTEXT_DIR}` (default `${GITHUB_OUTPUT_DIR}/github-context`).

## Command

```bash
scripts/ghx.sh context collect <ref> [repo_hint]
```

`ref` supports:
- `OWNER/REPO#NUMBER`
- GitHub issue/PR URL
- numeric ID with `repo_hint`

## Output layout

- `${GITHUB_CONTEXT_DIR}/manifest.json`
- `${GITHUB_CONTEXT_DIR}/github/*` artifacts referenced by the manifest

## Manifest contract

`manifest.json` is the machine-readable contract for collected context.

Key fields:
- `schema_version`: currently `2.0`
- `kind`: `pr` or `issue`
- `ref`: normalized reference (`owner/repo#number`)
- `success`: overall collection result
- `artifacts[]`: each artifact's `id`, `path`, `status`, `format`, `description`, `required_for`
- `notes[]`: skip/failure diagnostics

Artifact `status` meanings:
- `present`: file exists and was collected successfully
- `missing`: intentionally skipped or unavailable
- `error`: requested but collection failed

Consumers should read `artifacts[]` and not assume fixed filenames.

## Typical artifact ids

- PR: `pr_metadata`, `files`, `diff`, `review_threads`, `comments`, `check_runs`, `commits`
- Issue: `issue_metadata`, `comments`
