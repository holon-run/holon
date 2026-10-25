#!/usr/bin/env python3
"""Run independent, credential-free Docker fallback fault acceptance."""
from docker_e2e.fallback_storm import main

if __name__ == "__main__":
    raise SystemExit(main())
