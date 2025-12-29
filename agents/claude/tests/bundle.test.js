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
    throw error;
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
      // Files in tar archive have leading ./ prefix
      const pattern = file.replace(/\//g, '\\/').replace(/\./g, '\\.');
      assert.ok(new RegExp(`^${pattern}$`, 'm').test(output),
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

  test('agent.js can be executed (probe mode)', () => {
    const tmpDir = extractBundle(bundlePath);
    try {
      const agentPath = path.join(tmpDir, 'dist', 'agent.js');
      // Run agent in probe mode (validates it can start and write outputs)
      execSync(`node "${agentPath}" --probe`, {
        cwd: tmpDir,
        env: { ...process.env, NODE_ENV: 'production' },
        stdio: 'pipe',
        timeout: 10000, // 10 second timeout
      });
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

    // Bundle should be between 5MB and 200MB (approximate, adjust as needed)
    assert.ok(sizeMB >= 5, `Bundle is suspiciously small: ${sizeMB.toFixed(2)}MB`);
    assert.ok(sizeMB <= 200, `Bundle is suspiciously large: ${sizeMB.toFixed(2)}MB`);
  });
});
