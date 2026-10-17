# Codefactor

Codefactor is a local static analyzer for PHP, Rust, Python, JavaScript and TypeScript. It reads source files without executing them and reports code metrics, rule violations, architecture findings and call relationships. The executable includes a web interface for exploring source code and dependencies.

This repository contains the analysis engine, CLI, web interface and language parsers. Separate PHP, Python, TypeScript and Rust packages provide CLI launchers and optional APIs.

## Build and run

Install the Rust stable toolchain, then build the executable:

```sh
cargo build --release --locked -p analyzer-cli --bin codefactor
./target/release/codefactor --help
./target/release/codefactor scan ./src
./target/release/codefactor serve ./src --port 8080 --watch
```

Replace `./src` with the project directory to analyze. Open the loopback URL printed by `serve`, normally `http://127.0.0.1:8080/`. The server listens on loopback unless a host is explicitly supplied.

## Common commands

With `codefactor` on your `PATH`:

```sh
codefactor scan ./src
codefactor scan ./src --format json --out report.json
codefactor metrics ./src --group-by directory
codefactor ast ./src/main.py --format json
codefactor graph callers 'app.service.Worker.run' --format mermaid
codefactor config check
codefactor serve ./src --port 8080
```

`scan` and `metrics` default to Russian reports with metric explanations. JSON, Markdown and SARIF output are available for integrations. Detailed AST is generated on demand with `codefactor ast`.

Run `codefactor <command> --help` for command options and `codefactor config keys` for supported configuration keys.

## Configuration

A project may provide `analyzer.yaml`; command-line flags take precedence. For example:

```yaml
config_version: 1
languages:
  enabled: [python, typescript]
paths:
  include: ["src/**"]
  exclude: ["**/tests/**"]
limits:
  function_params: {warn: 4, error: 7}
```

Use `codefactor config check --file analyzer.yaml --strict` to validate a configuration before scanning.

## Analysis capabilities

The analyzer estimates symbolic worst-case execution cost, including resolved callees. Tight classes, upper bounds, heuristics and unknown costs are distinguished. Estimates are informational by default; `algorithmic_complexity.limits` enables warnings and errors.

Architecture analysis is enabled by default and does not change the CI exit status. The strict profile checks dependency cycles, explicit layer policies and structural code copies. The extended profile adds experimental design checks with source evidence and conditional recommendations.

Call resolution uses source declarations and project metadata. Dynamic behavior, unknown receiver types and unsupported constructs can leave targets unresolved or ambiguous. Analysis coverage describes the available static model. It does not establish all runtime behavior.

## Development

The source checkout can be built and tested independently:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
node --test crates/analyzer-cli/tests/*.cjs
```

Node.js is needed for the web model tests. The CLI embeds web assets and does not require Node.js at runtime.

## License

MIT. Vendored grammars and third-party test sources retain their own licenses and provenance records.
