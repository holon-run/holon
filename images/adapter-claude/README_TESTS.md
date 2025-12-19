# Python Adapter Unit Tests

This document describes the unit tests for `images/adapter-claude/adapter.py`.

## Overview

The unit tests cover all the required functionality specified in the issue:

- **ProgressLogger functionality**:
  - Log level gating (debug/info/progress/minimal)
  - `_safe_filename` only returns basename
  - `log_tool_use` does not leak paths/content

- **Diff generation**:
  - Command includes `--binary --full-index` flags for `git apply` compatibility

- **Artifact generation**:
  - Writes `manifest.json`, `diff.patch`, `summary.md`
  - Summary fallback path works when agent didn't write `summary.md`

## Test Files

- `test_adapter.py` - Full pytest-based test suite (requires pytest installation)
- `run_tests.py` - Standalone test runner (no dependencies)

## Running Tests

### Option 1: Standalone Test Runner (Recommended)

The standalone test runner works without any external dependencies:

```bash
cd images/adapter-claude
python3 run_tests.py
```

### Option 2: Using pytest

If you have pytest installed, you can run the full test suite:

```bash
cd images/adapter-claude
pip install pytest  # or use system package manager
python -m pytest test_adapter.py -v
```

## Test Coverage

### ProgressLogger Tests

1. **Log Level Initialization**: Tests all valid log levels (debug, info, progress, minimal)
2. **Level Gating**: Verifies that log levels properly filter messages:
   - DEBUG: Shows all messages
   - INFO: Shows info, progress, minimal (hides debug)
   - PROGRESS: Shows progress, minimal (hides debug, info)
   - MINIMAL: Shows only minimal messages

3. **Safe Filename**: Tests that `_safe_filename` only returns basename:
   - `/full/path/to/file.txt` → `file.txt`
   - `relative/path/file.py` → `file.py`
   - Handles edge cases (empty string, None)

4. **Tool Use Safety**: Tests that `log_tool_use` doesn't leak sensitive information:
   - Only shows basenames, not full paths
   - Limits file display when there are many files (> 3)
   - Increments tool use counter correctly

### Diff Generation Tests

1. **Binary Support**: Verifies the git diff command includes `--binary --full-index` flags
2. **Compatibility**: Ensures patches can be applied with `git apply`

### Artifact Generation Tests

1. **Required Files**: Tests that all required artifacts are created:
   - `manifest.json` with metadata and status
   - `diff.patch` with git diff content
   - `summary.md` (agent-generated or fallback)

2. **Summary Fallback**: Tests the fallback path when agent doesn't write summary:
   - Uses correct `goal` variable (not `clean_goal`)
   - Includes task outcome and result information

### Integration Tests

1. **Spec Loading**: Tests handling of different goal formats (string vs dict)
2. **Environment Sync**: Tests Claude settings environment variable sync
3. **Fix Permissions**: Tests file permission fixing functionality

## Safety Features Tested

The tests verify several important safety features:

1. **No Path Leakage**: File operations only expose basenames, not full paths
2. **No Content Leakage**: Tool logging doesn't expose file contents
3. **Secure Fallbacks**: Summary generation uses correct variables and doesn't reference undefined values
4. **Error Handling**: Functions handle missing environment variables and file errors gracefully

## Test Results

All tests should pass with output like:

```
Running adapter unit tests...

Testing ProgressLogger initialization...
✓ ProgressLogger initialization tests passed

Testing ProgressLogger level gating...
✓ Level gating tests passed

Testing _safe_filename...
✓ _safe_filename tests passed

Testing log_tool_use safety...
✓ log_tool_use safety tests passed

Testing diff generation flags...
✓ Diff generation flags test passed

Testing summary fallback...
✓ Summary fallback test passed

Testing fix_permissions...
✓ fix_permissions tests passed

Testing spec loading...
✓ Spec loading tests passed

Testing environment sync...
✓ Environment sync tests passed


==================================================
Test Summary: Total 9, Passed 9, Failed 0
🎉 All tests passed!
```

## Notes

- Tests run without calling Claude SDK or network
- Uses mocking to isolate functionality
- Tests are designed to be fast and reliable
- All edge cases are covered including error conditions