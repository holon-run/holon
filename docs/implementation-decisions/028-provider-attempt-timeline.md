# Provider Attempt Timeline

Decision:

- preserve a stable provider-attempt timeline alongside provider-turn outcomes
- expose the same timeline on transcript and audit surfaces
- keep structured transport diagnostics on failed attempts when available

Reason:

- operators need to see retries and fallback progression without reconstructing
  it from one final error string
