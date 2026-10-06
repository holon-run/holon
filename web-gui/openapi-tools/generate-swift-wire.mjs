import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const toolsDirectory = dirname(fileURLToPath(import.meta.url));
const outputDirectory = fileURLToPath(new URL(
  "../../packages/client-sdk-swift/Sources/HolonWire/Generated/", import.meta.url,
));
const roots = ["HandshakeResponse", "AgentListResponse", "ErrorResponse", "SessionResponse"];
const execFileAsync = promisify(execFile);

function schemaClosure(schemas) {
  const selected = new Set();
  const pending = [...roots];
  const visit = (value) => {
    if (Array.isArray(value)) return value.forEach(visit);
    if (!value || typeof value !== "object") return;
    if (value.$ref?.startsWith("#/components/schemas/")) {
      pending.push(value.$ref.slice("#/components/schemas/".length));
    }
    Object.values(value).forEach(visit);
  };
  while (pending.length) {
    const name = pending.pop();
    if (selected.has(name)) continue;
    if (!schemas[name]) throw new Error(`Missing Swift wire schema: ${name}`);
    selected.add(name);
    visit(schemas[name]);
  }
  return [...selected].sort();
}

async function swiftFiles(directory) {
  const names = await readdir(directory).catch((error) => {
    if (error.code === "ENOENT") return [];
    throw error;
  });
  return new Map(await Promise.all(names.filter((name) => name.endsWith(".swift"))
    .map(async (name) => [name, await readFile(join(directory, name), "utf8")])));
}

function referencedAnyOf(name, schema, schemas) {
  // The Swift5 generator flattens anyOf into a struct requiring every branch.
  // Keep untagged variants ordered, matching the server's serde decoding.
  const variants = schema.anyOf.map((branch) => {
    const reference = branch.$ref;
    if (Object.keys(branch).length !== 1 || !reference?.startsWith("#/components/schemas/")) {
      throw new Error(`Unsupported Swift anyOf branch: ${name}`);
    }
    const type = reference.split("/").at(-1);
    if (!/^[A-Z][A-Za-z0-9]*$/.test(type) || schemas[type]?.type !== "object") {
      throw new Error(`Unsupported Swift anyOf model: ${name}.${type}`);
    }
    return { type, caseName: type[0].toLowerCase() + type.slice(1) };
  });
  if (!variants.length || new Set(variants.map(({ type }) => type)).size !== variants.length
    || schema.discriminator || schema.properties || schema.required || schema.allOf || schema.oneOf) {
    throw new Error(`Unsupported Swift anyOf schema: ${name}`);
  }
  return [
    "// Generated from docs/website/reference/openapi.json. Do not edit.",
    "import Foundation", "",
    `public enum ${name}: Codable, JSONEncodable {`,
    ...variants.map(({ type, caseName }) => `    case ${caseName}(${type})`),
    "",
    "    public init(from decoder: Decoder) throws {",
    "        let container = try decoder.singleValueContainer()",
    ...variants.flatMap(({ type, caseName }) => [
      `        if let value = try? container.decode(${type}.self) {`,
      `            self = .${caseName}(value)`,
      "            return",
      "        }",
    ]),
    `        throw DecodingError.dataCorruptedError(in: container, debugDescription: "No matching anyOf branch for ${name}")`,
    "    }", "",
    "    public func encode(to encoder: Encoder) throws {",
    "        var container = encoder.singleValueContainer()",
    "        switch self {",
    ...variants.map(({ caseName }) => `        case .${caseName}(let value): try container.encode(value)`),
    "        }",
    "    }",
    "}", "",
  ].join("\n");
}

export async function generateSwiftWire(openapi, check) {
  const schemas = openapi.components.schemas;
  const selected = schemaClosure(schemas);
  const scratch = await mkdtemp(join(tmpdir(), "holon-swift-wire-"));
  try {
    const input = join(scratch, "openapi.json");
    await writeFile(input, JSON.stringify({
      openapi: openapi.openapi, info: openapi.info, paths: {},
      components: { schemas: Object.fromEntries(selected.map((name) => [name, schemas[name]])) },
    }));
    await execFileAsync(join(toolsDirectory, "node_modules/.bin/openapi-generator-cli"), [
      "generate", "-g", "swift5", "-i", input, "-o", join(scratch, "output"),
      "--global-property", "models,modelDocs=false,modelTests=false,apis=false,supportingFiles=false",
      "--additional-properties",
      "projectName=HolonWire,useSPMFileStructure=true,enumUnknownDefaultCase=true,validatable=false,hashableModels=false",
    ], { cwd: toolsDirectory });
    const generated = await swiftFiles(join(scratch, "output/Sources/HolonWire/Models"));
    for (const [name, content] of generated) {
      generated.set(name, content.replace(/[ \t]+$/gm, "").replace(/\n+$/, "\n"));
    }
    for (const name of selected.filter((name) => schemas[name].anyOf)) {
      generated.set(`${name}.swift`, referencedAnyOf(name, schemas[name], schemas));
    }
    for (const name of selected.filter((name) => schemas[name].type === "array")) {
      const reference = schemas[name].items?.$ref;
      if (!reference) throw new Error(`Unsupported Swift array alias: ${name}`);
      generated.set(`${name}.swift`, [
        "// Generated from docs/website/reference/openapi.json. Do not edit.",
        `public typealias ${name} = [${reference.split("/").at(-1)}]`, "",
      ].join("\n"));
    }
    if (check) {
      const current = await swiftFiles(outputDirectory);
      const stale = [...new Set([...current.keys(), ...generated.keys()])]
        .sort().filter((name) => current.get(name) !== generated.get(name));
      if (stale.length) throw new Error(`Stale Swift wire models: ${stale.join(", ")}`);
    } else {
      await rm(outputDirectory, { recursive: true, force: true });
      await mkdir(outputDirectory, { recursive: true });
      for (const [name, content] of generated) await writeFile(join(outputDirectory, name), content);
    }
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const openapi = JSON.parse(await readFile(new URL("../../docs/website/reference/openapi.json", import.meta.url)));
  await generateSwiftWire(openapi, process.argv.includes("--check"));
}
