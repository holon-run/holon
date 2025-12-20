"""
Configuration module for the Holon adapter.

This module provides a centralized way to manage paths and configuration
for the Holon adapter, making it testable by allowing the root directory
to be configured via environment variables or programmatically.
"""

import os
from pathlib import Path
from typing import Optional


class HolonConfig:
    """Configuration for Holon adapter paths and settings."""

    def __init__(self, holon_root: Optional[str] = None):
        """
        Initialize Holon configuration.

        Args:
            holon_root: Root directory for Holon. If None, uses HOLON_ROOT
                       environment variable, then defaults to "/holon".
        """
        self.holon_root = holon_root or os.environ.get("HOLON_ROOT", "/holon")
        self.root_path = Path(self.holon_root)

    @property
    def input_dir(self) -> Path:
        """Directory containing input files."""
        return self.root_path / "input"

    @property
    def workspace_dir(self) -> Path:
        """Directory for workspace operations."""
        return self.root_path / "workspace"

    @property
    def output_dir(self) -> Path:
        """Directory for output files."""
        return self.root_path / "output"

    @property
    def evidence_dir(self) -> Path:
        """Directory for evidence files."""
        return self.output_dir / "evidence"

    @property
    def spec_path(self) -> Path:
        """Path to the specification file."""
        return self.input_dir / "spec.yaml"

    @property
    def system_prompt_path(self) -> Path:
        """Path to the system prompt file."""
        return self.input_dir / "prompts" / "system.md"

    @property
    def user_prompt_path(self) -> Path:
        """Path to the user prompt file."""
        return self.input_dir / "prompts" / "user.md"

    @property
    def execution_log_path(self) -> Path:
        """Path to the execution log file."""
        return self.evidence_dir / "execution.log"

    @property
    def manifest_path(self) -> Path:
        """Path to the manifest file."""
        return self.output_dir / "manifest.json"

    @property
    def diff_path(self) -> Path:
        """Path to the diff patch file."""
        return self.output_dir / "diff.patch"

    @property
    def summary_path(self) -> Path:
        """Path to the summary file."""
        return self.output_dir / "summary.md"

    @property
    def claude_settings_path(self) -> Path:
        """Path to Claude settings file."""
        return Path.home() / ".claude" / "settings.json"

    def ensure_directories(self):
        """Create necessary directories if they don't exist."""
        self.output_dir.mkdir(parents=True, exist_ok=True)
        self.evidence_dir.mkdir(parents=True, exist_ok=True)

    def get_env_auth_token(self) -> Optional[str]:
        """Get authentication token from environment."""
        return os.environ.get("ANTHROPIC_AUTH_TOKEN") or os.environ.get("ANTHROPIC_API_KEY")

    def get_env_base_url(self) -> str:
        """Get base URL from environment."""
        return (
            os.environ.get("ANTHROPIC_BASE_URL") or
            os.environ.get("ANTHROPIC_API_URL") or
            "https://api.anthropic.com"
        )

    def get_env_model(self) -> Optional[str]:
        """Get model override from environment."""
        return os.environ.get("HOLON_MODEL")

    def get_env_fallback_model(self) -> Optional[str]:
        """Get fallback model from environment."""
        return os.environ.get("HOLON_FALLBACK_MODEL")

    def get_env_log_level(self) -> str:
        """Get log level from environment."""
        return os.environ.get("LOG_LEVEL", "progress")

    def get_env_timeout_seconds(self) -> int:
        """Get query timeout from environment."""
        return self._int_env("HOLON_QUERY_TIMEOUT_SECONDS", 300)

    def get_env_heartbeat_seconds(self) -> int:
        """Get heartbeat interval from environment."""
        return self._int_env("HOLON_HEARTBEAT_SECONDS", 60)

    def get_env_idle_timeout_seconds(self) -> int:
        """Get idle timeout from environment."""
        return self._int_env("HOLON_RESPONSE_IDLE_TIMEOUT_SECONDS", 1800)

    def get_env_total_timeout_seconds(self) -> int:
        """Get total timeout from environment."""
        return self._int_env("HOLON_RESPONSE_TOTAL_TIMEOUT_SECONDS", 7200)

    def get_env_host_uid(self) -> Optional[int]:
        """Get host UID from environment."""
        return self._int_env("HOST_UID", None)

    def get_env_host_gid(self) -> Optional[int]:
        """Get host GID from environment."""
        return self._int_env("HOST_GID", None)

    def _int_env(self, name: str, default):
        """Helper to get integer from environment with default."""
        value = os.environ.get(name)
        if value is None or value == "":
            return default
        try:
            return int(value)
        except ValueError:
            return default