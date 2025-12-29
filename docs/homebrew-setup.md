# Homebrew Tap Setup Guide

This document explains how to set up and maintain the Holon Homebrew tap.

## Overview

The Homebrew tap allows users to install Holon using Homebrew:

```bash
brew install holon-run/tap/holon
```

The tap is hosted in a separate GitHub repository: [holon-run/homebrew-tap](https://github.com/holon-run/homebrew-tap)

## Initial Setup

### 1. Create the Tap Repository

You need to create a separate GitHub repository for the Homebrew tap. The repository should contain:

- `Formula/holon.rb` - The Homebrew formula file
- `README.md` - Documentation for the tap

**Option A: Create from the homebrew-tap directory in this repo**

```bash
# Clone the main holon repo
git clone https://github.com/holon-run/holon.git
cd holon

# Create the tap repository using the homebrew-tap directory
cd homebrew-tap
git init
git add .
git commit -m "Initial Homebrew tap setup"

# Create a new GitHub repo called "homebrew-tap" under holon-run org
# Then add the remote:
git remote add origin git@github.com:holon-run/homebrew-tap.git
git branch -M main
git push -u origin main
```

**Option B: Manual repository creation**

1. Create a new repository on GitHub: `holon-run/homebrew-tap`
2. Copy the contents of `homebrew-tap/` from the main repo
3. Push to the new repository

### 2. Configure GitHub Actions (Optional)

If you want the tap repository to be automatically updated during releases:

1. Create a Personal Access Token (PAT) with `repo` scope
2. Add the PAT as a repository secret in the main Holon repo: `Settings > Secrets > Actions > New repository secret`
   - Name: `HOMEBREW_TAP_TOKEN`
   - Value: `<your PAT>`

3. Update the release workflow to commit and push to the tap repository (see section below)

## Formula Updates

### Manual Updates

After each release, you need to update the formula with new URLs and SHA256 checksums:

1. Download the updated formula from the release artifacts (uploaded by the release workflow)
2. Or run the update script manually:

```bash
./scripts/update-homebrew-formula.sh v0.1.0
```

3. Commit and push the changes to the tap repository:

```bash
cd homebrew-tap
git add Formula/holon.rb
git commit -m "Update formula for Holon v0.1.0"
git push origin main
```

### Automatic Updates (Recommended)

The release workflow (`.github/workflows/release.yml`) automatically:

1. Builds the release binaries
2. Generates SHA256 checksums
3. Updates the `homebrew-tap/Formula/holon.rb` file with new URLs and checksums
4. Optionally commits and pushes to the tap repository (if configured)

**To enable automatic updates:**

The release workflow (`.github/workflows/release.yml`) already includes the logic to push to the tap repository. You only need to:

1. Ensure the tap repository exists
2. Add the `HOMEBREW_TAP_TOKEN` secret to the main repository

The workflow will automatically:
- Clone the tap repository
- Copy the updated formula
- Commit and push the changes

No manual workflow modifications are needed.

## Testing the Formula

Before releasing, you can test the formula locally:

```bash
# From the homebrew-tap directory
brew install ./Formula/holon.rb

# Or from the tap (after pushing)
brew install holon-run/tap/holon

# Verify installation
holon version

# Test upgrade
brew upgrade holon-run/tap/holon
```

## Formula Structure

The formula (`Formula/holon.rb`) uses platform-specific installation:

- **macOS (Intel)**: Downloads `holon-darwin-amd64.tar.gz`
- **macOS (ARM)**: Downloads `holon-darwin-arm64.tar.gz`
- **Linux (amd64)**: Downloads `holon-linux-amd64.tar.gz`

Each tarball contains a binary named `holon` (no platform suffix), which is installed directly to the bin directory. Each platform has its own URL and SHA256 checksum, which are updated during releases.

## Homebrew Livecheck

The formula includes a `livecheck` block that enables users to check for updates:

```bash
brew livecheck holon-run/tap/holon
```

This uses GitHub's releases API to check for the latest version.

## Troubleshooting

### Formula not updating

If users report that `brew upgrade` doesn't work:

1. Check that the tap repository has the latest formula
2. Verify SHA256 checksums are correct
3. Ensure URLs point to the correct release assets

### SHA256 mismatch

If installation fails with a SHA256 mismatch:

1. Verify the release assets were uploaded correctly
2. Re-run the update script with correct checksums
3. Test the formula locally before committing

### Release workflow fails

If the release workflow fails to update the formula:

1. Check the workflow logs for errors
2. Verify the `update-homebrew-formula.sh` script exists and is executable
3. Ensure release binaries are built before the formula update step

## Alternative: In-Repo Tap

Instead of a separate repository, you can host the tap within the main repository:

1. Keep the `homebrew-tap/` directory in the main repo
2. Create a GitHub Pages site or use GitHub's raw content
3. Users install via: `brew tap holon-run/holon https://holon.run/tap`

However, this approach is less common and may not work with all Homebrew versions.

## References

- [Homebrew Formula Cookbook](https://docs.brew.sh/Formula-Cookbook)
- [Homebrew for Python Developers](https://docs.brew.sh/Homebrew-and-Python)
- [Creating a Tap](https://docs.brew.sh/How-to-Create-a-Homebrew-Tap)
