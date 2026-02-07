#!/usr/bin/env bash
#
# build-skills-package.sh - Build Holon skill package for distribution
#
# This script creates a versioned skill package archive from the skills/ directory.
# Output: dist/skills/<package>-<version>.zip and .sha256 checksum file
#
# Usage:
#   ./scripts/build-skills-package.sh [version]
#
# Environment variables:
#   VERSION        - Version tag (default: auto-detected from git)
#   PACKAGE_NAME   - Package name (default: "holon-skills")
#   OUTPUT_DIR     - Output directory (default: "dist/skills")
#   SKILLS_DIR     - Source skills directory (default: "skills")

set -euo pipefail

# Script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# Defaults
PACKAGE_NAME="${PACKAGE_NAME:-holon-skills}"
OUTPUT_DIR="${OUTPUT_DIR:-dist/skills}"
SKILLS_DIR="${SKILLS_DIR:-skills}"

# Detect version if not provided
if [[ -z "${VERSION:-}" ]]; then
    if [[ -d .git ]]; then
        VERSION="$(git describe --tags --exact-match 2>/dev/null || git describe --tags --always --dirty 2>/dev/null || echo "v0.0.0-dev")"
        if [[ ! "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+ ]]; then
            VERSION="v0.0.0-dev"
        fi
    else
        VERSION="v0.0.0-dev"
    fi
fi

# Validate version format
if [[ ! "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]]; then
    echo "Error: Invalid version format '$VERSION'. Expected v<major>.<minor>.<patch> or v<major>.<minor>.<patch>-<prerelease>" >&2
    exit 1
fi

# Get commit SHA for reproducibility
COMMIT_SHA=""
if [[ -d .git ]]; then
    COMMIT_SHA="$(git rev-parse HEAD 2>/dev/null || echo "")"
fi

# Derived filenames
ARCHIVE_BASE="${PACKAGE_NAME}-${VERSION}"
ARCHIVE_NAME="${ARCHIVE_BASE}.zip"
CHECKSUM_NAME="${ARCHIVE_BASE}.zip.sha256"

# Make OUTPUT_DIR absolute before changing to temp dir
mkdir -p "${REPO_ROOT}/${OUTPUT_DIR}"
OUTPUT_DIR="$(cd "${REPO_ROOT}/${OUTPUT_DIR}" && pwd)"

OUTPUT_ARCHIVE="${OUTPUT_DIR}/${ARCHIVE_NAME}"
OUTPUT_CHECKSUM="${OUTPUT_DIR}/${CHECKSUM_NAME}"
TEMP_DIR="$(mktemp -d)"
trap "rm -rf '${TEMP_DIR}'" EXIT

echo "Building Holon Skill Package"
echo "============================"
echo "Package:    ${PACKAGE_NAME}"
echo "Version:    ${VERSION}"
echo "Source:     ${SKILLS_DIR}"
echo "Output:     ${OUTPUT_DIR}"
echo ""

# Validate source directory
if [[ ! -d "${REPO_ROOT}/${SKILLS_DIR}" ]]; then
    echo "Error: Skills directory not found: ${REPO_ROOT}/${SKILLS_DIR}" >&2
    exit 1
fi

# Discover all skills
echo "Discovering skills..."
SKILLS=()
while IFS= read -r -d '' skill_dir; do
    skill_name="$(basename "$skill_dir")"
    skill_manifest="${skill_dir}/SKILL.md"

    # Verify skill has SKILL.md
    if [[ ! -f "${skill_manifest}" ]]; then
        echo "Error: Skill directory ${skill_name} missing SKILL.md" >&2
        exit 1
    fi

    # Extract and validate skill name from frontmatter
    if command -v yq &>/dev/null; then
        manifest_name="$(yq '.name' "${skill_manifest}" 2>/dev/null || echo "")"
        if [[ -n "$manifest_name" && "$manifest_name" != "$skill_name" ]]; then
            echo "Warning: Skill directory name '${skill_name}' doesn't match manifest name '${manifest_name}'"
        fi
    fi

    SKILLS+=("$skill_name")
    echo "  ✓ ${skill_name}"
done < <(find "${REPO_ROOT}/${SKILLS_DIR}" -mindepth 1 -maxdepth 1 -type d -print0 | sort -z)

if [[ ${#SKILLS[@]} -eq 0 ]]; then
    echo "Error: No skills found in ${SKILLS_DIR}" >&2
    exit 1
fi

echo ""
echo "Found ${#SKILLS[@]} skill(s)"

# Prepare staging directory
STAGE_DIR="${TEMP_DIR}/${ARCHIVE_BASE}"
mkdir -p "${STAGE_DIR}/skills"

# Copy skills to staging area
echo ""
echo "Staging skills..."
for skill in "${SKILLS[@]}"; do
    echo "  Copying ${skill}..."
    # Use cp -r to copy the skill directory itself (not just contents)
    cp -r "${REPO_ROOT}/${SKILLS_DIR}/${skill}" "${STAGE_DIR}/skills/"
done

# Generate package.json
echo ""
echo "Generating package.json..."

# Generate ISO 8601 timestamp
GENERATED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

# Get remote URL
SOURCE_URL=""
if [[ -d .git ]]; then
    SOURCE_URL="$(git config --get remote.origin.url 2>/dev/null || echo "https://github.com/holon-run/holon")"
    # Convert SSH to HTTPS
    SOURCE_URL="${SOURCE_URL#git@}"
    SOURCE_URL="${SOURCE_URL#https://}"
    SOURCE_URL="${SOURCE_URL%/}"
    SOURCE_URL="${SOURCE_URL/:/\/}"
    SOURCE_URL="https://${SOURCE_URL}"
else
    SOURCE_URL="https://github.com/holon-run/holon"
fi

# Create JSON array of skills
if command -v jq &>/dev/null; then
    SKILLS_JSON="$(printf '%s\n' "${SKILLS[@]}" | jq -R . | jq -s .)"
else
    # Fallback: manually construct JSON array
    SKILLS_JSON="["
    first=true
    for skill in "${SKILLS[@]}"; do
        if [[ "$first" == "true" ]]; then
            first=false
        else
            SKILLS_JSON+=","
        fi
        SKILLS_JSON+="\"$skill\""
    done
    SKILLS_JSON+="]"
fi

# Generate package.json
cat > "${STAGE_DIR}/package.json" <<EOF
{
  "\$schema": "https://schemas.holon.run/skill-package/v1",
  "name": "${PACKAGE_NAME}",
  "version": "${VERSION}",
  "description": "Official Holon builtin skills collection",
  "skills": ${SKILLS_JSON},
  "source": {
    "type": "git",
    "url": "${SOURCE_URL}",
    "ref": "${VERSION}",
    "commit": "${COMMIT_SHA}"
  },
  "generated_at": "${GENERATED_AT}",
  "generator": {
    "name": "holon-build-skills",
    "version": "${VERSION}"
  }
}
EOF

echo "  ✓ package.json"

# Validate package.json against schema if ajv is available
SCHEMA_FILE="${REPO_ROOT}/schemas/skill-package.schema.json"
if [[ -f "${SCHEMA_FILE}" ]]; then
    if command -v ajv &>/dev/null; then
        if ! ajv validate -s "${SCHEMA_FILE}" -d "${STAGE_DIR}/package.json" 2>/dev/null; then
            echo "Warning: package.json schema validation failed" >&2
        else
            echo "  ✓ Schema validated"
        fi
    fi
fi

# Create ZIP archive
echo ""
echo "Creating archive..."
cd "${TEMP_DIR}"
# Use absolute path for output archive
ABSOLUTE_OUTPUT_ARCHIVE="${OUTPUT_ARCHIVE}"
if [[ ! "${ABSOLUTE_OUTPUT_ARCHIVE}" =~ ^/ ]]; then
    ABSOLUTE_OUTPUT_ARCHIVE="$(pwd)/${OUTPUT_ARCHIVE}"
fi

if command -v zip &>/dev/null; then
    # Use zip with recursion for consistent structure
    zip -q -r "${ABSOLUTE_OUTPUT_ARCHIVE}" "${ARCHIVE_BASE}" -x "*.DS_Store"
else
    echo "Error: 'zip' command not found. Please install zip utility." >&2
    exit 1
fi

echo "  ✓ ${OUTPUT_ARCHIVE}"

# Generate SHA256 checksum
echo ""
echo "Generating checksum..."
if command -v sha256sum &>/dev/null; then
    # Linux
    (
        cd "${OUTPUT_DIR}"
        sha256sum "${ARCHIVE_NAME}" > "${CHECKSUM_NAME}"
    )
elif command -v shasum &>/dev/null; then
    # macOS
    (
        cd "${OUTPUT_DIR}"
        shasum -a 256 "${ARCHIVE_NAME}" > "${CHECKSUM_NAME}"
    )
else
    echo "Warning: SHA256 checksum tool not found" >&2
    exit 1
fi

echo "  ✓ ${OUTPUT_CHECKSUM}"

# Display archive info
echo ""
echo "Archive created successfully!"
echo ""
echo "Contents:"
unzip -l "${OUTPUT_ARCHIVE}" | tail -n +4 | head -n -2
echo ""
echo "Checksum:"
cat "${OUTPUT_CHECKSUM}"
echo ""
echo "Files:"
echo "  ${OUTPUT_ARCHIVE}"
echo "  ${OUTPUT_CHECKSUM}"
echo ""
echo "Install with:"
echo "  holon run --goal \"<goal>\" --skill https://github.com/holon-run/holon/releases/download/${VERSION}/${ARCHIVE_NAME}#sha256=\$(cat ${OUTPUT_CHECKSUM} | cut -d' ' -f1)"
echo ""
