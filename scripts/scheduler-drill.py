#!/usr/bin/env python3
"""Stable CLI entry point for resumable scheduler drills."""

from docker_e2e.scheduler_drill import main


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, ValueError) as error:
        print(f"error: {error}", file=__import__("sys").stderr)
        raise SystemExit(2)
