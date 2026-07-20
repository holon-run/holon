#!/usr/bin/env python3
"""Stable CLI entry point for the Holon release Docker E2E suite."""

from docker_e2e.runner import main


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, ValueError) as error:
        print(f"error: {error}", file=__import__("sys").stderr)
        raise SystemExit(2)
