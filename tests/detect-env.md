# Name-only environment detection

```sh
brainpod detect . --with-env
brainpod detect ./services --with-env --json
```

Without the flag, environment adapters are not run, no additional files are read, and the `environment` field is absent. With it, the field is present even when empty. Project/framework/runtime detection is unchanged.

## Why these five

This is a practical initial selection, **not a measured global top-five ranking**. Research on 2026-11-10 used public, read-only documentation and GitHub repository metadata:

- **Dotenv:** the cross-language `.env` convention; [motdotla/dotenv](https://github.com/motdotla/dotenv) had 20,563 stars. Its [parser rules](https://github.com/motdotla/dotenv#what-rules-does-the-parsing-engine-follow) explicitly describe assignments, comments, quotes and multiline values.
- **Compose:** [docker/compose](https://github.com/docker/compose) had 38,674 stars. The [Compose specification](https://github.com/compose-spec/compose-spec/blob/main/spec.md#environment) explicitly distinguishes `environment`, pass-through keys and `env_file` declarations.
- **Dockerfile:** Docker is described as near-universal in the [2025 Stack Overflow technology survey](https://survey.stackoverflow.co/2025/technology/). The [official frontend reference](https://github.com/moby/buildkit/blob/master/frontend/dockerfile/docs/reference.md#env) distinguishes persisted `ENV` declarations from build-only `ARG` (BuildKit had 10,382 stars).
- **Kubernetes:** [kubernetes/kubernetes](https://github.com/kubernetes/kubernetes) had 128,969 stars. Its [environment documentation](https://kubernetes.io/docs/tasks/inject-data-application/define-environment-variable-container/) explicitly defines `env` and `envFrom` without requiring cluster queries.
- **GitHub Actions:** an explicit, declarative CI format with [documented workflow/job/step `env` maps](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#env). [actions/runner](https://github.com/actions/runner) had 6,386 stars; the pinned local corpus independently supplied environment declarations in 42 of 51 repositories. This sample is web-project-biased, not market-share evidence.

Stars are adoption proxies, not comparable format-usage counts. No private repository content was sent to a service. Context7 was unavailable; primary documentation was read directly.

## Exact coverage

Filename/path routing is declared in `catalog.environment` in [`rules.toml`](../src/detect/rules.toml), compiled alongside the existing catalog. First matching entry wins. Syntax stays in the five adapters in [`environment.rs`](../src/detect/environment.rs); no executable configuration language or config-key guessing is involved.

| Format | Candidates | Supported declarations |
|---|---|---|
| `dotenv` | `.env`, `.env.*`, `*.env`, `*.env.example`, `*.env.sample`, `*.env.template`; excludes `*.enc` and `.env.vault` | `NAME=value`, optional `export`, whitespace, empty values, comments, single/double/backtick quotes, escaped quotes and quoted multiline values. BOM/CRLF accepted. Bare names, colon assignment and shell programs are not supported. |
| `compose` | `compose.yml/yaml`, `docker-compose.yml/yaml` and dotted variants such as `compose.override.yaml` | `services.*.environment` scalar maps or string lists (`NAME=value` or bare `NAME`); null/bare entries are `external`. `env_file` string/list/path-mapping entries become unresolved references, not file reads. |
| `dockerfile` | `Dockerfile`, `Dockerfile.*`, `*.Dockerfile` | Case-insensitive `ENV`, multiple assignments, legacy `ENV NAME value`, quotes, escaped spaces, backslash/backtick continuations, continued comments, and `escape` directives. Each `FROM` advances a numbered stage. `ARG`, `RUN` assignments and base-image environments are not inferred. |
| `kubernetes` | YAML/JSON in directories named `k8s`, `kubernetes`, `kube`, `manifests`, `deploy`, `deployment`, `deployments`; also `pod.yml/yaml`, `deployment.yml/yaml`, `kubernetes-*.yml/yaml`, `*.k8s.yml/yaml/json`, `*.kube.yml/yaml/json` | Core `v1` Pod/List/ReplicationController, `apps/v1` Deployment/StatefulSet/DaemonSet/ReplicaSet, `batch/v1` Job/CronJob. Known pod-spec paths only; regular/init/ephemeral containers; explicit `env[].name`, `valueFrom`, `envFrom` and prefixes. Lists and multi-document streams supported. |
| `github-actions` | `.github/workflows/*.yml/yaml` at any repository depth | Only workflow `env`, `jobs.*.env` and `jobs.*.steps[].env` maps. Whole-map expressions are unresolved references. `with`, matrix keys, scripts and writes to `$GITHUB_ENV` are not inferred. |

YAML supports flow/block collections, quoted/block scalars and bounded anchors/merge keys. Duplicate mapping keys, unsupported tags, malformed input or budget breaches reject the entire YAML stream with a redacted diagnostic. No includes or property interpolation are enabled. Kubernetes candidates are intentionally narrower than all YAML: application data, Rails fixtures, Spring properties, arbitrary JSON, CRDs, Helm rendering and Kustomize evaluation are not scanned/inferred as environments. Use an extra catalog entry for a nonconventional manifest location.

Names use the conservative common subset `[A-Za-z_][A-Za-z0-9_.-]*`. Unsupported/dynamic names are skipped with diagnostics. This is not a claim to accept every name allowed by every ecosystem. Kubernetes resource/key references additionally allow a leading digit. Simple downward-API field/resource paths are retained as reference keys; complex field selectors are left unresolved.

A `RUN`/`COPY`/`ADD` body containing `<<` conservatively stops inspection of that Dockerfile at the containing instruction with a diagnostic, preventing script bodies from becoming false `ENV` declarations. Instructions after the heredoc and experimental `ENV` flags are not covered. Custom Dockerfile frontends, Compose includes/extends/overlays, precedence, image stages/inheritance, workflow activation and environment installation are not evaluated.

## Output, provenance and privacy

Each `environment` record has:

- `name` when explicitly known; otherwise an unresolved reference record without a name.
- `source`: scan-root-relative metadata filename; `format`: one of the five above.
- `location`: one-based `line:N` for dotenv, numbered stage/one-based line/assignment index for Dockerfile, or zero-based `document:N` plus escaped JSON-pointer-style paths for YAML/JSON.
- `project`: nearest detected ancestor project/workspace path, or `null`. This is directory ownership, **not evidence that the application uses the variable**. Workflow files normally belong to the repository root, not projects mentioned in scripts.
- `state`: `declared`, `external` (Compose pass-through), or `unresolved`.
- Optional `reference`: kind and statically known target/key/prefix. Dynamic expressions and absolute/dynamic file targets are omitted, not interpolated or echoed.

Records are stably sorted by source; within a source they retain adapter order (JSON/YAML mapping keys are sorted). Repeated list entries, repeated lines and the same name in different files/services/stages/scopes remain separate. There is no last-wins merge, collision resolution or workspace-to-member copying. Existing `project.workspaces` supplies workspace membership separately. Identical names do not establish dataflow between declarations.

Literal environment values are never returned in JSON/text, diagnostics or retained declaration facts. File bytes and YAML values exist transiently in parser memory, then are discarded. No values are loaded into the process, expanded, sourced, executed, fetched or saved. Current process environment variables are not inspected. ConfigMap/Secret data keys are not enumerated, even when those resources are present locally; `envFrom` remains an unresolved declaration reference. Referenced env files are not followed; matching files independently discovered in the inventory retain their own source/project provenance.

The existing directory exclusions, symlink skipping, 128-level depth limit and 1 MiB per-file read limit apply. Explicit symlink scan roots are rejected with the flag. Reads stop at 1 MiB + 1 byte for oversized-file diagnostics; no total repository-size cap is imposed. YAML is limited to depth 48, 256 documents, 100,000 events, 50,000 nodes, 128 anchors/aliases and 2 MiB of expanded scalar content per stream. Diagnostics never quote environment input and are deduplicated per source/message. Static metadata scanning assumes the directory tree is not concurrently replaced during traversal.

## Reproduction

No dependencies or downloaded project code need to be installed/executed. These commands use the existing dependency cache and pinned snapshot cache:

```sh
cargo test --offline --locked -j 4
cargo clippy --offline --locked -j 4 --all-targets -- -D warnings
cargo fmt --all -- --check
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_audit_detect.py'
cargo build --release --offline --locked -j 4
python3 tests/audit_detect.py --list

cache=/tmp/brainpod-detect-deno.GqlZEX/combined-cache
python3 tests/audit_detect.py --binary target/release/brainpod --cache "$cache" \
  --runs 5 --output /tmp/detect-default.json
python3 tests/audit_detect.py --binary target/release/brainpod --cache "$cache" \
  --with-env --runs 5 --output /tmp/detect-with-env.json
```

`environmentChecks` in the pinned corpus are only enabled by `--with-env`; they sample reviewed names/sources/formats/project ownership and unresolved references in mixed repositories. The audit runner resolves verified cache directory aliases before passing an explicit scan root. Do not add `--fetch` for offline reproduction. Synthetic Rust tests cover malformed input, secret redaction, no evaluation, quoting/multiline declarations, duplicate/scoped names, symlinks, read/depth/parser limits and serial/parallel determinism.

## Local results (2026-11-10)

Local validation passed 215 Rust tests, 12 Python audit-runner tests, Clippy with warnings denied, formatting and the release build. The ignored release synthetic scanner benchmark measured 4.83 ms median / 200 manifest reads. The two unchanged XML-only microbenchmarks and native OS CI were not rerun.

- **51/51 pinned repositories**, five deterministic runs per mode: **295/295** existing assertions without the flag, **321/321** with it (including **26/26** environment assertions across eight reviewed repositories).
- All 51 full default JSON reports were separately byte-compared with the base binary at `124014e7`. All opt-in `projects` arrays equal the default arrays. The ten pinned Brainpod starter branches were rerun five times in each mode; default reports matched the cached baseline and opt-in project arrays were unchanged. Starters supplied 660 declaration/reference records.
- The corpus supplied 13,069 declaration/reference records: 7,180 Actions, 2,940 dotenv, 2,346 Compose, 581 Dockerfile and 22 Kubernetes. Repositories with records: 42, 26, 23, 26 and 2 respectively. These counts include examples, duplicate names, unresolved references and separate scopes; they are **not** distinct-variable counts or exhaustive precision/recall measurements.
- Default scans read 5,185 files / 7,252,464 bytes; opt-in scans read 6,593 / 13,974,123. Five-run warm median repository CLI times were 6.14 ms default and 10.50 ms opt-in on this host. An interleaved 12-sample no-flag 200-service / 20,200-file CLI A/B measured 8.76 ms base vs 8.82 ms current, both with exactly 200 manifest reads. CLI timing includes startup and serialization, unlike an in-process scanner benchmark.
- Opt-in reports had 31 additional diagnostics: 22 invalid/budget-exceeding YAML streams (including unrendered Helm templates), four conservative Docker heredoc stops, three unsupported dotenv lines, one malformed quoted dotenv declaration and one unsupported name. The five preexisting project diagnostics remain unchanged. Unsupported input is surfaced, not treated as complete coverage.

Raw local results are in `/tmp/brainpod-env-audit/{default,with-env,starters,regression}.json`; temporary A/B/starter reproduction scripts are `/tmp/brainpod-env-{audits.sh,regression.py}`. These cache-dependent artifacts are not committed. Portable corpus reproduction is above.
