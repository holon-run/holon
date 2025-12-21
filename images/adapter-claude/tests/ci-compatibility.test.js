import { test, describe } from "node:test";
import assert from "node:assert";
import fs from "fs";
import path from "path";

describe("CI Compatibility", () => {
  test("all test files are present and accessible", () => {
    const testDir = path.dirname(new URL(import.meta.url).pathname);
    const testFiles = [
      "adapter.test.js",
      "ci-compatibility.test.js"
    ];

    for (const testFile of testFiles) {
      const filePath = path.join(testDir, testFile);
      assert.ok(fs.existsSync(filePath), `Test file ${testFile} exists`);
      assert.ok(fs.statSync(filePath).isFile(), `${testFile} is a regular file`);
    }
  });

  test("adapter source file exists for type checking", () => {
    const srcPath = path.join(path.dirname(path.dirname(new URL(import.meta.url).pathname)), "src", "adapter.ts");
    assert.ok(fs.existsSync(srcPath), "Adapter source file exists");
    assert.ok(fs.statSync(srcPath).isFile(), "Adapter source is a regular file");
  });

  test("package.json has correct test scripts", () => {
    const packageJsonPath = path.join(path.dirname(path.dirname(new URL(import.meta.url).pathname)), "package.json");
    const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, "utf8"));

    assert.ok(packageJson.scripts, "package.json has scripts section");
    assert.ok(packageJson.scripts.test, "package.json has test script");
    assert.ok(packageJson.scripts.test.includes("node --test"), "test script uses node:test");
  });
});