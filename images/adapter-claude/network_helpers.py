"""
Network helper module for the Holon adapter.

This module provides wrappers for network operations,
making them easy to mock for testing.
"""

import urllib.request
from typing import Optional
from claude_agent_sdk import ClaudeAgentOptions, ClaudeSDKClient


class NetworkClient:
    """Wrapper for network operations that can be mocked."""

    def test_connectivity(self, url: str, timeout: int = 10) -> bool:
        """
        Test connectivity to a URL.

        Args:
            url: URL to test connectivity to
            timeout: Timeout in seconds

        Returns:
            True if connectivity test succeeds, False otherwise
        """
        try:
            with urllib.request.urlopen(url, timeout=timeout) as response:
                return response.status == 200
        except Exception:
            return False

    def create_claude_client(self, options: ClaudeAgentOptions) -> ClaudeSDKClient:
        """
        Create a Claude SDK client.

        Args:
            options: Configuration options for the client

        Returns:
            ClaudeSDKClient instance
        """
        return ClaudeSDKClient(options=options)


# Global instance that can be replaced in tests
network_client = NetworkClient()