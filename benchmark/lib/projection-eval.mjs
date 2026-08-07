import fs from "node:fs/promises";
import { isDeepStrictEqual } from "node:util";

export async function loadProjectionEvalSuite(filePath) {
  const content = await fs.readFile(filePath, "utf8");
  return validateProjectionEvalSuite(JSON.parse(content), { filePath });
}

function projectionMetrics(manifest) {
  const source = manifest.metrics ?? manifest;
  const metrics = {};
  for (const key of [
    "success",
    "verification",
    "tokens",
    "rounds",
    "tool_calls",
    "duration_ms"
  ]) {
    if (key in source) {
      metrics[key] = source[key];
    }
  }
  return metrics;
}

export function validateProjectionEvalSuite(suite, { filePath = "<memory>" } = {}) {
  ensureObject(suite, filePath);
  assertAllowedKeys(
    suite,
    [
      "schema_version",
      "suite_id",
      "baseline_command",
      "candidate_command",
      "repetitions",
      "shared_config",
      "cases"
    ],
    filePath
  );
  requireKeys(suite, ["schema_version", "suite_id", "baseline_command", "cases"], filePath);

  if (suite.schema_version !== 1) {
    throw new Error(`${filePath}.schema_version must be 1`);
  }
  ensureNonEmptyString(suite.suite_id, `${filePath}.suite_id`);
  ensureNonEmptyString(suite.baseline_command, `${filePath}.baseline_command`);
  if ("candidate_command" in suite && suite.candidate_command !== undefined) {
    ensureNonEmptyString(suite.candidate_command, `${filePath}.candidate_command`);
  }
  if ("repetitions" in suite && (!Number.isInteger(suite.repetitions) || suite.repetitions < 1)) {
    throw new Error(`${filePath}.repetitions must be a positive integer`);
  }
  if ("shared_config" in suite) {
    ensureObject(suite.shared_config, `${filePath}.shared_config`);
  }
  if (!Array.isArray(suite.cases) || suite.cases.length === 0) {
    throw new Error(`${filePath}.cases must be a non-empty array`);
  }

  const seenIds = new Set();
  for (const [index, entry] of suite.cases.entries()) {
    const label = `${filePath}.cases[${index}]`;
    ensureObject(entry, label);
    assertAllowedKeys(entry, ["id", "input", "assertions"], label);
    requireKeys(entry, ["id", "input", "assertions"], label);
    ensureNonEmptyString(entry.id, `${label}.id`);
    ensureNonEmptyString(entry.input, `${label}.input`);
    validateAssertions(entry.assertions, `${label}.assertions`);
    if (seenIds.has(entry.id)) {
      throw new Error(`${label}.id must be unique within the suite`);
    }
    seenIds.add(entry.id);
  }

  return suite;
}

export function evaluateProjectionCase({
  caseId,
  assertions,
  baselineManifest,
  candidateManifest = null
}) {
  ensureNonEmptyString(caseId, "caseId");
  validateAssertions(assertions, "assertions");
  ensureObject(baselineManifest, "baselineManifest");
  if (candidateManifest !== null) {
    ensureObject(candidateManifest, "candidateManifest");
  }

  const rows = [];
  let baselinePassedAssertions = 0;
  let candidatePassedAssertions = 0;
  let improvedAssertions = 0;
  let regressedAssertions = 0;
  let changedAssertions = 0;

  for (const [key, expected] of Object.entries(assertions)) {
    const baselineActual = lookupManifestValue(baselineManifest, key);
    const baselinePass = isDeepStrictEqual(baselineActual, expected);
    const candidateActual =
      candidateManifest === null ? undefined : lookupManifestValue(candidateManifest, key);
    const candidatePass =
      candidateManifest === null ? null : isDeepStrictEqual(candidateActual, expected);
    const changed =
      candidateManifest === null ? false : !isDeepStrictEqual(baselineActual, candidateActual);
    const improved = candidateManifest === null ? false : !baselinePass && candidatePass;
    const regressed = candidateManifest === null ? false : baselinePass && !candidatePass;

    if (baselinePass) {
      baselinePassedAssertions += 1;
    }
    if (candidatePass) {
      candidatePassedAssertions += 1;
    }
    if (changed) {
      changedAssertions += 1;
    }
    if (improved) {
      improvedAssertions += 1;
    }
    if (regressed) {
      regressedAssertions += 1;
    }

    rows.push({
      key,
      expected,
      baseline: { actual: baselineActual, pass: baselinePass },
      candidate:
        candidateManifest === null ? null : { actual: candidateActual, pass: candidatePass },
      changed,
      improved,
      regressed
    });
  }

  const totalAssertions = rows.length;
  const baseline = {
    pass: baselinePassedAssertions === totalAssertions,
    passed_assertions: baselinePassedAssertions,
    total_assertions: totalAssertions
  };
  const candidate =
    candidateManifest === null
      ? null
      : {
          pass: candidatePassedAssertions === totalAssertions,
          passed_assertions: candidatePassedAssertions,
          total_assertions: totalAssertions
        };

  return {
    schema_version: 1,
    case_id: caseId,
    ok: candidate ? candidate.pass : baseline.pass,
    baseline,
    candidate,
    metrics: {
      baseline: projectionMetrics(baselineManifest),
      candidate: candidateManifest === null ? null : projectionMetrics(candidateManifest)
    },
    delta:
      candidateManifest === null
        ? null
        : {
            changed_assertions: changedAssertions,
            improved_assertions: improvedAssertions,
            regressed_assertions: regressedAssertions
          },
    assertions: rows
  };
}

function lookupManifestValue(manifest, key) {
  for (const candidate of [manifest?.assertions, manifest?.results, manifest?.signals, manifest]) {
    if (candidate && typeof candidate === "object" && key in candidate) {
      return candidate[key];
    }
  }
  return undefined;
}

function validateAssertions(assertions, label) {
  ensureObject(assertions, label);
  const entries = Object.entries(assertions);
  if (entries.length === 0) {
    throw new Error(`${label} must contain at least one assertion`);
  }
  for (const [key, value] of entries) {
    if (!String(key).trim()) {
      throw new Error(`${label} keys must be non-empty strings`);
    }
    if (value === undefined) {
      throw new Error(`${label}.${key} must not be undefined`);
    }
  }
}

function ensureObject(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
}

function ensureNonEmptyString(value, label) {
  if (typeof value !== "string" || !value.trim()) {
    throw new Error(`${label} must be a non-empty string`);
  }
}

function requireKeys(value, keys, label) {
  for (const key of keys) {
    if (!(key in value)) {
      throw new Error(`${label} is missing required key ${key}`);
    }
  }
}

function assertAllowedKeys(value, allowedKeys, label) {
  for (const key of Object.keys(value)) {
    if (!allowedKeys.includes(key)) {
      throw new Error(`${label} has unsupported key ${key}`);
    }
  }
}
