#!/bin/bash
# Test script to verify that HTML/markdown in COMMENT_BODY is handled safely
# This simulates the gate execution step's trigger detection logic

set -euo pipefail

# Test cases
test_cases=(
    # Simple trigger
    "@holonbot"
    # Trigger with goal
    "@holonbot fix this bug"
    # Cloudflare Pages bot comment (HTML table example)
    "<table>
  <thead>
    <tr>
      <th>Deployment</th>
      <th>URL</th>
      <th>Created</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td><a href=\"https://github.com/user/repo/deployments\">abc123</a></td>
      <td><a href=\"https://abc123.pages.dev\">https://abc123.pages.dev</a></td>
      <td>2025-12-29 at 1:23am</td>
    </tr>
  </tbody>
</table>"
    # HTML with special characters that could break bash
    "<div>Test &amp; Check &lt;tag&gt; &quot;quoted&quot;</div>"
    # Multiline HTML
    "<!--
    <html>
      <body>Content</body>
    </html>
    -->"
    # Normal comment without trigger
    "This is a normal comment"
    # Comment with @holonbot in the middle (not a trigger)
    "I think @holonbot should look at this"
    # HTML with potential delimiter collision (testing against heredoc issues)
    "<div>__HOLON_COMMENT__</div>"
)

# Expected results: 1 = should trigger, 0 = should not trigger
expected=(
    1  # "@holonbot"
    1  # "@holonbot fix this bug"
    0  # Cloudflare HTML table
    0  # HTML with special chars
    0  # Multiline HTML
    0  # Normal comment
    0  # @holonbot in middle
    0  # HTML with delimiter collision
)

passed=0
failed=0

echo "Testing HTML/markdown handling in trigger parsing..."
echo "==================================================="
echo

for i in "${!test_cases[@]}"; do
    comment_body="${test_cases[$i]}"
    expect="${expected[$i]}"

    # Simulate the base64 encoding/decoding from the workflow
    comment_b64="$(printf '%s' "$comment_body" | base64 -w 0)"
    decoded_body="$(printf '%s' "$comment_b64" | base64 -d)"

    # Verify round-trip encoding
    if [ "$decoded_body" != "$comment_body" ]; then
        echo "❌ Test $((i+1)) FAILED: Base64 round-trip failed"
        echo "   Original: ${comment_body:0:50}..."
        echo "   Decoded:  ${decoded_body:0:50}..."
        failed=$((failed+1))
        continue
    fi

    # Simulate the trigger detection logic from the gate step
    first_line="$(printf '%s\n' "$decoded_body" | head -n1 | xargs || true)"
    should_trigger=0

    if [[ "$first_line" == "@holonbot" ]] || [[ "$first_line" == "@holonbot "* ]]; then
        should_trigger=1
    fi

    # Check result
    test_num=$((i+1))
    if [ "$should_trigger" -eq "$expect" ]; then
        echo "✓ Test $test_num PASSED"
        if [ "$expect" -eq 1 ]; then
            echo "   Correctly detected trigger: $first_line"
        else
            echo "   Correctly ignored: ${first_line:0:50}..."
        fi
        passed=$((passed+1))
    else
        echo "❌ Test $test_num FAILED"
        echo "   Comment: ${first_line:0:50}..."
        echo "   Expected trigger=$expect, got trigger=$should_trigger"
        failed=$((failed+1))
    fi
    echo
done

echo "==================================================="
echo "Results: $passed passed, $failed failed"

if [ "$failed" -eq 0 ]; then
    echo "✓ All tests passed!"
    exit 0
else
    echo "✗ Some tests failed!"
    exit 1
fi
