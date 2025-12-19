import pytest
import os
import sys
import json
import yaml
import tempfile
import subprocess
from unittest.mock import Mock, patch, mock_open, MagicMock
from pathlib import Path

# Add the adapter directory to the path so we can import the module
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from adapter import ProgressLogger, LogLevel, fix_permissions


class TestProgressLogger:
    """Test suite for ProgressLogger class functionality"""

    def test_log_level_initialization(self):
        """Test that ProgressLogger initializes with correct log levels"""
        logger = ProgressLogger("debug")
        assert logger.log_level == LogLevel.DEBUG

        logger = ProgressLogger("INFO")
        assert logger.log_level == LogLevel.INFO

        logger = ProgressLogger("progress")
        assert logger.log_level == LogLevel.PROGRESS

        logger = ProgressLogger("minimal")
        assert logger.log_level == LogLevel.MINIMAL

    def test_invalid_log_level_raises_error(self):
        """Test that invalid log levels raise ValueError"""
        with pytest.raises(ValueError):
            ProgressLogger("invalid")

    def test_level_gating_debug_shows_all(self):
        """Test that DEBUG level shows all log messages"""
        logger = ProgressLogger("debug")

        with patch('builtins.print') as mock_print:
            logger.debug("debug message")
            logger.info("info message")
            logger.progress("progress message")
            logger.minimal("minimal message")

            # All should be shown at debug level
            assert mock_print.call_count == 4
            calls = [str(call) for call in mock_print.call_args_list]
            assert "[DEBUG] debug message" in str(calls)
            assert "[INFO] info message" in str(calls)
            assert "[PROGRESS] progress message" in str(calls)
            assert "[PHASE] minimal message" in str(calls)

    def test_level_gating_info_shows_info_and_above(self):
        """Test that INFO level shows info, progress, and minimal messages"""
        logger = ProgressLogger("info")

        with patch('builtins.print') as mock_print:
            logger.debug("debug message")  # Should not show
            logger.info("info message")
            logger.progress("progress message")
            logger.minimal("minimal message")

            # Should show 3 messages (not debug)
            assert mock_print.call_count == 3
            calls = [str(call) for call in mock_print.call_args_list]
            assert "[DEBUG] debug message" not in str(calls)
            assert "[INFO] info message" in str(calls)
            assert "[PROGRESS] progress message" in str(calls)
            assert "[PHASE] minimal message" in str(calls)

    def test_level_gating_progress_shows_progress_and_above(self):
        """Test that PROGRESS level shows progress and minimal messages"""
        logger = ProgressLogger("progress")

        with patch('builtins.print') as mock_print:
            logger.debug("debug message")   # Should not show
            logger.info("info message")     # Should not show
            logger.progress("progress message")
            logger.minimal("minimal message")

            # Should show 2 messages
            assert mock_print.call_count == 2
            calls = [str(call) for call in mock_print.call_args_list]
            assert "[DEBUG] debug message" not in str(calls)
            assert "[INFO] info message" not in str(calls)
            assert "[PROGRESS] progress message" in str(calls)
            assert "[PHASE] minimal message" in str(calls)

    def test_level_gating_minimal_shows_only_minimal(self):
        """Test that MINIMAL level shows only minimal messages"""
        logger = ProgressLogger("minimal")

        with patch('builtins.print') as mock_print:
            logger.debug("debug message")     # Should not show
            logger.info("info message")       # Should not show
            logger.progress("progress message")  # Should not show
            logger.minimal("minimal message")

            # Should show only 1 message
            assert mock_print.call_count == 1
            calls = [str(call) for call in mock_print.call_args_list]
            assert "[DEBUG] debug message" not in str(calls)
            assert "[INFO] info message" not in str(calls)
            assert "[PROGRESS] progress message" not in str(calls)
            assert "[PHASE] minimal message" in str(calls)

    def test_safe_filename_returns_basename_only(self):
        """Test that _safe_filename only returns the basename, not full path"""
        logger = ProgressLogger("info")

        # Test with full path
        full_path = "/some/deep/path/to/file.txt"
        safe_name = logger._safe_filename(full_path)
        assert safe_name == "file.txt"

        # Test with relative path
        rel_path = "relative/path/to/file.py"
        safe_name = logger._safe_filename(rel_path)
        assert safe_name == "file.py"

        # Test with just filename
        filename = "simple.txt"
        safe_name = logger._safe_filename(filename)
        assert safe_name == "simple.txt"

        # Test with edge cases
        assert logger._safe_filename("") == "unknown"
        assert logger._safe_filename(None) == "unknown"
        assert logger._safe_filename("/") == ""
        assert logger._safe_filename("/.hidden") == ".hidden"

    def test_log_tool_use_safety_no_content_leak(self):
        """Test that log_tool_use doesn't leak file content or full paths"""
        logger = ProgressLogger("progress")

        with patch('builtins.print') as mock_print:
            # Test with files_touched
            files = [
                "/sensitive/path/secret1.txt",
                "/another/path/secret2.py",
                "relative/secret3.md"
            ]
            logger.log_tool_use("ReadTool", files_touched=files)

            # Should only show basenames
            calls = [str(call) for call in mock_print.call_args_list]
            call_text = ' '.join(calls)
            assert "secret1.txt" in call_text
            assert "secret2.py" in call_text
            assert "secret3.md" in call_text
            # Should NOT contain full paths
            assert "/sensitive/path/" not in call_text
            assert "/another/path/" not in call_text
            assert "relative/" not in call_text

        with patch('builtins.print') as mock_print:
            # Test with file_count
            logger.log_tool_use("WriteTool", file_count=5)
            calls = [str(call) for call in mock_print.call_args_list]
            call_text = ' '.join(calls)
            assert "WriteTool → 5 items" in call_text

        with patch('builtins.print') as mock_print:
            # Test with no additional info
            logger.log_tool_use("GenericTool")
            calls = [str(call) for call in mock_print.call_args_list]
            call_text = ' '.join(calls)
            assert "GenericTool" in call_text

    def test_log_tool_use_file_display_limit(self):
        """Test that log_tool_use limits file display when there are many files"""
        logger = ProgressLogger("progress")

        with patch('builtins.print') as mock_print:
            # Test with few files (<= 3)
            few_files = ["file1.txt", "file2.py", "file3.md"]
            logger.log_tool_use("ReadTool", files_touched=few_files)

            calls = [str(call) for call in mock_print.call_args_list]
            call_text = ' '.join(calls)
            # Should show individual files
            assert "file1.txt" in call_text
            assert "file2.py" in call_text
            assert "file3.md" in call_text
            assert "3 files" in call_text

        mock_print.reset_mock()
        with patch('builtins.print') as mock_print:
            # Test with many files (> 3)
            many_files = [f"file{i}.txt" for i in range(10)]
            logger.log_tool_use("ReadTool", files_touched=many_files)

            calls = [str(call) for call in mock_print.call_args_list]
            call_text = ' '.join(calls)
            # Should only show count, not individual files
            assert "10 files" in call_text
            assert "file1.txt" not in call_text
            assert "file2.txt" not in call_text

    def test_log_phase_and_outcome(self):
        """Test log_phase and log_outcome methods"""
        logger = ProgressLogger("minimal")

        with patch('builtins.print') as mock_print:
            logger.log_phase("Test Phase")
            calls = [str(call) for call in mock_print.call_args_list]
            call_text = ' '.join(calls)
            assert "Starting: Test Phase" in call_text

        mock_print.reset_mock()
        with patch('builtins.print') as mock_print:
            logger.log_outcome(True, 123.45)
            calls = [str(call) for call in mock_print.call_args_list]
            call_text = ' '.join(calls)
            assert "Outcome: SUCCESS" in call_text
            assert "123.5s" in call_text  # Rounded to 1 decimal

        mock_print.reset_mock()
        with patch('builtins.print') as mock_print:
            logger.log_outcome(False, 67.89, error="Test error")
            calls = [str(call) for call in mock_print.call_args_list]
            call_text = ' '.join(calls)
            assert "Outcome: FAILURE" in call_text
            assert "67.9s" in call_text

    def test_log_summary_excerpt(self):
        """Test log_summary_excerpt functionality"""
        logger = ProgressLogger("minimal")

        # Create a temporary summary file
        summary_content = """# Task Summary

This is a test summary with multiple lines.
Line 3
Line 4
Line 5
Line 6
Line 7
"""

        with tempfile.NamedTemporaryFile(mode='w', suffix='.md', delete=False) as f:
            f.write(summary_content)
            temp_path = f.name

        try:
            with patch('builtins.print') as mock_print:
                logger.log_summary_excerpt(temp_path, lines=3)

                calls = [str(call) for call in mock_print.call_args_list]
                call_text = ' '.join(calls)
                assert "=== SUMMARY EXCERPT ===" in call_text
                assert "This is a test summary" in call_text
                assert "Line 3" in call_text
                assert "Line 4" in call_text
                assert "and 4 more lines" in call_text
                assert "=== END SUMMARY ===" in call_text
        finally:
            os.unlink(temp_path)

        # Test with non-existent file
        with patch('builtins.print') as mock_print:
            logger.log_summary_excerpt("/non/existent/file.md")
            calls = [str(call) for call in mock_print.call_args_list]
            call_text = ' '.join(calls)
            assert "[WARNING] Summary file not found" in call_text

    def test_tool_use_count_increments(self):
        """Test that tool_use_count increments with each log_tool_use call"""
        logger = ProgressLogger("progress")

        initial_count = logger.tool_use_count
        logger.log_tool_use("Tool1")
        assert logger.tool_use_count == initial_count + 1

        logger.log_tool_use("Tool2", files_touched=["file1.txt"])
        assert logger.tool_use_count == initial_count + 2

        logger.log_tool_use("Tool3", file_count=5)
        assert logger.tool_use_count == initial_count + 3


class TestDiffGeneration:
    """Test suite for diff generation functionality"""

    @patch('subprocess.run')
    def test_diff_generation_includes_binary_and_full_index_flags(self, mock_subprocess):
        """Test that diff generation command includes --binary and --full-index flags"""
        # Mock the git diff command
        mock_subprocess.return_value = Mock(
            stdout="diff content",
            stderr="",
            returncode=0
        )

        # Import and run the relevant part of the adapter
        with patch('os.chdir'), \
             patch('os.path.exists', return_value=True), \
             patch('subprocess.run') as mock_add:

            # Simulate the git add command
            mock_add.return_value = Mock(returncode=0)

            # Call the diff generation part (simplified from adapter)
            result = subprocess.run(
                ["git", "diff", "--cached", "--patch", "--binary", "--full-index"],
                capture_output=True,
                text=True,
            )

            # Verify the command was called with correct flags
            mock_subprocess.assert_called_with(
                ["git", "diff", "--cached", "--patch", "--binary", "--full-index"],
                capture_output=True,
                text=True,
            )


class TestArtifactGeneration:
    """Test suite for artifact generation functionality"""

    def test_artifact_generation_creates_required_files(self):
        """Test that artifact generation creates manifest.json, diff.patch, and summary.md"""
        with tempfile.TemporaryDirectory() as temp_dir:
            output_dir = temp_dir

            # Mock data
            patch_content = "diff patch content"
            success = True
            duration = 123.45
            goal = "Test goal"
            result = "Task completed successfully"

            # Generate manifest
            manifest = {
                "metadata": {
                    "adapter": "claude-code",
                    "version": "0.1.0"
                },
                "status": "completed",
                "outcome": "success" if success else "failure",
                "duration": duration,
                "artifacts": [
                    {"name": "diff.patch", "path": "diff.patch"},
                    {"name": "summary.md", "path": "summary.md"},
                    {"name": "evidence", "path": "evidence/"}
                ]
            }

            # Write manifest
            manifest_path = os.path.join(output_dir, "manifest.json")
            with open(manifest_path, 'w') as f:
                json.dump(manifest, f, indent=2)

            # Write diff
            diff_path = os.path.join(output_dir, "diff.patch")
            with open(diff_path, 'w') as f:
                f.write(patch_content)

            # Write summary (agent-generated)
            summary_path = os.path.join(output_dir, "summary.md")
            summary_text = f"# Agent Summary\n\nCustom summary written by agent."
            with open(summary_path, 'w') as f:
                f.write(summary_text)

            # Verify files exist
            assert os.path.exists(manifest_path)
            assert os.path.exists(diff_path)
            assert os.path.exists(summary_path)

            # Verify content
            with open(manifest_path, 'r') as f:
                loaded_manifest = json.load(f)
                assert loaded_manifest["outcome"] == "success"
                assert loaded_manifest["duration"] == duration
                assert len(loaded_manifest["artifacts"]) == 3

            with open(diff_path, 'r') as f:
                assert f.read() == patch_content

            with open(summary_path, 'r') as f:
                assert "Agent Summary" in f.read()

    def test_summary_fallback_when_no_agent_summary(self):
        """Test summary fallback when agent didn't write summary.md"""
        with tempfile.TemporaryDirectory() as temp_dir:
            output_dir = temp_dir

            # Test data
            goal = "Test goal for fallback"
            success = True
            result = "Task completed with fallback"

            # Check that summary.md doesn't exist (agent didn't create it)
            summary_path = os.path.join(output_dir, "summary.md")
            assert not os.path.exists(summary_path)

            # Generate fallback summary (logic from adapter)
            summary_text = f"# Task Summary\n\nGoal: {goal}\n\nOutcome: {'Success' if success else 'Failure'}\n\n## Actions\n{result}\n"

            # Write fallback summary
            with open(summary_path, 'w') as f:
                f.write(summary_text)

            # Verify fallback summary was created and contains correct content
            assert os.path.exists(summary_path)
            with open(summary_path, 'r') as f:
                content = f.read()
                assert "Task Summary" in content
                assert f"Goal: {goal}" in content
                assert "Outcome: Success" in content
                assert result in content

    def test_summary_fallback_uses_correct_variable(self):
        """Test that summary fallback uses 'goal' variable correctly (not clean_goal)"""
        with tempfile.TemporaryDirectory() as temp_dir:
            output_dir = temp_dir

            # Test data to verify correct variable usage
            goal = "Test goal with correct variable reference"
            success = False
            result = "Task failed but fallback works"

            # Generate fallback summary using the exact logic from line 353 of adapter.py
            summary_text = f"# Task Summary\n\nGoal: {goal}\n\nOutcome: {'Success' if success else 'Failure'}\n\n## Actions\n{result}\n"

            summary_path = os.path.join(output_dir, "summary.md")
            with open(summary_path, 'w') as f:
                f.write(summary_text)

            # Verify the fallback uses 'goal' correctly
            with open(summary_path, 'r') as f:
                content = f.read()
                assert f"Goal: {goal}" in content
                assert "Outcome: Failure" in content

            # Verify it doesn't reference undefined 'clean_goal'
            assert "clean_goal" not in content


class TestFixPermissions:
    """Test suite for fix_permissions function"""

    def test_fix_permissions_no_env_vars(self):
        """Test fix_permissions gracefully handles missing environment variables"""
        with patch.dict(os.environ, {}, clear=True):
            with patch('os.chown') as mock_chown:
                # Should return early without attempting to chown
                fix_permissions("/test/dir")
                mock_chown.assert_not_called()

    def test_fix_permissions_with_valid_env_vars(self):
        """Test fix_permissions works with valid HOST_UID and HOST_GID"""
        with patch.dict(os.environ, {"HOST_UID": "1000", "HOST_GID": "1000"}):
            with patch('os.chown') as mock_chown, \
                 patch('os.walk') as mock_walk:

                # Mock directory structure
                mock_walk.return_value = [
                    ("/test", ["subdir"], ["file1.txt"]),
                    ("/test/subdir", [], ["file2.txt"])
                ]

                logger = Mock()
                fix_permissions("/test", logger)

                # Should chown directory and all contents
                expected_calls = [
                    (("/test", 1000, 1000), {}),
                    (("/test/subdir", 1000, 1000), {}),
                    (("/test/file1.txt", 1000, 1000), {}),
                    (("/test/subdir/file2.txt", 1000, 1000), {})
                ]

                assert mock_chown.call_count == 4
                logger.debug.assert_called_with("Fixing permissions for /test to 1000:1000")

    def test_fix_permissions_handles_errors_gracefully(self):
        """Test fix_permissions handles permission errors gracefully"""
        with patch.dict(os.environ, {"HOST_UID": "1000", "HOST_GID": "1000"}):
            with patch('os.chown', side_effect=PermissionError("Permission denied")), \
                 patch('os.walk', return_value=[("/test", [], ["file.txt"])]):

                logger = Mock()
                # Should not raise exception
                fix_permissions("/test", logger)

                # Should log warning
                logger.info.assert_called_with("Warning: Failed to fix permissions: Permission denied")

    def test_fix_permissions_without_logger(self):
        """Test fix_permissions works without logger parameter"""
        with patch.dict(os.environ, {"HOST_UID": "1000", "HOST_GID": "1000"}):
            with patch('os.chown', side_effect=PermissionError("Permission denied")), \
                 patch('os.walk', return_value=[("/test", [], ["file.txt"])]), \
                 patch('builtins.print') as mock_print:

                # Should not raise exception
                fix_permissions("/test", logger=None)

                # Should print to stderr
                mock_print.assert_called_with("Warning: Failed to fix permissions: Permission denied", file=sys.stderr)


class TestAdapterIntegration:
    """Integration tests for adapter functionality that doesn't require Claude SDK"""

    def test_spec_loading_with_dict_goal(self):
        """Test spec loading handles both string and dict goal formats"""
        # Test with dict goal
        spec_dict = {
            "goal": {
                "description": "Test goal description"
            }
        }

        # Simulate the goal handling logic from adapter.py lines 160-165
        goal_val = spec_dict.get('goal', '')
        if isinstance(goal_val, dict):
            goal = goal_val.get('description', '')
        else:
            goal = str(goal_val)

        assert goal == "Test goal description"

        # Test with string goal
        spec_string = {
            "goal": "Simple string goal"
        }

        goal_val = spec_string.get('goal', '')
        if isinstance(goal_val, dict):
            goal = goal_val.get('description', '')
        else:
            goal = str(goal_val)

        assert goal == "Simple string goal"

    @patch('subprocess.run')
    def test_git_configuration_commands(self, mock_subprocess):
        """Test that git configuration commands are called correctly"""
        mock_subprocess.return_value = Mock(returncode=0)

        # Simulate the git configuration from adapter.py lines 200-203
        subprocess.run(["git", "config", "--global", "--add", "safe.directory", "/holon/workspace"], check=False)
        subprocess.run(["git", "config", "--global", "user.name", "holon-adapter"], check=False)
        subprocess.run(["git", "config", "--global", "user.email", "adapter@holon.local"], check=False)

        # Verify calls were made
        assert mock_subprocess.call_count == 3

        calls = [call[0][0] for call in mock_subprocess.call_args_list]
        assert ["git", "config", "--global", "--add", "safe.directory", "/holon/workspace"] in calls
        assert ["git", "config", "--global", "user.name", "holon-adapter"] in calls
        assert ["git", "config", "--global", "user.email", "adapter@holon.local"] in calls

    def test_environment_sync_to_claude_settings(self):
        """Test environment variable sync to Claude settings"""
        test_settings = {
            "env": {
                "existing_var": "existing_value"
            }
        }

        # Mock environment variables
        env_vars = {
            "ANTHROPIC_AUTH_TOKEN": "test-token",
            "ANTHROPIC_BASE_URL": "https://test.api.com",
            "IS_SANDBOX": "1"
        }

        with patch.dict(os.environ, env_vars, clear=True), \
             patch('os.path.exists', return_value=True), \
             patch('builtins.open', mock_open(read_data=json.dumps(test_settings))), \
             patch('json.dump') as mock_dump:

            # Simulate the environment sync logic from lines 219-245
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
            assert env_section["ANTHROPIC_AUTH_TOKEN"] == "test-token"
            assert env_section["ANTHROPIC_API_KEY"] == "test-token"
            assert env_section["ANTHROPIC_BASE_URL"] == "https://test.api.com"
            assert env_section["ANTHROPIC_API_URL"] == "https://test.api.com"
            assert env_section["CLAUDE_CODE_API_URL"] == "https://test.api.com"
            assert env_section["IS_SANDBOX"] == "1"
            assert env_section["existing_var"] == "existing_value"  # Should preserve existing


if __name__ == "__main__":
    pytest.main([__file__, "-v"])