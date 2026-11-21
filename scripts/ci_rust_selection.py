#!/usr/bin/env python3
"""Select CI's Rust packages from changed paths and Cargo's local dependencies."""

import argparse
import json
import os
import re
import subprocess
from collections import defaultdict
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parent.parent
# Root Rust jobs check local-onnx through its feature and run the runtime
# tests through the memory-bounded library shards.
SKIP_STANDALONE = {"decision-local-onnx", "holon-runtime"}
RUST_TEST_SHARDS = ("lib-a", "lib-b", "control", "cli", "misc")


def crate_manifests():
    return {path.parent.name: path for path in sorted(ROOT.glob("crates/*/Cargo.toml"))}


def test_only(path):
    # Exported test-support features can be used by other packages.
    if "test_support" in path.parts or path.name == "test_support.rs":
        return False
    if path.parts[0] == "tests":
        return True
    if path.parts[0] != "src":
        return False
    return any(part == "tests" or part.endswith("_tests") for part in path.parts[:-1]) or (
        path.suffix == ".rs" and (path.stem == "tests" or path.stem.endswith("_tests"))
    )


def reverse_dependencies(manifests):
    """Use Cargo's declarations, including inactive optional and target edges."""
    normal = defaultdict(set)
    dev = defaultdict(set)
    packages = {path.parent.resolve(): name for name, path in manifests.items()}
    for directory, name in packages.items():
        output = subprocess.check_output(
            ["cargo", "metadata", "--offline", "--no-deps", "--format-version", "1",
             "--manifest-path", str(directory / "Cargo.toml")],
            text=True, timeout=15,
        )
        package, = [
            package for package in json.loads(output)["packages"]
            if Path(package["manifest_path"]).parent.resolve() == directory
        ]
        for dep in package["dependencies"]:
            if not dep.get("path"):
                continue
            supplier = packages[Path(dep["path"]).resolve()]
            reverse = dev if dep["kind"] == "dev" else normal
            reverse[supplier].add(name)
    return normal, dev


def select(paths, manifests, dependency_graph):
    """Return (root checks, standalone crate names or all, standalone tests only)."""
    if not paths:
        return True, None, False
    production = set()
    tests = set()
    for changed in paths:
        path = PurePosixPath(changed)
        if path.name in {"Cargo.toml", "Cargo.lock"}:
            return True, None, False
        if path.parts[:2] == ("docs", "rfcs") and path.suffix == ".md":
            continue
        if path.parts[0] == "src":
            production.add("holon")
        elif path.parts[0] == "tests" and len(path.parts) == 2 and path.suffix == ".rs":
            # Cargo discovers top-level Rust integration targets in the root package.
            tests.add("holon")
        elif len(path.parts) >= 3 and path.parts[0] == "crates" and path.parts[1] in manifests:
            name = path.parts[1]
            if test_only(PurePosixPath(*path.parts[2:])) and name not in SKIP_STANDALONE:
                tests.add(name)
            else:
                production.add(name)
        else:
            # Shared files can affect generated, embedded, or cross-crate inputs.
            return True, None, False

    affected = production.copy()
    if production:
        normal, dev = dependency_graph()
        pending = list(production)
        while pending:
            for consumer in normal.get(pending.pop(), set()) - affected:
                affected.add(consumer)
                pending.append(consumer)
        # Dev dependencies affect the consumer's tests without changing its library.
        dev_consumers = {consumer for name in affected for consumer in dev.get(name, ())}
        affected.update(dev_consumers)
    affected.update(tests)
    root = "holon" in affected
    crates = sorted(affected - {"holon"} - SKIP_STANDALONE)
    return root, crates, bool(crates) and not root and not production


def changed_paths(base):
    if not base or base == "0" * 40:
        raise ValueError("missing comparison base")
    output = subprocess.check_output(
        ["git", "diff", "--no-renames", "--name-only", "-z", base, "HEAD", "--"],
        cwd=ROOT, timeout=15,
    )
    return [os.fsdecode(path) for path in output.split(b"\0") if path]


def cli_source(path):
    return path in (PurePosixPath("src/cli.rs"), PurePosixPath("src/tail.rs")) or (
        path.parts[:2] in (("src", "cli"), ("src", "tail")) and path.suffix == ".rs"
    )


def runtime_test_group(path):
    """Recognize an existing test file with a single explicit module gate."""
    if path.parts[:2] != ("crates", "holon-runtime"):
        return None
    relative = PurePosixPath(*path.parts[2:])
    if relative.suffix != ".rs" or relative.name == "mod.rs" or not test_only(relative):
        return None
    source = ROOT / path
    if not source.is_file():
        return None

    group = r'holon_lib_test_shard = "([abcd])"'
    gates = [
        rf'#\[cfg\(all\(test, {group}\)\)\]',
        rf'#\[cfg\({group}\)\]\s*#\[cfg\(test\)\]',
        rf'#\[cfg\(test\)\]\s*#\[cfg\({group}\)\]',
    ]
    # runtime::tests is itself test-only, but some of its fixture modules
    # are shared across compile groups. Only a gated child can be selected.
    if relative.parent == PurePosixPath("src/runtime/tests"):
        gates.append(rf'#\[cfg\({group}\)\]')
    declaration = re.compile(
        rf'(?m)^(?:{"|".join(gates)})\s*mod {re.escape(source.stem)};[ \t]*$'
    )
    parents = [source.parent / "mod.rs", source.parent.with_suffix(".rs")]
    if relative.parent == PurePosixPath("src"):
        parents.append(source.parent / "lib.rs")
    groups = {
        group
        for parent in parents
        if parent.is_file()
        for match in declaration.finditer(parent.read_text(encoding="utf-8"))
        for group in match.groups()
        if group is not None
    }
    if len(groups) == 1:
        return groups.pop()
    return None


def plan_outputs(paths, manifests, dependency_graph):
    rust_paths = [
        PurePosixPath(path) for path in paths
        if not (path.startswith("docs/rfcs/") and path.endswith(".md"))
    ]
    cli_only = bool(rust_paths) and all(map(cli_source, rust_paths))
    groups = [runtime_test_group(path) for path in rust_paths]
    runtime_tests_only = bool(groups) and all(groups)
    if cli_only:
        root, crates, tests_only = True, [], False
        runtime_groups = []
        shards = ["lib-b", "cli"]
    elif runtime_tests_only:
        root, crates, tests_only = False, [], True
        runtime_groups = sorted(set(groups))
        owners = {"a": "lib-a", "b": "lib-b", "c": "lib-a", "d": "lib-b"}
        shards = sorted({owners[group] for group in runtime_groups})
    else:
        root, crates, tests_only = select(paths, manifests, dependency_graph)
        runtime_groups = list("abcd")
        shards = list(RUST_TEST_SHARDS)

    return {
        "root": root,
        "crates": crates,
        "crate_tests_only": tests_only,
        "cli_only": cli_only,
        "runtime_tests_only": runtime_tests_only,
        "runtime_test_groups": runtime_groups,
        "rust_test_shards": shards,
    }


def plan(base):
    manifests = crate_manifests()
    try:
        outputs = plan_outputs(
            changed_paths(base), manifests,
            lambda: reverse_dependencies({"holon": ROOT / "Cargo.toml", **manifests}),
        )
    except (subprocess.SubprocessError, ValueError, KeyError, OSError) as error:
        print(f"::warning::Rust selection fell back to all checks ({type(error).__name__})")
        outputs = plan_outputs([], manifests, None)
    lines = [f"{key}={json.dumps(value)}\n" for key, value in outputs.items()]
    print("".join(lines), end="")
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a", encoding="utf-8") as file:
            file.writelines(lines)


def run_crates(command):
    manifests = crate_manifests()
    selected = json.loads(os.environ.get("RUST_CRATE_SELECTION") or "null")
    names = sorted(set(manifests) - SKIP_STANDALONE) if selected is None else selected
    for name in names:
        args = ["cargo", command, "--manifest-path", str(manifests[name].relative_to(ROOT)), "--locked"]
        if name == "holon-domain":
            args.extend(["--features", "error-sanitization"])
        subprocess.run(args, check=True, cwd=ROOT)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="action", required=True)
    plan_parser = subcommands.add_parser("plan")
    plan_parser.add_argument("base")
    run_parser = subcommands.add_parser("run")
    run_parser.add_argument("mode", choices=["check", "test"])
    args = parser.parse_args()
    if args.action == "plan":
        plan(args.base)
    else:
        run_crates(args.mode)


if __name__ == "__main__":
    main()
