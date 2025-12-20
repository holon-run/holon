"""
Subprocess helper module for the Holon adapter.

This module provides a thin wrapper around subprocess operations,
making them easy to mock for testing.
"""

import subprocess
from typing import List, Optional, Tuple, Any


class SubprocessRunner:
    """Wrapper for subprocess operations that can be mocked."""

    def run(self, cmd: List[str], **kwargs) -> subprocess.CompletedProcess:
        """
        Run a subprocess command.

        Args:
            cmd: Command to run as a list of strings
            **kwargs: Additional arguments passed to subprocess.run

        Returns:
            CompletedProcess instance
        """
        return subprocess.run(cmd, **kwargs)

    def run_git_config(self, workspace_path: str) -> None:
        """
        Configure git for the workspace.

        Args:
            workspace_path: Path to the workspace directory
        """
        self.run(["git", "config", "--global", "--add", "safe.directory", workspace_path], check=False)
        self.run(["git", "config", "--global", "user.name", "holon-adapter"], check=False)
        self.run(["git", "config", "--global", "user.email", "adapter@holon.local"], check=False)

    def run_git_init(self) -> subprocess.CompletedProcess:
        """Initialize git repository."""
        return self.run(["git", "init"], check=True, capture_output=True)

    def run_git_add(self, files: Optional[List[str]] = None) -> subprocess.CompletedProcess:
        """
        Add files to git staging area.

        Args:
            files: List of files to add. If None, adds all files (-A).
        """
        cmd = ["git", "add"]
        if files is None:
            cmd.append("-A")
        else:
            cmd.extend(files)
        return self.run(cmd, capture_output=True)

    def run_git_commit(self, message: str) -> subprocess.CompletedProcess:
        """
        Create a git commit.

        Args:
            message: Commit message
        """
        return self.run(["git", "commit", "-m", message], check=True, capture_output=True)

    def run_git_diff_cached(self) -> subprocess.CompletedProcess:
        """
        Generate a diff patch of staged changes.

        Returns:
            CompletedProcess with the diff content in stdout
        """
        return self.run(
            ["git", "diff", "--cached", "--patch", "--binary", "--full-index"],
            capture_output=True,
            text=True,
        )


# Global instance that can be replaced in tests
subprocess_runner = SubprocessRunner()