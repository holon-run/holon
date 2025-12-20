"""
File system helper module for the Holon adapter.

This module provides wrappers for file system operations,
making them easy to mock for testing.
"""

import os
import json
import yaml
from pathlib import Path
from typing import Dict, Any, Optional


class FileSystemOperations:
    """Wrapper for file system operations that can be mocked."""

    def chdir(self, path: str) -> None:
        """Change current working directory."""
        os.chdir(path)

    def exists(self, path: Path) -> bool:
        """Check if a path exists."""
        return os.path.exists(path)

    def makedirs(self, path: Path, exist_ok: bool = False) -> None:
        """Create directories recursively."""
        os.makedirs(path, exist_ok=exist_ok)

    def read_text(self, path: Path) -> str:
        """Read text from a file."""
        with open(path, 'r') as f:
            return f.read()

    def write_text(self, path: Path, content: str) -> None:
        """Write text to a file."""
        with open(path, 'w') as f:
            f.write(content)

    def read_yaml(self, path: Path) -> Dict[str, Any]:
        """Read YAML file and return as dictionary."""
        with open(path, 'r') as f:
            return yaml.safe_load(f)

    def read_json(self, path: Path) -> Dict[str, Any]:
        """Read JSON file and return as dictionary."""
        with open(path, 'r') as f:
            return json.load(f)

    def write_json(self, path: Path, data: Dict[str, Any], indent: int = 2) -> None:
        """Write dictionary to JSON file."""
        with open(path, 'w') as f:
            json.dump(data, f, indent=indent)

    def append_text(self, path: Path, content: str) -> None:
        """Append text to a file."""
        with open(path, 'a') as f:
            f.write(content)

    def chown(self, path: str, uid: int, gid: int) -> None:
        """Change ownership of a file or directory."""
        os.chown(path, uid, gid)

    def walk(self, path: str):
        """Walk directory tree."""
        return os.walk(path)

    def expanduser(self, path: str) -> str:
        """Expand ~ to user home directory."""
        return os.path.expanduser(path)


class PermissionFixer:
    """Handles fixing file permissions for host UID/GID."""

    def __init__(self, fs_ops: FileSystemOperations):
        self.fs_ops = fs_ops

    def fix_permissions(self, directory: Path, uid: Optional[int] = None, gid: Optional[int] = None):
        """
        Recursively change ownership of the directory and its contents.

        Args:
            directory: Directory to fix permissions for
            uid: User ID (if None, gets from HOST_UID environment)
            gid: Group ID (if None, gets from HOST_GID environment)
        """
        if uid is None:
            uid_str = os.environ.get("HOST_UID")
            uid = int(uid_str) if uid_str else None

        if gid is None:
            gid_str = os.environ.get("HOST_GID")
            gid = int(gid_str) if gid_str else None

        if uid is None or gid is None:
            return

        try:
            # Change ownership of the directory itself
            self.fs_ops.chown(str(directory), uid, gid)

            # Recursively change ownership of contents
            for root, dirs, files in self.fs_ops.walk(str(directory)):
                for d in dirs:
                    self.fs_ops.chown(os.path.join(root, d), uid, gid)
                for f in files:
                    self.fs_ops.chown(os.path.join(root, f), uid, gid)
        except Exception as e:
            # Silently ignore permission errors during testing
            import sys
            print(f"Warning: Failed to fix permissions: {e}", file=sys.stderr)


# Global instances that can be replaced in tests
fs_operations = FileSystemOperations()
permission_fixer = PermissionFixer(fs_operations)