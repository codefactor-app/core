# Codefactor

[Русский](README.md) · **English**

**Understand code structure, trace dependencies and find maintenance problems before refactoring.**

Codefactor is an open-source static analyzer for **PHP, Rust, Python, JavaScript and TypeScript**. It combines code metrics, call graphs, architecture checks and symbolic execution-cost estimates in a single executable, with a CLI for automation and a local web interface for exploration.

Analysis runs on your machine and reads source files without executing them. You can inspect a checkout without building the analyzed project or running its application. The web interface is embedded in the executable; no separate frontend installation, external CDN or hosted service is required.

[Download releases](https://github.com/codefactor-app/core/releases) · [Report an issue](https://github.com/codefactor-app/core/issues) · [Related repositories](https://github.com/codefactor-app) · [MIT license](LICENSE)

## Contents

- [Why use Codefactor?](#why-use-codefactor)
- [What it analyzes](#what-it-analyzes)
- [Quick start](#quick-start)
- [Explore in the browser](#explore-in-the-browser)
- [CLI workflows](#cli-workflows)
- [Configuration](#configuration)
- [Reports and CI](#reports-and-ci)
- [Companion packages](#companion-packages)
- [How it works](#how-it-works)
- [Analysis boundaries](#analysis-boundaries)
- [Development and contributing](#development-and-contributing)

## Why use Codefactor?

- **Explore an unfamiliar repository.** Browse source, functions, modules and their relationships from the same local interface.
- **Prepare a refactoring.** Trace callers and callees, inspect paths between functions and compare dependencies between two parts of a project.
- **Find maintenance hotspots.** Locate large functions, deep nesting, high complexity, excessive coupling and duplicated function bodies.
- **Review architecture.** Identify dependency cycles and check explicitly configured layer boundaries against source evidence.
- **Introduce quality checks gradually.** Set project-specific thresholds, record an existing baseline and export findings for CI or other tools.

## What it analyzes

One engine handles all five languages, including mixed JavaScript/TypeScript projects. Choose the scan root and enabled languages to control the scope.

| Area | Available information |
| --- | --- |
| Size and structure | File and function length, parameter counts, methods and fields per class, nesting depth and return points |
| Complexity and maintainability | Cyclomatic and cognitive complexity, Halstead volume, maintainability index and comment ratio |
| Coupling and cohesion | Fan-in/fan-out, inheritance depth, LCOM4 cohesion and module cycles |
| Duplication | Token-based duplicate function detection and structural copy findings |
| Call relationships | Callers, callees, paths, source callsites, branch conditions and resolution confidence |
| Architecture | Dependency cycles, configured layer violations and experimental design checks |
| Algorithmic cost | Symbolic worst-case growth estimates, including supported loops and resolved callees |
| External operations | Recognized database, queue, shell, HTTP and TCP calls, with configurable matching rules |

Metrics can be aggregated by project, directory, module or file. Findings include source locations and the relevant values and thresholds. Supported constructs and resolution confidence vary by language; see [analysis boundaries](#analysis-boundaries).

## Quick start

### Download a binary

Get the executable for your platform from [GitHub Releases](https://github.com/codefactor-app/core/releases). Releases include platform manifests and a `SHA256SUMS` file for verifying downloads.

| Platform | x64 | ARM64 |
| --- | --- | --- |
| Linux | `codefactor-linux-x64` | `codefactor-linux-arm64` |
| macOS | `codefactor-darwin-x64` | `codefactor-darwin-arm64` |
| Windows | `codefactor-win32-x64.exe` | `codefactor-win32-arm64.exe` |

Rename the downloaded file to `codefactor` (`codefactor.exe` on Windows) and put it on your `PATH`. On Linux and macOS, make it executable:

```sh
chmod +x codefactor
./codefactor --help
```

From the project you want to analyze:

```sh
codefactor scan .
codefactor serve . --port 8080 --open --watch
```

`scan` prints a summary, metrics and findings. `serve` opens the local web interface; `--watch` refreshes analysis after source changes. Open the URL printed by the server, normally `http://127.0.0.1:8080/`.

No configuration file is required for a first scan. Prefer the project root as the scan root so project metadata remains available for resolving imports and dependencies. Use [path filters](#configuration) to restrict analysis to your source directories.

### Build from source

Install the [Rust stable toolchain](https://www.rust-lang.org/tools/install) and a C compiler toolchain for the Tree-sitter parsers, then run:

```sh
git clone https://github.com/codefactor-app/core.git
cd core
cargo build --release --locked -p analyzer-cli --bin codefactor
./target/release/codefactor --help
./target/release/codefactor scan /path/to/project
./target/release/codefactor serve /path/to/project --port 8080 --watch
```

On Windows, the executable is `target\release\codefactor.exe`. Node.js is only needed for web model development tests; it is not required to build or run the CLI.

## Explore in the browser

The local interface connects five views of the analyzed project:

| View | What you can do |
| --- | --- |
| Code and structure | Browse files, read source with finding highlights, inspect functions and jump to their call graphs |
| Code map | Explore nested dependency blocks or a matrix, grouped by folders, namespaces/modules or configured sets |
| Findings | Filter rule violations, inspect source evidence and follow findings back to the code |
| Architecture | Review structural findings, affected entities and conditional design recommendations |
| Analysis criteria | Inspect the active configuration, metric definitions and rule thresholds |

Function graphs let you expand callers and callees, inspect callsites and conditions, and distinguish resolved, ambiguous and dynamic calls. Set relations show direct or transitive connections between selected directories, modules, classes or named groups.

Graph exports include JSON, Markdown, Mermaid, SVG and PNG. Code maps and set relations also offer image exports for reviews and documentation. All web scripts and styles are served from the binary.

The server binds to `127.0.0.1` by default. Its read-only interface serves project source to connected clients; choose a different `--host` only when you intend to make that source reachable. The current web interface and default console reports are in Russian.

## CLI workflows

Run these commands from the analyzed project's root, with `codefactor` on your `PATH`. Replace example file paths and qualified function names with declarations from your project.

### Scan and compare metrics

```sh
codefactor scan .
codefactor scan . --lang python,typescript --all
codefactor scan . --format json --out report.json
codefactor metrics . --group-by directory
codefactor metrics . --group-by file --format json --out metrics.json
codefactor ast ./src/main.py --format json --out ast.json
```

Console scans show up to 50 findings by default. `--all` shows every finding; `--no-explain` hides metric explanations. Exported reports retain all findings regardless of the console display limit. Detailed syntax trees are generated separately with `ast`, rather than repeated in every scan report.

### Trace calls and dependencies

```sh
codefactor graph callers 'app.service.Worker.run' --depth 3 --format mermaid
codefactor graph callees 'app.service.Worker.run' --with-conditions --format json
codefactor graph paths --from 'app.main' --to 'app.storage.save' --shortest
codefactor relations --a 'dir:src/api' --b 'dir:src/storage' --transitive 3 --format md
codefactor external list --category db
```

Use `--root /path/to/project` with graph, relations and external queries when running outside the project directory. Graph selectors use the declaration's qualified name or ID; the source view and JSON reports help identify them. Relation selectors include `dir:`, `file:`, `glob:`, `ns:`, `mod:`, `pkg:`, `class:` and `set:`.

Run `codefactor --help` for all commands and `codefactor <command> --help` for options accepted by your installed version.

## Configuration

Place `analyzer.yaml` or `analyzer.yml` in the scan root, or select a file with `--config`. Paths in project filters are relative to that root. Command-line options override project configuration, which overrides built-in defaults.

For a project whose source lives under `src/`:

```yaml
config_version: 1
languages:
  enabled: [php, rust, python, javascript, typescript]
paths:
  include: ["src/**"]
  exclude: ["**/tests/**", "**/generated/**"]
limits:
  function_params: {warn: 4, error: 7}
  cyclomatic_complexity: {warn: 10, error: 20}
  nesting_depth: {warn: 3, error: 5}
architecture:
  enabled: true
  profile: strict
```

Validate and inspect settings before using them in CI:

```sh
codefactor config check --file analyzer.yaml --strict
codefactor config show --file analyzer.yaml --resolved
codefactor config keys
codefactor scan . --config analyzer.yaml --strict-config
```

`config keys` is the configuration reference compiled into the installed binary. Additional sections cover per-language/path/entity thresholds, YAML includes, vendor handling, named sets, external-operation matching, caching and report defaults. Source discovery respects Git ignore rules; configured vendor roots receive separate handling for dependency analysis.

### Define architecture boundaries

The default `strict` profile checks dependency cycles, explicitly configured layer policies and structural copies. For example, to flag calls from domain code into infrastructure, extend the `architecture` section:

```yaml
architecture:
  profile: strict
  layers:
    - name: domain
      selectors: ["dir:src/domain"]
    - name: infrastructure
      selectors: ["dir:src/infrastructure"]
  forbidden_dependencies:
    - from: domain
      to: infrastructure
```

`--architecture-profile extended` adds experimental design checks, including SOLID-related findings and pattern suggestions with source evidence. Architecture findings are advisory and never change the CI exit status, including with `--fail-on-warn`. Use `--no-architecture` to disable architecture analysis.

### Set algorithmic complexity limits

Execution-cost analysis estimates how work grows with input size. It distinguishes tight classes, upper bounds, heuristics and unknown costs, and does not measure elapsed time. Estimates are informational by default. To enable thresholds:

```yaml
algorithmic_complexity:
  limits:
    warn: "N"
    error: "N*log(N)"
  unknown: info
```

A finding occurs when an estimate exceeds the configured class; equality is allowed. Tight estimates can produce errors, while upper bounds and heuristics produce warnings. Unknown operations remain explicit instead of being assumed constant-time.

## Reports and CI

| Output | Use |
| --- | --- |
| Console | Interactive summaries, metric explanations and findings |
| JSON | Full scan data, metrics, calls, diagnostics and integration with other tools |
| Markdown | Reports for code review or project documentation |
| SARIF | Findings for tools that consume static-analysis results |

These are scan output formats. `metrics` supports console and JSON; graph and AST commands have their own export options.

```sh
codefactor scan . --strict-config --fail-on-warn --format sarif --out codefactor.sarif
codefactor scan . --format json,md --out report
codefactor scan . --only-violations --format json --out findings.json
```

The multi-format example writes `report.json` and `report.md`. `--only-violations` produces a smaller JSON report containing metadata, a summary, findings and diagnostics, omitting the full file, metric and graph payloads. Operational logs go to stderr, keeping machine-readable stdout separate.

| Exit status | Meaning |
| --- | --- |
| `0` | No blocking findings; advisory findings may still be present |
| `1` | Warnings with `--fail-on-warn`, or a command/operational failure |
| `2` | Analysis rule errors |
| `3` | Root configuration YAML syntax error |

Inspect stderr and report diagnostics alongside the status: a nonzero status may still accompany a successfully written findings report, and a zero status does not establish complete analysis coverage.

For an existing codebase, create a baseline to suppress current findings in subsequent scans and focus on new ones:

```sh
codefactor baseline create --root .
codefactor scan . --fail-on-warn
```

The baseline is stored as `.analyzer-baseline.json` in the analyzed root. Review and version it with your project if the same baseline should apply in CI. Individual findings can also be suppressed with an `analyzer-ignore: rule_id` source comment on the declaration line or the preceding line.

## Companion packages

This repository contains the complete engine, CLI, parsers and web interface. Companion projects target common package managers and provide CLI launchers and optional APIs around the same engine. A package's host language does not restrict which languages the engine can analyze.

| Repository | Ecosystem | CLI entry point |
| --- | --- | --- |
| [codefactor-app/php](https://github.com/codefactor-app/php) | PHP / Composer | `vendor/bin/codefactor` |
| [codefactor-app/python](https://github.com/codefactor-app/python) | Python / pip | `codefactor`, `python -m codefactor` |
| [codefactor-app/typescript](https://github.com/codefactor-app/typescript) | JavaScript and TypeScript / npm | `npx --no-install codefactor` |
| [codefactor-app/rust](https://github.com/codefactor-app/rust) | Rust / Cargo | `codefactor`, `cargo codefactor` |

The launchers preserve CLI arguments, the working directory, standard streams and exit status. Their APIs expose scan, AST and graph operations. Track package publication and installation availability in the linked repositories; the standalone core binary is available from [core releases](https://github.com/codefactor-app/core/releases).

## How it works

Codefactor discovers source files, parses them with Tree-sitter and normalizes language-specific declarations into a shared intermediate representation. The engine builds relationships, computes metrics and costs, applies configured rules and exposes the results through reports and the local server. Project metadata contributes to resolution without executing package managers, build scripts or the analyzed code.

| Source | Responsibility |
| --- | --- |
| [analyzer-core](crates/analyzer-core/src) | Shared model, discovery, configuration, metrics, resolution, graphs, architecture, costs and reports |
| [analyzer-parser](crates/analyzer-parser/src) | Shared Tree-sitter parsing and normalization helpers |
| [Language crates](crates) | PHP, Rust, Python, JavaScript and TypeScript analysis strategies |
| [analyzer-cli](crates/analyzer-cli/src) | CLI commands and local HTTP server |
| [Web assets](crates/analyzer-cli/web) | Embedded browser interface and graph models |
| [Vendored grammars](vendor) | Pinned PHP and TypeScript grammars with their own provenance and licenses |

Project resolution includes Node package metadata, TypeScript/JavaScript path mappings and supported Cargo metadata and local dependencies. The Rust core also exposes `LanguageStrategy` and metric registries for extending analysis in applications embedding the engine.

## Analysis boundaries

Codefactor models the source and metadata it can inspect. Runtime dispatch, reflection, metaprogramming, unknown receiver types and unsupported constructs can leave calls unresolved or ambiguous. Macro expansion, generated code and complete compiler/dependency-resolution behavior are outside the supported static model.

Complexity estimates depend on supported constructs and stated assumptions; they are not benchmarks. Structural copies do not establish semantic equivalence, and experimental design findings need human review. Coverage and diagnostics describe the available model rather than proving all runtime behavior. Review parse and resolution diagnostics before drawing conclusions from a missing finding or graph edge.

## Development and contributing

Changes to language support, resolution, metrics, reports and the interface are welcome. Use [issues](https://github.com/codefactor-app/core/issues) for reproducible bugs and feature proposals, and [pull requests](https://github.com/codefactor-app/core/pulls) for changes. For analysis bugs, include a minimal source example, the command and configuration, the Codefactor version, and expected versus actual results.

From a source checkout, run the relevant checks:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
node --test crates/analyzer-cli/tests/*.cjs
```

Rust tests cover the engine, CLI and language behavior. Node.js runs the web model tests. The [CLI test directory](crates/analyzer-cli/tests) contains integration tests and pinned call-resolution corpora; vendored test sources retain upstream license and provenance information. Rebuild the executable after changing embedded web assets.

## License

Codefactor is distributed under the [MIT license](LICENSE). Vendored grammars and third-party test sources retain their own licenses and provenance records.
