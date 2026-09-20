import { execFile } from "node:child_process";
import {
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import openapiTS, { astToString } from "openapi-typescript";

const execFileAsync = promisify(execFile);
const schemaUrl = new URL("../../docs/website/reference/openapi.json", import.meta.url);
const typescriptOutputUrl = new URL(
  "../app/src/runtime/generated/openapi.ts",
  import.meta.url,
);
const kotlinOutputUrl = new URL(
  "../../packages/client-wire-kotlin/src/main/kotlin/run/holon/client/wire/generated/models/",
  import.meta.url,
);
const toolsDirectory = dirname(fileURLToPath(import.meta.url));
const kotlinPackage = "run.holon.client.wire.generated.models";
const kotlinRoots = [
  "ErrorResponse",
  "HandshakeResponse",
  "AgentListResponse",
  "CurrentUserResponse",
  "SessionExchangeRequest",
  "SessionResponse",
  "NativeSessionResponse",
  "ControlPromptRequest",
  "EnqueueResponse",
];
const check = process.argv.includes("--check");
const typescriptHeader = [
  "// Generated from docs/website/reference/openapi.json by web-gui/openapi-tools.",
  "// Do not edit by hand. Run `make transport-types` from the repository root.",
  "",
].join("\n");

function collectSchemaClosure(schemas, roots) {
  const selected = new Set();
  const pending = [...roots];

  while (pending.length > 0) {
    const name = pending.pop();
    if (selected.has(name)) {
      continue;
    }
    const schema = schemas[name];
    if (!schema) {
      throw new Error(`Missing client wire schema: ${name}`);
    }
    selected.add(name);

    const visit = (value) => {
      if (Array.isArray(value)) {
        value.forEach(visit);
        return;
      }
      if (!value || typeof value !== "object") {
        return;
      }
      const reference = value.$ref;
      const prefix = "#/components/schemas/";
      if (typeof reference === "string" && reference.startsWith(prefix)) {
        pending.push(reference.slice(prefix.length));
      }
      Object.values(value).forEach(visit);
    };

    visit(schema);
  }

  return [...selected].sort();
}

function renderKotlinArrayAlias(name, schema) {
  const reference = schema.items?.$ref;
  const prefix = "#/components/schemas/";
  if (schema.type !== "array" || !reference?.startsWith(prefix)) {
    throw new Error(
      `Kotlin array alias ${name} must reference a named item schema`,
    );
  }
  const itemType = reference.slice(prefix.length);
  return [
    "// Generated from docs/website/reference/openapi.json by web-gui/openapi-tools.",
    "// Do not edit by hand. Run `make transport-types` from the repository root.",
    "",
    `package ${kotlinPackage}`,
    "",
    `typealias ${name} = List<${itemType}>`,
    "",
  ].join("\n");
}

const kotlinKeywords = new Set([
  "as",
  "break",
  "class",
  "continue",
  "do",
  "else",
  "false",
  "for",
  "fun",
  "if",
  "in",
  "interface",
  "is",
  "null",
  "object",
  "package",
  "return",
  "super",
  "this",
  "throw",
  "true",
  "try",
  "typealias",
  "typeof",
  "val",
  "var",
  "when",
  "while",
]);

function renderKotlinIdentifier(value) {
  if (/^[A-Za-z_][A-Za-z0-9_]*$/.test(value) && !kotlinKeywords.has(value)) {
    return value;
  }
  if (!value.includes("`") && !value.includes("\n")) {
    return `\`${value}\``;
  }
  throw new Error(`Unsupported Kotlin wire enum value: ${value}`);
}

function renderKotlinOpenEnum(name, schema) {
  if (schema.type !== "string" || !Array.isArray(schema.enum)) {
    throw new Error(`Kotlin open enum ${name} must be a string enum`);
  }
  const knownValues = schema.enum.flatMap((value) => [
    `        val ${renderKotlinIdentifier(value)}: ${name} =`,
    `            ${name}(${JSON.stringify(value)})`,
    "",
  ]);
  return [
    "// Generated from docs/website/reference/openapi.json by web-gui/openapi-tools.",
    "// Do not edit by hand. Run `make transport-types` from the repository root.",
    "",
    `package ${kotlinPackage}`,
    "",
    "import kotlinx.serialization.Serializable",
    "",
    "/**",
    " * An open wire enum. Known values are exposed as constants, while unknown",
    " * values remain decodable so newer runtimes stay compatible with this client.",
    " */",
    "@JvmInline",
    "@Serializable",
    `value class ${name}(val value: kotlin.String) {`,
    "    override fun toString(): kotlin.String = value",
    "",
    "    companion object {",
    ...knownValues,
    "    }",
    "}",
    "",
  ].join("\n");
}

async function readKotlinFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true }).catch(() => []);
  const files = new Map();
  for (const entry of entries) {
    if (entry.isFile() && entry.name.endsWith(".kt")) {
      const contents = await readFile(join(directory, entry.name), "utf8");
      files.set(
        entry.name,
        contents.replace(/[ \t]+$/gm, "").replace(/\n+$/, "\n"),
      );
    }
  }
  return files;
}

function reportKotlinDrift(current, generated) {
  const names = new Set([...current.keys(), ...generated.keys()]);
  return [...names]
    .sort()
    .filter((name) => current.get(name) !== generated.get(name));
}

async function generateKotlinModels(openapi) {
  const schemas = openapi.components?.schemas;
  if (!schemas) {
    throw new Error("OpenAPI document does not define components.schemas");
  }

  const selectedNames = collectSchemaClosure(schemas, kotlinRoots);
  const aliases = selectedNames.filter((name) => schemas[name].type === "array");
  const openEnums = selectedNames.filter(
    (name) =>
      schemas[name].type === "string" && Array.isArray(schemas[name].enum),
  );
  const modelNames = selectedNames.filter((name) => schemas[name].type !== "array");
  const reducedOpenapi = {
    openapi: openapi.openapi,
    info: {
      title: "Holon client wire models",
      version: openapi.info.version,
    },
    paths: {},
    components: {
      schemas: Object.fromEntries(
        selectedNames.map((name) => [name, schemas[name]]),
      ),
    },
  };

  const temporaryRoot = await mkdtemp(join(tmpdir(), "holon-client-wire-"));
  try {
    const inputPath = join(temporaryRoot, "openapi.json");
    const outputPath = join(temporaryRoot, "generated");
    await writeFile(inputPath, `${JSON.stringify(reducedOpenapi, null, 2)}\n`);
    await execFileAsync(
      join(toolsDirectory, "node_modules", ".bin", "openapi-generator-cli"),
      [
        "generate",
        "-g",
        "kotlin",
        "-i",
        inputPath,
        "-o",
        outputPath,
        "--global-property",
        `models=${modelNames.join(":")},modelDocs=false,modelTests=false,apis=false,supportingFiles=false`,
        "--additional-properties",
        `packageName=${kotlinPackage.replace(/\.models$/, "")},serializationLibrary=kotlinx_serialization,library=jvm-retrofit2,generateOneOfAnyOfWrappers=true,dateLibrary=string`,
      ],
      { cwd: toolsDirectory },
    );

    const generatedDirectory = join(
      outputPath,
      "src",
      "main",
      "kotlin",
      ...kotlinPackage.split("."),
    );
    const generated = await readKotlinFiles(generatedDirectory);
    for (const name of aliases) {
      generated.set(`${name}.kt`, renderKotlinArrayAlias(name, schemas[name]));
    }
    for (const name of openEnums) {
      generated.set(`${name}.kt`, renderKotlinOpenEnum(name, schemas[name]));
    }
    return generated;
  } finally {
    await rm(temporaryRoot, { force: true, recursive: true });
  }
}

const ast = await openapiTS(schemaUrl);
const generatedTypescript = `${typescriptHeader}${astToString(ast)}`;
const openapi = JSON.parse(await readFile(schemaUrl, "utf8"));
const generatedKotlin = await generateKotlinModels(openapi);

if (check) {
  const currentTypescript = await readFile(typescriptOutputUrl, "utf8").catch(
    () => "",
  );
  const currentKotlin = await readKotlinFiles(fileURLToPath(kotlinOutputUrl));
  const staleKotlin = reportKotlinDrift(currentKotlin, generatedKotlin);
  if (currentTypescript !== generatedTypescript || staleKotlin.length > 0) {
    console.error(
      "Generated transport types are stale. Run `make transport-types` and commit the result.",
    );
    if (staleKotlin.length > 0) {
      console.error(`Stale Kotlin files: ${staleKotlin.join(", ")}`);
    }
    process.exitCode = 1;
  }
} else {
  await mkdir(dirname(fileURLToPath(typescriptOutputUrl)), { recursive: true });
  await writeFile(typescriptOutputUrl, generatedTypescript);

  const kotlinDirectory = fileURLToPath(kotlinOutputUrl);
  await rm(kotlinDirectory, { force: true, recursive: true });
  await mkdir(kotlinDirectory, { recursive: true });
  for (const [name, contents] of generatedKotlin) {
    await writeFile(join(kotlinDirectory, name), contents);
  }
}
