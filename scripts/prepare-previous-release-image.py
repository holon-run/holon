#!/usr/bin/env python3
"""Build an isolated previous-release image from verified release assets."""
from docker_e2e.previous_release import main

if __name__ == "__main__":
    raise SystemExit(main())
