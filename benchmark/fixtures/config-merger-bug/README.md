# Config Merger Bug Fixture

This fixture models a tiny configuration pipeline:

- `src/defaults.js` provides default config
- `src/normalize.js` sanitizes user overrides
- `src/merge.js` combines defaults and overrides
- `src/index.js` exposes the public API

The failing test expects nested defaults to survive partial overrides and falsey
boolean overrides to be preserved rather than dropped.
