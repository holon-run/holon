#!/usr/bin/env python3
"""Prepare a revision-bearing Docker acceptance build without version overrides."""
from docker_e2e.identified_build import main

if __name__ == "__main__":
    raise SystemExit(main())
