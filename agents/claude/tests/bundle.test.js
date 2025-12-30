import { execSync } from 'child_process';
import fs from 'fs';
import path from 'path';
import os from 'os';
import { test, describe } from 'node:test';
import assert from 'node:assert';

// Helper function to get bundle path
function getBundlePath() {
  // Tests are run from agents/claude directory
  const rootDir = process.cwd();
  const pkg = JSON.parse(fs.readFileSync(path.join(rootDir, 'package.json'), 'utf8'));

  const name = process.env.BUNDLE_NAME || 'agent-claude';
  const version = process.env.BUNDLE_VERSION || pkg.version || '0.0.0';
  const platform = process.env.BUNDLE_PLATFORM || 'linux';
  const arch = process.env.BUNDLE_ARCH || 'amd64';
  const libc = process.env.BUNDLE_LIBC || 'glibc';
  const outputDir = process.env.BUNDLE_OUTPUT_DIR || path.join(rootDir, 'dist', 'agent-bundles');

  return path.join(outputDir, `agent-bundle-${name}-${version}-${platform}-${arch}-${libc}.tar.gz`);
}

// Helper function to extract bundle to temp directory
function extractBundle(bundlePath) {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bundle-test-'));
  try {
    execSync(`tar -xzf "${bundlePath}" -C "${tmpDir}"`, { stdio: 'pipe' });
    return tmpDir;
  } catch (error) {
    fs.rmSync(tmpDir, { recursive: true, force: true });
    throw new Error(`Failed to extract bundle: ${error.message}`);
  }
}

describe('Agent Bundle', () => {
  const bundlePath = getBundlePath();

  test('bundle exists', () => {
    assert.strictEqual(fs.existsSync(bundlePath), true, `Bundle not found at ${bundlePath}`);
  });

  test('bundle is a valid tar.gz archive', () => {
    // Try to list archive contents
    const output = execSync(`tar -tzf "${bundlePath}"`, { stdio: 'pipe' }).toString();
    assert.ok(output.length > 0, 'Archive appears to be empty or invalid');
  });

  test('bundle contains package.json', () => {
    const output = execSync(`tar -tzf "${bundlePath}"`, { stdio: 'pipe' }).toString();
    assert.ok(/^\.\/package\.json$/m.test(output), 'package.json not found in bundle');
  });

  test('bundle contains all required files', () => {
    const output = execSync(`tar -tzf "${bundlePath}"`, { stdio: 'pipe' }).toString();
    const required = [
      './package.json',
      './manifest.json',
      './dist/agent.js',
      './bin/agent',
      './node_modules/@anthropic-ai/claude-agent-sdk/package.json'
    ];

    for (const file of required) {
      // Files in tar archive have leading ./ prefix; escape regex metacharacters for exact line match
      const pattern = new RegExp('^' + file.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '$', 'm');
      assert.ok(pattern.test(output),
        `Required file not found in bundle: ${file}`);
    }
  });

  test('package.json has type: module', () => {
    const tmpDir = extractBundle(bundlePath);
    try {
      const pkgPath = path.join(tmpDir, 'package.json');
      const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
      assert.strictEqual(pkg.type, 'module', 'package.json must have type: module for ES modules');
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  test('package.json is valid JSON', () => {
    const tmpDir = extractBundle(bundlePath);
    try {
      const pkgPath = path.join(tmpDir, 'package.json');
      // Should not throw
      JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  test('manifest.json is valid JSON', () => {
    const tmpDir = extractBundle(bundlePath);
    try {
      const manifestPath = path.join(tmpDir, 'manifest.json');
      const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
      assert.ok(manifest.bundleVersion, 'manifest must have bundleVersion');
      assert.ok(manifest.name, 'manifest must have name');
      assert.ok(manifest.version, 'manifest must have version');
      assert.ok(manifest.entry, 'manifest must have entry');
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  test('bin/agent is executable', () => {
    const tmpDir = extractBundle(bundlePath);
    try {
      const agentPath = path.join(tmpDir, 'bin', 'agent');
      try {
        fs.accessSync(agentPath, fs.constants.X_OK);
      } catch {
        assert.fail('bin/agent is not executable');
      }
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  test('agent.js syntax is valid', () => {
    const tmpDir = extractBundle(bundlePath);
    try {
      const agentPath = path.join(tmpDir, 'dist', 'agent.js');
      // Use node -c to check syntax without executing
      execSync(`node -c "${agentPath}"`, { stdio: 'pipe' });
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  test('agent.js can be executed (probe mode)', { timeout: 60000 }, () => {
    const tmpDir = extractBundle(bundlePath);

    // Check if we're already running in a Holon container environment
    const inHolonContainer = fs.existsSync('/holon/workspace') &&
                             fs.existsSync('/holon/input') &&
                             fs.existsSync('/holon/output');

    try {
      if (inHolonContainer) {
        // Use existing Holon environment
        console.log('  ℹ Running in Holon container environment');
        const agentPath = path.join(tmpDir, 'dist', 'agent.js');
        const output = execSync(`node "${agentPath}" --probe`, {
          cwd: tmpDir,
          env: { ...process.env, NODE_ENV: 'production' },
          stdio: 'pipe',
          timeout: 30000,
        }).toString();

        assert.ok(output.includes('Probe completed'), 'Agent probe did not complete successfully');
      } else {
        // Need to create Holon environment
        const holonDir = fs.mkdtempSync(path.join(os.tmpdir(), 'holon-test-'));
        const inputDir = path.join(holonDir, 'input');
        const workspaceDir = path.join(holonDir, 'workspace');
        const outputDir = path.join(holonDir, 'output');

        try {
          // Create minimal Holon environment
          fs.mkdirSync(inputDir, { recursive: true });
          fs.mkdirSync(workspaceDir, { recursive: true });
          fs.mkdirSync(outputDir, { recursive: true });

          // Create minimal spec.yaml
          fs.writeFileSync(
            path.join(inputDir, 'spec.yaml'),
            `version: "v1"
kind: Holon
metadata:
  name: "bundle-test-probe"
context:
  workspace: "/holon/workspace"
goal:
  description: "Bundle test probe validation"
output:
  artifacts:
    - path: "manifest.json"
      required: true
`
          );

          // Create minimal workspace file
          fs.writeFileSync(path.join(workspaceDir, 'README.md'), 'Bundle test workspace\n');

          // Check if Docker is available
          let dockerAvailable = false;
          try {
            execSync('docker --version', { stdio: 'pipe' });
            dockerAvailable = true;
          } catch {
            // Docker not available, skip test
            console.log('  ⚠ WARNING: Docker not available and not in Holon container, skipping agent probe test');
            return;
          }

          if (!dockerAvailable) {
            return;
          }

          // Determine Node version
          const nodeVersion = process.env.BUNDLE_NODE_VERSION || '20';
          const image = process.env.BUNDLE_VERIFY_IMAGE || `node:${nodeVersion}-bookworm-slim`;

          // Run agent probe test in Docker container
          const output = execSync(
            `docker run --rm \
             -v "${inputDir}:/holon/input:ro" \
             -v "${workspaceDir}:/holon/workspace:ro" \
             -v "${outputDir}:/holon/output" \
             -v "${tmpDir}:/holon/agent:ro" \
             --entrypoint /bin/sh \
             "${image}" -c "cd /holon/agent && NODE_ENV=production node dist/agent.js --probe"`,
            {
              stdio: 'pipe',
              timeout: 60000,
            }
          ).toString();

          // Verify probe completed successfully
          assert.ok(output.includes('Probe completed'), 'Agent probe did not complete successfully');

          // Verify manifest.json was written
          const manifestPath = path.join(outputDir, 'manifest.json');
          assert.ok(fs.existsSync(manifestPath), 'Agent probe did not write manifest.json');

          const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
          assert.strictEqual(manifest.status, 'completed', 'Manifest status should be completed');
          assert.strictEqual(manifest.outcome, 'success', 'Manifest outcome should be success');
        } finally {
          fs.rmSync(holonDir, { recursive: true, force: true });
        }
      }
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  test('node_modules are bundled', () => {
    const tmpDir = extractBundle(bundlePath);
    try {
      const nodeModulesPath = path.join(tmpDir, 'node_modules');
      assert.ok(fs.existsSync(nodeModulesPath), 'node_modules directory not found in bundle');

      // Check for critical dependencies
      const criticalDeps = [
        '@anthropic-ai/claude-agent-sdk',
        'yaml',
        'zod',
      ];

      for (const dep of criticalDeps) {
        const depPath = path.join(nodeModulesPath, dep);
        assert.ok(fs.existsSync(depPath), `Critical dependency not bundled: ${dep}`);
      }
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  test('bundle has reasonable size', () => {
    const stats = fs.statSync(bundlePath);
    const sizeMB = stats.size / (1024 * 1024);

    // Sanity check: bundle size should be within a reasonable range.
    // Defaults are 5MB–200MB but can be overridden via env vars to adapt to
    // different deployment targets or build configurations.
    const minSizeMB = Number(process.env.BUNDLE_MIN_SIZE_MB) || 5;
    const maxSizeMB = Number(process.env.BUNDLE_MAX_SIZE_MB) || 200;

    assert.ok(sizeMB >= minSizeMB, `Bundle is suspiciously small: ${sizeMB.toFixed(2)}MB (min: ${minSizeMB}MB)`);
    assert.ok(sizeMB <= maxSizeMB, `Bundle is suspiciously large: ${sizeMB.toFixed(2)}MB (max: ${maxSizeMB}MB)`);
  });
});
