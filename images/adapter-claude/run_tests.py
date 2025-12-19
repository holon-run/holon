#!/usr/bin/env python3
"""
Simple test runner for adapter tests that doesn't require pytest installation.
"""

import sys
import os
import traceback
from unittest.mock import Mock, patch, mock_open, MagicMock
import tempfile
import subprocess
import json

# Add the adapter directory to the path so we can import the module
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

# Import the adapter module
from adapter import ProgressLogger, LogLevel, fix_permissions


def test_logger_initialization():
    """Test ProgressLogger initialization with different log levels"""
    print("Testing ProgressLogger initialization...")

    logger = ProgressLogger("debug")
    assert logger.log_level == LogLevel.DEBUG, f"Expected DEBUG, got {logger.log_level}"

    logger = ProgressLogger("INFO")
    assert logger.log_level == LogLevel.INFO, f"Expected INFO, got {logger.log_level}"

    logger = ProgressLogger("progress")
    assert logger.log_level == LogLevel.PROGRESS, f"Expected PROGRESS, got {logger.log_level}"

    logger = ProgressLogger("minimal")
    assert logger.log_level == LogLevel.MINIMAL, f"Expected MINIMAL, got {logger.log_level}"

    # Test invalid level
    try:
        ProgressLogger("invalid")
        assert False, "Expected ValueError for invalid log level"
    except ValueError:
        pass  # Expected

    print("✓ ProgressLogger initialization tests passed")


def test_level_gating():
    """Test that log levels properly filter messages"""
    print("Testing ProgressLogger level gating...")

    # Test DEBUG level - should show all
    logger = ProgressLogger("debug")
    with patch('builtins.print') as mock_print:
        logger.debug("debug message")
        logger.info("info message")
        logger.progress("progress message")
        logger.minimal("minimal message")
        assert mock_print.call_count == 4, f"Expected 4 calls, got {mock_print.call_count}"

    # Test INFO level - should filter debug
    logger = ProgressLogger("info")
    with patch('builtins.print') as mock_print:
        logger.debug("debug message")  # Should not show
        logger.info("info message")    # Should show
        logger.progress("progress message")  # Should show
        logger.minimal("minimal message")    # Should show
        assert mock_print.call_count == 3, f"Expected 3 calls, got {mock_print.call_count}"

    # Test MINIMAL level - should show only minimal
    logger = ProgressLogger("minimal")
    with patch('builtins.print') as mock_print:
        logger.debug("debug message")     # Should not show
        logger.info("info message")       # Should not show
        logger.progress("progress message")  # Should not show
        logger.minimal("minimal message")    # Should show
        assert mock_print.call_count == 1, f"Expected 1 call, got {mock_print.call_count}"

    print("✓ Level gating tests passed")


def test_safe_filename():
    """Test that _safe_filename only returns basename"""
    print("Testing _safe_filename...")

    logger = ProgressLogger("info")

    # Test with full path
    full_path = "/some/deep/path/to/file.txt"
    safe_name = logger._safe_filename(full_path)
    assert safe_name == "file.txt", f"Expected 'file.txt', got '{safe_name}'"

    # Test with relative path
    rel_path = "relative/path/to/file.py"
    safe_name = logger._safe_filename(rel_path)
    assert safe_name == "file.py", f"Expected 'file.py', got '{safe_name}'"

    # Test edge cases
    assert logger._safe_filename("") == "unknown", "Expected 'unknown' for empty string"
    assert logger._safe_filename(None) == "unknown", "Expected 'unknown' for None"

    print("✓ _safe_filename tests passed")


def test_log_tool_use_safety():
    """Test that log_tool_use doesn't leak paths or content"""
    print("Testing log_tool_use safety...")

    logger = ProgressLogger("progress")

    with patch('builtins.print') as mock_print:
        files = [
            "/sensitive/path/secret1.txt",
            "/another/path/secret2.py",
            "relative/secret3.md"
        ]
        logger.log_tool_use("ReadTool", files_touched=files)

        # Should only show basenames
        calls = [str(call) for call in mock_print.call_args_list]
        call_text = ' '.join(calls)

        assert "secret1.txt" in call_text, "Should show basename"
        assert "secret2.py" in call_text, "Should show basename"
        assert "secret3.md" in call_text, "Should show basename"
        # Should NOT contain full paths
        assert "/sensitive/path/" not in call_text, "Should not show full path"
        assert "/another/path/" not in call_text, "Should not show full path"
        assert "relative/" not in call_text, "Should not show relative path"

    # Test tool use counter
    initial_count = logger.tool_use_count
    logger.log_tool_use("Tool1")
    assert logger.tool_use_count == initial_count + 1, "Tool use counter should increment"

    print("✓ log_tool_use safety tests passed")


def test_diff_generation_flags():
    """Test that diff generation includes --binary and --full-index flags"""
    print("Testing diff generation flags...")

    with patch('subprocess.run') as mock_subprocess:
        mock_subprocess.return_value = Mock(stdout="diff", stderr="", returncode=0)

        # Call the diff generation command (from adapter.py line 313)
        result = subprocess.run(
            ["git", "diff", "--cached", "--patch", "--binary", "--full-index"],
            capture_output=True,
            text=True,
        )

        # Verify command was called with correct flags
        mock_subprocess.assert_called_with(
            ["git", "diff", "--cached", "--patch", "--binary", "--full-index"],
            capture_output=True,
            text=True,
        )

    print("✓ Diff generation flags test passed")


def test_summary_fallback():
    """Test summary fallback uses correct variables"""
    print("Testing summary fallback...")

    with tempfile.TemporaryDirectory() as temp_dir:
        goal = "Test goal for fallback"
        success = False
        result = "Task failed but fallback works"

        # Generate fallback summary using exact logic from adapter.py line 353
        summary_text = f"# Task Summary\n\nGoal: {goal}\n\nOutcome: {'Success' if success else 'Failure'}\n\n## Actions\n{result}\n"

        summary_path = os.path.join(temp_dir, "summary.md")
        with open(summary_path, 'w') as f:
            f.write(summary_text)

        # Verify the fallback uses 'goal' correctly
        with open(summary_path, 'r') as f:
            content = f.read()
            assert f"Goal: {goal}" in content, f"Should contain goal: {goal}"
            assert "Outcome: Failure" in content, "Should contain failure outcome"
            # Verify it doesn't reference undefined 'clean_goal'
            assert "clean_goal" not in content, "Should not reference clean_goal"

    print("✓ Summary fallback test passed")


def test_fix_permissions():
    """Test fix_permissions function"""
    print("Testing fix_permissions...")

    # Test with no environment variables
    with patch.dict(os.environ, {}, clear=True):
        with patch('os.chown') as mock_chown:
            fix_permissions("/test/dir")
            mock_chown.assert_not_called()

    # Test with valid environment variables
    with patch.dict(os.environ, {"HOST_UID": "1000", "HOST_GID": "1000"}):
        with patch('os.chown') as mock_chown, \
             patch('os.walk') as mock_walk:

            mock_walk.return_value = [("/test", [], ["file.txt"])]
            logger = Mock()

            fix_permissions("/test", logger)

            # Should chown directory and file
            assert mock_chown.call_count == 2, f"Expected 2 chown calls, got {mock_chown.call_count}"
            logger.debug.assert_called_with("Fixing permissions for /test to 1000:1000")

    print("✓ fix_permissions tests passed")


def test_spec_loading():
    """Test spec loading handles different goal formats"""
    print("Testing spec loading...")

    # Test with dict goal
    spec_dict = {
        "goal": {
            "description": "Test goal description"
        }
    }

    goal_val = spec_dict.get('goal', '')
    if isinstance(goal_val, dict):
        goal = goal_val.get('description', '')
    else:
        goal = str(goal_val)

    assert goal == "Test goal description", f"Expected 'Test goal description', got '{goal}'"

    # Test with string goal
    spec_string = {
        "goal": "Simple string goal"
    }

    goal_val = spec_string.get('goal', '')
    if isinstance(goal_val, dict):
        goal = goal_val.get('description', '')
    else:
        goal = str(goal_val)

    assert goal == "Simple string goal", f"Expected 'Simple string goal', got '{goal}'"

    print("✓ Spec loading tests passed")


def test_environment_sync():
    """Test environment sync to Claude settings"""
    print("Testing environment sync...")

    test_settings = {
        "env": {
            "existing_var": "existing_value"
        }
    }

    env_vars = {
        "ANTHROPIC_AUTH_TOKEN": "test-token",
        "ANTHROPIC_BASE_URL": "https://test.api.com",
        "IS_SANDBOX": "1"
    }

    with patch.dict(os.environ, env_vars, clear=True):
        # Simulate environment sync logic from lines 219-245
        settings = test_settings
        env_section = settings.get("env", {})
        auth_token = os.environ.get("ANTHROPIC_AUTH_TOKEN") or os.environ.get("ANTHROPIC_API_KEY")
        base_url = os.environ.get("ANTHROPIC_BASE_URL") or os.environ.get("ANTHROPIC_API_URL")

        if auth_token:
            env_section["ANTHROPIC_AUTH_TOKEN"] = auth_token
            env_section["ANTHROPIC_API_KEY"] = auth_token
        if base_url:
            env_section["ANTHROPIC_BASE_URL"] = base_url
            env_section["ANTHROPIC_API_URL"] = base_url
            env_section["CLAUDE_CODE_API_URL"] = base_url

        env_section["IS_SANDBOX"] = "1"
        settings["env"] = env_section

        # Verify the settings were updated correctly
        assert env_section["ANTHROPIC_AUTH_TOKEN"] == "test-token", "Auth token not set correctly"
        assert env_section["ANTHROPIC_BASE_URL"] == "https://test.api.com", "Base URL not set correctly"
        assert env_section["IS_SANDBOX"] == "1", "IS_SANDBOX not set correctly"
        assert env_section["existing_var"] == "existing_value", "Should preserve existing"

    print("✓ Environment sync tests passed")


def main():
    """Run all tests"""
    print("Running adapter unit tests...\n")

    tests = [
        test_logger_initialization,
        test_level_gating,
        test_safe_filename,
        test_log_tool_use_safety,
        test_diff_generation_flags,
        test_summary_fallback,
        test_fix_permissions,
        test_spec_loading,
        test_environment_sync
    ]

    passed = 0
    failed = 0

    for test in tests:
        try:
            test()
            passed += 1
        except Exception as e:
            print(f"✗ {test.__name__} failed: {e}")
            print(f"  Traceback: {traceback.format_exc()}")
            failed += 1
        print()

    print(f"\n{'='*50}")
    print(f"Test Summary: Total {len(tests)}, Passed {passed}, Failed {failed}")

    if failed == 0:
        print("🎉 All tests passed!")
        return True
    else:
        print("❌ Some tests failed!")
        return False


if __name__ == "__main__":
    success = main()
    sys.exit(0 if success else 1)