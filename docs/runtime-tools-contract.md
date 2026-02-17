# Runtime Tools Contract

This document defines Holon's built-in runtime tools contract.

## Scope

- This contract is internal to Holon runtime.
- It has no project-level config and no CLI override.
- Custom images can preinstall these tools; Holon still verifies and installs missing tools during composed-image build.

## Required Tools

The following commands are required in runtime containers:

- `bash`
- `git`
- `curl`
- `jq`
- `rg`
- `find`
- `sed`
- `awk`
- `xargs`
- `tar`
- `gzip`
- `unzip`
- `python3`
- `node`
- `npm`
- `gh`
- `yq`
- `fd`
- `make`
- `patch`

## Behavior

- Holon installs missing required tools when building composed images.
- Supported package-manager families: `apt-get`, `dnf`, `yum`.
- If required tools cannot be ensured, build fails fast with a clear missing-tools error.
