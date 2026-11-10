<picture>
  <source media="(prefers-color-scheme: dark)" srcset=".github/hero-dark.gif">
  <img src=".github/hero.gif" alt="Brainpod — build it in Claude, Cursor or Codex, host it in Europe" width="900">
</picture>

# Brainpod CLI

A non-interactive CLI for managing Brainpod pods, images, blueprints, revisions, resources, deployments, object storage, database tunnels, and events. Its default output is deterministic line-oriented text suitable for LLMs and shell tools. Add `--json` to receive machine-readable JSON; login, database tunnels, and event watches use NDJSON.

The CLI builds application images locally from an existing Dockerfile or with Railpack, then pushes them directly to the selected pod's private Brainpod registry namespace. Image builds probe the API's cluster architectures, prefer amd64 and then arm64, and store the selected default architecture in the configuration. Use `--platform linux/arm64` for a one-off override.

## Install

Published releases carry prebuilt binaries for Linux and macOS on amd64 and arm64, and for Windows on amd64. Pick the asset for your platform:

| OS | Architecture | Asset |
|---|---|---|
| Linux | `x86_64` | `brainpod-amd64-linux.tar.gz` |
| Linux | `aarch64` or `arm64` | `brainpod-arm64-linux.tar.gz` |
| macOS | `x86_64` | `brainpod-amd64-macos.tar.gz` |
| macOS | `arm64` | `brainpod-arm64-macos.tar.gz` |
| Windows | `x86_64` | `brainpod-amd64-windows.zip` |

Download it alongside the checksum file and verify it before extracting. The `latest` alias redirects to the newest published release:

```sh
curl -fsSL -O https://github.com/brainpodnl/cli/releases/latest/download/brainpod-arm64-macos.tar.gz
curl -fsSL -O https://github.com/brainpodnl/cli/releases/latest/download/SHA256SUMS
shasum -a 256 --ignore-missing -c SHA256SUMS
tar -xzf brainpod-arm64-macos.tar.gz
```

Use `sha256sum --ignore-missing -c SHA256SUMS` on Linux. The Unix archives contain a single `brainpod` binary; the Windows zip contains `brainpod.exe`. Extract it into a directory already on your `PATH`. Make the Unix binary executable first. The macOS binaries are not notarized yet, so clear the quarantine attribute if Gatekeeper blocks one:

```sh
xattr -d com.apple.quarantine brainpod
```

Confirm the install with a command that needs no API token:

```sh
brainpod describe
```

Building from source needs Nix; this is the same build the release workflow runs:

```sh
nix build
```

Cross-compile the Windows binary from an x86_64 Linux Nix host with:

```sh
nix build .#packages.x86_64-linux.windows
```

The result is `result/bin/brainpod.exe`. API commands and database tunnels work natively on Windows. Railpack image builds are unavailable because Railpack does not publish a Windows binary; use a Dockerfile or run image builds under WSL2.

`nix develop` gives you the toolchain, `rustfmt`, and `rust-analyzer` for working on the CLI itself. `direnv` picks up the same shell through `.envrc`.

## Quick start

Authenticate and create a pod. The create response reports the pod's name; store it as the default so later commands do not need `--pod`:

```sh
brainpod login
brainpod pod create --display-name "My API"
brainpod config set pod <pod>
```

Build the application image from the current directory and push it to the pod's registry namespace. This needs Docker with Buildx; the result includes a digest-pinned reference to use as an App resource's `spec.image`:

```sh
brainpod image build api . --tag v1
```

Compose the resources the application needs, then promote the draft revision and wait until every resource reports healthy:

```sh
brainpod describe resource app --json
brainpod resource create --file resources.json
brainpod deploy --summary "First deploy" --wait
```

Resource create only changes the pod's mutable head; nothing runs until `deploy` promotes it. Follow the application's logs with the URN the resource commands return:

```sh
brainpod events --watch --resource urn:brain:app:default:api
```

If you would rather have a coding agent drive this, the [Brainpod skills](https://github.com/brainpodnl/skills) repository teaches Claude Code, Cursor, and Codex to sign up, compose the resource graph, and deploy a project end to end.

## Configuration

Authenticate in the Brainpod dashboard and store the resulting API token in the CLI configuration:

```sh
brainpod login
```

The default configuration file is `~/.config/brainpod/config.toml`. `XDG_CONFIG_HOME` is respected, and `BRAINPOD_CONFIG` can override the complete path.

```sh
brainpod config set api-token brain_example
brainpod config set pod my-pod
brainpod config set endpoint https://api.brainpod.io
brainpod config set control-plane-endpoint https://control-plane.brainpod.io
brainpod config set registry-endpoint https://registry.brainpod.io
brainpod config set architecture arm64
brainpod config show
brainpod config path
```

Configuration uses TOML:

```toml
endpoint = "https://api.brainpod.io"
control_plane_endpoint = "https://control-plane.brainpod.io"
registry_endpoint = "https://registry.brainpod.io"
api_token = "brain_example"
pod = "my-pod"
architecture = "amd64"
```

Values are resolved in this order:

1. Global flags: `--endpoint`, `--control-plane-endpoint`, `--registry-endpoint`, `--api-token`, `--pod`
2. `BRAINPOD_API_ENDPOINT`, `BRAINPOD_CONTROL_PLANE_ENDPOINT`, `BRAINPOD_REGISTRY_ENDPOINT`, `BRAINPOD_API_TOKEN`, `BRAINPOD_POD`
3. The configuration file
4. The defaults `https://api.brainpod.io`, `https://control-plane.brainpod.io`, and `https://registry.brainpod.io`

For image builds, `--platform` overrides the configured architecture. Without it, the CLI probes the available clusters, prefers `amd64` and then `arm64`, and stores the selected architecture in the configuration.

`brainpod login` uses `https://console.brainpod.io` as its dashboard and supports overriding it with `BRAINPOD_DASHBOARD_ENDPOINT` for local or test environments.

The config file is written with mode `0600` on Unix. `config show` never reveals the API token.

## Output contract

Text is the default. Each endpoint has a purpose-built renderer. Collection endpoints use plain tables without terminal control sequences, while detail and mutation endpoints use concise labeled sections:

```text
NAME    HEAD STATUS  HEAD                                           DEPLOYED
------  -----------  ---------------------------------------------  ---------------------------------------------
my-pod  ready        v12 (1a2b3c4d-1111-2222-3333-444455556666)    v11 (7e8f9a0b-1111-2222-3333-444455556666)
```

JSON mode bypasses text rendering and emits the complete API response as one JSON value for non-streaming commands:

```sh
brainpod pod list --json
brainpod --json resource list
```

Login writes both the authorization notice and the successful authentication result to stdout. With `--json`, it emits newline-delimited JSON so callers receive the authorization URL before the callback completes. A successful login emits an `authorize` event followed by an `authenticated` event containing the complete user response:

```json
{"event":"authorize","url":"https://console.brainpod.io/cli/authorize?...","expiresInSeconds":600}
{"event":"authenticated","user":{"email":"user@example.com"}}
```

Event watches are also streamed as newline-delimited JSON so each event is available immediately. Each line contains the SSE event name, event ID, and decoded data.

Text event output uses color for timestamps, levels, platform events, and HTTP statuses when stdout is a terminal. JSON and redirected output never contain ANSI color sequences.

Errors go to stderr and return a non-zero exit code. With `--json`, errors also use JSON and API errors retain the API's stable error code, request ID, and details. Account-limit validation errors include instructions and an `upgradeUrl` pointing to `https://console.brainpod.io/onboarding?upgrade=1`.

## Commands

```text
brainpod detect [<path>] [--rules <rules.toml>]
brainpod describe [<command>...]
brainpod describe resource [<kind>]
brainpod login
brainpod whoami
brainpod cluster list
brainpod pod list
brainpod pod create [--display-name <name>]
brainpod pod get <pod>

brainpod --pod <pod> agent start [--path <dir>] [--no-ignore]
brainpod agent serve [--path <dir>] [--port <number>]
brainpod agent step <id> [--label <text>] \
  [--state <pending|running|done|failed>] [--detail <text>] [--path <dir>]
brainpod agent log [--stream <name>] [--path <dir>]
brainpod agent finish [--state <done|failed>] [--message <text>] [--path <dir>]
brainpod agent clear [--path <dir>] [--all]

brainpod blueprint list
brainpod blueprint get <blueprint>
brainpod --pod <pod> blueprint install <blueprint> [--file <path|->]

brainpod --pod <pod> image list [--search <text>] [--visibility <all|public|pod>] \
  [--limit <1-100>] [--offset <number>]
brainpod --pod <pod> image inspect <repository> <tag> [--visibility <public|pod>]
brainpod --pod <pod> image build <image> [<context>] [--tag <tag>] \
  [--builder <auto|dockerfile|railpack>] [--output <oci-directory>]

brainpod --pod <pod> revision list [--cursor <uuid>] [--limit <1-50>]
brainpod --pod <pod> revision get <revision>
brainpod --pod <pod> revision diff <revision> [--base <revision>]
brainpod --pod <pod> revision wait <revision> [--timeout <seconds>]

brainpod --pod <pod> resource list [--revision <uuid> | --at <timestamp>]
brainpod --pod <pod> resource get <kind> <name> [--revision <uuid> | --at <timestamp>]
brainpod --pod <pod> resource create --file <path|-> [--dry-run]
brainpod --pod <pod> resource replace <kind> <name> --file <path|->
brainpod --pod <pod> resource delete <kind> <name>
brainpod --pod <pod> resource variables [<kind> <name>] [--revision <uuid> | --at <timestamp>]

brainpod --pod <pod> tunnel <database-resource> [<listen-address>] [--skip-preflight]

brainpod --pod <pod> deploy [--summary <text>] [--wait] [--timeout <seconds>]
brainpod --pod <pod> redeploy

brainpod --pod <pod> events --resource <resource> [--kind <app|http-access|platform>] \
  [--level <trace|debug|info|warn|error>] [--search <text>] \
  [--range <5m|15m|30m|1h|24h|7d>] [--cursor <cursor>]
brainpod --pod <pod> events --watch --resource <resource> \
  [--kind <app|http-access|platform>] [--level <trace|debug|info|warn|error>] \
  [--search <text>] [--range <5m|15m|30m|1h|24h|7d>] [--cursor <cursor>] \
  [--duration <1-20>] [--last-event-id <id>]
```

`brainpod tunnel` creates a two-hour database tunnel session and forwards local TCP connections until Ctrl-C is pressed. Select the pod with `--pod`, `BRAINPOD_POD`, or the configured default, then identify a deployed PostgreSQL, MariaDB, Valkey, or Microsoft SQL Server resource by name, URN, or stable UUID. For example, `brainpod --pod my-pod tunnel db` resolves `db` through the API before opening the tunnel. The listener defaults to `127.0.0.1` and the engine's standard port; pass an explicit address such as `127.0.0.1:15432` to override it. By default, the command prints an engine-specific banner with the local-to-remote port mapping, credentials, client command, and DSN before it starts accepting connections, so GUI clients such as DBeaver can be configured first. Pass `--skip-preflight` to skip credential retrieval and omit the password and DSN. The API token must grant `resources:read` and `database:connect` for the database or its pod.

```text
╭─ ◆ Brainpod tunnel
│
│  PostgreSQL
│  Local      127.0.0.1:15432
│  Remote     PostgreSQL:5432
│
│  Username   brainpod
│  Database   brainpod
│  Password   ...
│
├─ Client
│  psql "host=127.0.0.1 port=15432 user=brainpod dbname=brainpod sslmode=require"
│
├─ DSN
│  postgres://brainpod:...@127.0.0.1:15432/brainpod?sslmode=require
│
╰─ ● Ready · press Ctrl+C to stop
```

New, closed, and failed local connections are reported as concise status lines while the tunnel is running.

Events accept a resource name, URN, or stable UUID. For example, `brainpod --pod my-pod events --resource api` resolves `api` to its canonical URN before querying events. Passing a URN directly skips resolution, so an API token with only `events:read` remains sufficient; resolving a name or UUID also requires `resources:read`. Omit `--kind` to return every stream available for the resource. `--level` requires `--kind app`.

Event watches flush text or JSON output as messages arrive and reconnect after each server-imposed stream duration, continuing until interrupted. The per-request duration defaults to 10 seconds. Reconnects use the latest SSE event ID to avoid replaying emitted events. Use `--last-event-id` to set the initial event ID; `--cursor` resumes the initial request from an API event cursor.

`brainpod cluster list` lists active clusters and their supported architectures.

`brainpod agent` maintains a session console: a page an agent puts in front of the user so a deploy is something they watch rather than sit through. `agent start` writes `console.html` and `session.json` into a directory of their own under `.brainpod/` at the repository root, adds `.brainpod/` to `.gitignore` unless `--no-ignore` is passed, and prints the page to open. The page reads `session.json` and `session.log` from its own directory and needs no server. Where `agent serve` is running it advertises an event stream in the session, and the page upgrades from polling to push; if that stream drops it falls back to polling, so the two paths run the same code. Every write replaces the file through a temporary rename, so the page never reads a partial write.

Which of the two ways to open the page works depends on the browser. An agent's embedded browser will only execute a local page from inside the project, so it opens `console.html` directly. A browser outside the agent will display that page but never populate it, because reading a file from the same directory is blocked on `file://`. For those, `agent serve` publishes the console over loopback instead, which puts the page and its session on one origin. It announces the URL on stdout and then blocks, so run it in the background and read the first line; the URL carries a random path because loopback is reachable by anything else on the machine.

`agent start` always mints a new session and discards the previous one; it never merges, so running a workflow twice cannot leave the earlier deploy's steps showing under the new one. `agent step` requires `--label` the first time an id is recorded and updates it thereafter. `agent log` reads stdin and appends to the session's `session.log`, tagging each line with `--stream` so one log can carry the build, the tests, and anything else in the order it happened. `image build` writes its own output there under `[build]`. Commands resolve the repository root rather than the working directory, so a command run from a subdirectory reaches the same console.

Each chat gets its own console, so two agents working in one checkout do not overwrite each other's page. Only `agent start` creates a session; every other command finds an existing one, in this order: the `--session` value or `BRAINPOD_AGENT_SESSION`; the chat the harness names through `CLAUDE_CODE_SESSION_ID` or `CODEX_THREAD_ID`; the process that ran `agent start`, which is how a sub-agent given an identifier of its own still reports into its supervisor's console; and failing all of those, the only session running. Where two sessions are equally plausible the command fails and asks for `--session` rather than writing into a page somebody else is watching. `agent clear` removes only the current chat's console unless `--all` is passed, and finished sessions are dropped three days after they end.

Pod-scoped commands use `--pod`, `BRAINPOD_POD`, or the configured default pod. Resource kinds are `app`, `config`, `route`, `postgres`, `mariadb`, `valkey`, and `disk`. Namespace is currently fixed to the API-supported `default` namespace.

`revision wait` polls revision details until every resource reports `healthy: true`. `deploy --wait` deploys first, then waits on the returned revision in the same way. Both use a 90-second timeout by default; pass `--timeout <seconds>` to override it. Interactive waits report each unhealthy-to-healthy transition on stderr. Progress is suppressed when stderr is redirected or `--json` is used. Failed or canceled revisions stop the wait immediately, and timeouts report the resources that are still unhealthy.

## Project detection

Inspect a directory without credentials, network access, subprocesses, or executing project code:

```sh
brainpod detect .
brainpod detect ./services --json
brainpod detect . --rules ./detection.toml --json
```

The result contains sorted project paths, manifest names, languages and version declarations, priority-ranked detections with evidence and `uses`/`provides` framework roles, workspace members, diagnostics, and scan statistics. Framework versions are attached to dependency evidence. JavaScript versions describe Node.js; TypeScript versions describe the compiler. C# versions distinguish `language` (LangVersion), `target` (TFM), `runtime`, and `sdk`; Java distinguishes compiler `source`/`target`/`release`, toolchains, build-JDK metadata, and pins. Requirements and pins retain their source and kind: these are declarations, not a claim about installed versions. Missing versions remain unknown.

Built-in coverage includes Laravel, Symfony, CakePHP, CodeIgniter, Composer; Go modules/workspaces and Gin/Echo/Fiber; Cargo workspaces, axum, Rocket, Actix; Bundler, Rails, Sinatra, Hanami; Python requirements, PEP 621, Poetry, Pipenv, uv workspaces, Django, FastAPI, Flask, Starlette, Celery; npm/pnpm workspaces and catalogs, Next.js, Nuxt, Astro, SvelteKit, Remix, Angular, Express, React, Vue, Vite; C#/.NET projects and solutions, ASP.NET Core/classic, Aspire, FastEndpoints; Java/JVM Maven and Gradle projects, Spring/Spring Boot, Quarkus, Micronaut; and standalone source files in all nine languages. Rust workspace dependency aliases and inherited `rust-version` are understood. Ancestor runtime pin/toolchain files are included with provenance. Poetry names, literal gemspec metadata, and Rails component dependencies are recognized. pnpm catalog references retain the original declaration and add the locally resolved constraint with its YAML source; unresolved references produce diagnostics.

The engine has three layers: bounded discovery, format readers that emit facts, and compiled detection rules. [`src/detect/rules.toml`](src/detect/rules.toml) owns manifest filenames/readers, JSON/TOML field projections, version/property mappings, workspace candidates, source-language families, inheritance boundaries, and directory exclusions. Format-specific syntax and local-reference resolution stay in adapters; framework decisions do not.

Additional framework rules use unique IDs:

```toml
[[rules]]
id = "company-next"
language = "javascript"
kind = "framework"
priority = 200
all = ["dep:npm:next", "file:company.config.*"]
none = ["file:legacy.config"]
```

A new JSON/TOML manifest can also be registered without another Rust parser or filename branch. For example, `--rules custom.toml` can add a project format and runtime declaration:

```toml
[[catalog.manifests]]
id = "service-meta"
files = ["service.meta.json"]
reader = "json"
package = "service-meta"
names = ["/name"]

[[catalog.manifests.versions]]
language = "javascript"
kind = "requirement"
paths = ["/runtime/node"]

[[rules]]
id = "service-meta"
language = "javascript"
kind = "project"
priority = 10
any = ["package:service-meta"]
```

The same catalog supports identity/dependency extraction and workspace members. JSON pointers select fields; schemas reject unknown options/readers and conflicting definitions. MSBuild/Maven/Gradle property-to-version mappings and MSBuild reference item kinds are declarative too.

`all` requires every pattern, `any` requires at least one when supplied, and `none` rejects any match. Higher priorities sort first; ties sort by ID. Matches coexist rather than hiding other frameworks or languages. Patterns also accept conjunction groups, for example `any = ["dep:gem:rails", { all = ["dep:gem:actionpack", "dep:gem:railties"] }]`. Patterns support `file:<basename glob>` (`*`, `?`), `source:<extension>`, `dep:<ecosystem>:<exact package>`, `identity:<ecosystem>:<exact package>`, `workspace:<ecosystem>`, and `package:<ecosystem>`. Workspace paths additionally support `**`. Ecosystems include `composer`, `go`, `cargo`, `gem`, `pypi`, `npm`, `nuget`, `dotnet-sdk`, `dotnet-framework`, `dotnet-reference`, `maven`, `maven-parent`, `maven-plugin`, and `gradle-plugin`.

Rule atoms and basename patterns are compiled once into typed fact indexes and fast exact/prefix/suffix matchers. The scanner walks once, caches the catalog, and never reads application source bodies or lockfiles. Wide trees use at most four directory workers; small trees stay serial. Directory facts are indexed and irrelevant empty directories are discarded, while output stays deterministic. pnpm YAML is deserialized directly into typed fields without building a syntax tree; unused fields/comments are not retained. XML uses the existing zero-allocation pull tokenizer, with structure/namespace checks and DTD/custom-entity rejection. Maven local parents and managed versions, MSBuild directory props/targets, central package versions, and static Gradle plugin/dependency/module declarations are resolved without running build tools. Arbitrary MSBuild functions, remote Maven parents/BOMs, profile activation, and dynamic Gradle build logic are not executed or guessed; unresolved version references remain explicit. Each manifest/version file is capped at 1 MiB. Symlinks are not followed; traversal stops at depth 128 with a diagnostic. Dependency/build directories such as `.git`, `.jj`, `node_modules`, `vendor`, `target`, `.venv`, `.next`, `dist`, `build`, `.gradle`, and `.vs` are pruned. `bin`/`obj` are additionally pruned beneath C# project roots without skipping Rust `src/bin` sources. It does not evaluate `.gitignore`. Source-only directories roll up to a same-language project ancestor when available; sibling services keep their own manifest boundaries.

Detection uses declared dependencies, not arbitrary substring matches or inferred framework usage from source. Composer/Cargo development dependencies are excluded; npm development dependencies are included because tools such as Astro commonly live there. Dynamic Ruby declarations and unsupported requirements options produce diagnostics instead of executing code. Invalid manifests are diagnosed without stopping discovery of other projects.

Run the generated large-tree performance fixture with:

```sh
cargo test --release detect::tests::benchmark_polyglot_scan -- --ignored --nocapture
```

### Reproduce the repository checks

The corpus is public and pinned to commit SHAs in [`tests/detect-corpus.json`](tests/detect-corpus.json). The [audit report](tests/detect-corpus.md) contains assertions, per-repository timings, known limitations, and the additional Brainpod starter branches.

| Language | Repositories |
|---|---|
| JavaScript | [react/react](https://github.com/react/react), [vercel/next.js](https://github.com/vercel/next.js), [mrdoob/three.js](https://github.com/mrdoob/three.js), [axios/axios](https://github.com/axios/axios), [react/create-react-app](https://github.com/react/create-react-app) |
| TypeScript | [freeCodeCamp/freeCodeCamp](https://github.com/freeCodeCamp/freeCodeCamp), [vuejs/vue](https://github.com/vuejs/vue), [n8n-io/n8n](https://github.com/n8n-io/n8n), [firecrawl/firecrawl](https://github.com/firecrawl/firecrawl), [excalidraw/excalidraw](https://github.com/excalidraw/excalidraw) |
| Python | [Significant-Gravitas/AutoGPT](https://github.com/Significant-Gravitas/AutoGPT), [AUTOMATIC1111/stable-diffusion-webui](https://github.com/AUTOMATIC1111/stable-diffusion-webui), [langgenius/dify](https://github.com/langgenius/dify), [langflow-ai/langflow](https://github.com/langflow-ai/langflow), [Comfy-Org/ComfyUI](https://github.com/Comfy-Org/ComfyUI) |
| PHP | [coollabsio/coolify](https://github.com/coollabsio/coolify), [nextcloud/server](https://github.com/nextcloud/server), [laravel/framework](https://github.com/laravel/framework), [filamentphp/filament](https://github.com/filamentphp/filament), [symfony/symfony](https://github.com/symfony/symfony) |
| Ruby | [rails/rails](https://github.com/rails/rails), [jekyll/jekyll](https://github.com/jekyll/jekyll), [mastodon/mastodon](https://github.com/mastodon/mastodon), [huginn/huginn](https://github.com/huginn/huginn), [discourse/discourse](https://github.com/discourse/discourse) |
| Go | [infiniflow/ragflow](https://github.com/infiniflow/ragflow), [gohugoio/hugo](https://github.com/gohugoio/hugo), [gin-gonic/gin](https://github.com/gin-gonic/gin), [netdata/netdata](https://github.com/netdata/netdata), [caddyserver/caddy](https://github.com/caddyserver/caddy) |
| Rust | [dani-garcia/vaultwarden](https://github.com/dani-garcia/vaultwarden), [meilisearch/meilisearch](https://github.com/meilisearch/meilisearch), [juspay/hyperswitch](https://github.com/juspay/hyperswitch), [parcel-bundler/parcel](https://github.com/parcel-bundler/parcel), [DioxusLabs/dioxus](https://github.com/DioxusLabs/dioxus) |
| C# | [jellyfin/jellyfin](https://github.com/jellyfin/jellyfin), [dotnet/aspnetcore](https://github.com/dotnet/aspnetcore), [jasontaylordev/CleanArchitecture](https://github.com/jasontaylordev/CleanArchitecture), [bitwarden/server](https://github.com/bitwarden/server), [ardalis/CleanArchitecture](https://github.com/ardalis/CleanArchitecture) |
| Java | [macrozheng/mall](https://github.com/macrozheng/mall), [spring-projects/spring-boot](https://github.com/spring-projects/spring-boot), [elastic/elasticsearch](https://github.com/elastic/elasticsearch), [spring-projects/spring-framework](https://github.com/spring-projects/spring-framework), [jeecgboot/JeecgBoot](https://github.com/jeecgboot/JeecgBoot) |

```sh
python3 tests/audit_detect.py --list
cargo build --release --locked
cache=$(mktemp -d)
python3 tests/audit_detect.py --fetch --cache "$cache" --output "$cache/results.json"
```

Repeat offline by reusing the cache and omitting `--fetch`.

XML timing is independently reproducible (warm bounded reads/UTF-8 versus preloaded pull parsing; excludes discovery, fact extraction, and CLI output):

```sh
XML_BENCH_CACHE="$cache" cargo test --release detect::xml::tests::benchmark_xml_corpus \
  -- --ignored --nocapture --test-threads=1
cargo test --release detect::xml::tests::benchmark_xml_local -- --ignored --nocapture
```

On the pinned corpus, 1,192 XML manifests (2.7 MB) took about **2.7 ms to read** and **7.4 ms to parse** in aggregate on the measured warm-cache machine. This is not a cold-disk or whole-engine latency claim. No XML parser replacement was justified by those measurements.

## Image discovery

Image commands require a pod and API token with `registry:pull` permission. List returns active public images and images in the selected pod:

```sh
brainpod --pod my-pod image list
brainpod --pod my-pod image list --visibility pod --search worker --limit 10 --offset 20 --json
```

`--visibility` accepts `all`, `public`, or `pod` for listing and is optional. The default limit is 25 and the maximum is 100. The response includes the total count and a next link when more results are available.

Inspect an exact image and all of its active architecture variants. Inspection defaults to the selected pod's private image (`pod`). Pass `--visibility public` to inspect a public image:

```sh
brainpod --pod my-pod image inspect api v1 --visibility pod
brainpod --pod my-pod image inspect ubuntu latest --visibility public --json
```

Inspection results include architecture-specific digest references, UID/GID values, exposed ports, and timestamps. Use a returned digest-pinned variant reference as an App resource's `spec.image`.

## Image building

Image builds require Docker with Buildx support. By default, the CLI uses `Dockerfile` from the build context when present and otherwise uses Railpack. Override detection with `--builder dockerfile` or `--builder railpack`. Dockerfile builds use Buildx directly and preserve the Dockerfile's configured runtime user.

For Railpack builds, the CLI downloads its pinned Railpack release on first use, verifies its SHA-256 checksum, and caches it in the operating system's user cache directory. It generates a Railpack plan and adds a final layer that runs as `railpack` with UID/GID 1000.

The image is pushed directly to `registry.brainpod.io/<pod>/<image>:<tag>` using the configured Brainpod API token, which must allow `registry:push` for the selected pod. Docker login is not required and the token is not written to Docker configuration. The result includes an immutable digest reference suitable for an App resource's `spec.image`:

```sh
brainpod --pod my-pod image build api . --tag v1
brainpod --pod my-pod image build worker ./services/worker --builder railpack --output ./worker.oci --json
```

The context defaults to the current directory and the tag defaults to `latest`. The CLI probes Brainpod's active clusters and prefers `linux/amd64`, then `linux/arm64`; use `--platform` to override the selected platform. On ARM hosts, Docker must provide emulation when targeting amd64. `--output` retains the final OCI image layout in addition to pushing it; without that option, the layout is temporary. Existing output paths are rejected rather than overwritten.

Use `--registry-endpoint` or `BRAINPOD_REGISTRY_ENDPOINT` for test and local registries. Plain HTTP is only used when the configured endpoint explicitly starts with `http://`.

## Command discovery

`describe` exposes the installed CLI's version-matched command contract without requiring an API token. Omit the command path to return the complete command tree, or select a command for focused metadata:

```sh
brainpod describe
brainpod describe resource create
brainpod describe resource create --json
```

JSON descriptions include command paths, usage, arguments, allowed values, defaults, conflicts, authentication and pod requirements, side effects, examples, and related operational guidance. The command syntax is generated from the same Clap definitions used for argument parsing. Resource schemas are available without authentication:

```sh
brainpod describe resource
brainpod describe resource app --json
brainpod describe resource postgres --json
```

Resource schemas are fetched from `https://api.prod.brainpod.io/v1/openapi.json` on each request. If the production document cannot be reached or does not contain the expected resource schemas, the version embedded in the CLI is used instead. Set `BRAINPOD_OPENAPI_URL` to override the document URL, or pass `--endpoint` to derive the document URL from another API endpoint.

Each schema carries the variables that kind exports, so the references an App resource can use in `spec.env` are discoverable before the resource exists. Names in this catalog are placeholders: `${<name>.uri}` becomes `${db.uri}` for a resource named `db`. Template-sourced variables also report the template, which contains references only and never values.

## Resource variables

Once resources exist, `resource variables` reports their references resolved against a revision, for the whole pod or for one resource:

```sh
brainpod --pod my-pod resource variables
brainpod --pod my-pod resource variables postgres db --json
```

The text output is a `NAME`, `REF`, `VALUE`, `SECRET` table. `REF` is ready to paste into an App resource's `spec.env`. Secret values are never returned by the API and print as `<secret>`; a variable that cannot be resolved from the current content, such as a route hostname that has not been assigned, prints as `<unresolved>`. `--json` emits the complete variable objects, including each one's description, template, and owning resource URN. Use `--revision` or `--at` to read a historical revision.

## Blueprint input

Blueprint install accepts an optional JSON object containing values from the blueprint's input schema. Omit `--file` to install with the blueprint defaults. Use `--file -` to read JSON from stdin. Installing changes the pod's mutable head but does not deploy it.

```sh
brainpod blueprint get laravel
brainpod --pod my-pod blueprint install laravel --file blueprint-input.json
brainpod --pod my-pod deploy --summary "Install Laravel blueprint"
```

## Resource input

Resource create accepts either one JSON resource or an array. A single resource is normalized to the API's list request. Use `--file -` to read JSON from stdin.

```json
{
  "apiVersion": "pod.brainpod.io/v1alpha1",
  "kind": "Disk",
  "metadata": {
    "name": "data",
    "namespace": "default"
  },
  "spec": {
    "size": 10
  }
}
```

Validate without mutating:

```sh
brainpod resource create --file disk.json --dry-run --json
```

Create and deploy:

```sh
brainpod resource create --file resources.json --json
brainpod deploy --summary "Configure application resources" --json
```

## S3 gateway

Use the normal Brainpod API token and selected Pod (`--pod`, `BRAINPOD_POD`, or configured default). The key needs explicit `s3:list`, `s3:read`, and/or `s3:write` policies for that Pod; write includes replacement and deletion. Existing keys are not expanded automatically. Buckets must already be provisioned through the resource workflow.

```sh
brainpod --pod my-pod s3 ls
brainpod --pod my-pod s3 ls s3://uploads/images/
brainpod --pod my-pod s3 cp ./photo.jpg s3://uploads/images/photo.jpg
brainpod --pod my-pod s3 upload ./photo.jpg s3://uploads/photo.jpg
brainpod --pod my-pod s3 cp s3://uploads/photo.jpg ./downloaded.jpg
brainpod --pod my-pod s3 cp s3://uploads/photo.jpg s3://archive/photo.jpg
brainpod --pod my-pod s3 mv s3://uploads/photo.jpg s3://uploads/renamed.jpg
brainpod --pod my-pod s3 rm s3://uploads/renamed.jpg
```

Remote paths are `s3://<logical-bucket>/<literal-key>` within the selected Pod, not provider bucket names or URLs. Keys are literal UTF-8: quote shell metacharacters, and do not pre-percent-encode spaces, plus signs, or percent signs. Repeated and trailing slashes are preserved; dot-segment object keys are rejected to prevent URL normalization. Listings show one level of objects/prefixes and automatically fetch every page. `--json` returns an `items` array or a transfer/deletion result.

Transfers require explicit file/key destinations (no implicit basename or directory expansion). Local destinations must not already exist; downloads are saved atomically after success. Remote destinations replace existing keys. All `cp`/`mv` transfers, including downloads and gateway-to-gateway copies, and file-based `upload` are limited to **25 MiB** and buffered in bounded memory. Explicit streaming upload/download modes are described below. Gateway copies download then upload, not native CopyObject. `mv` deletes its source only after a successful copy; a deletion failure reports that both copies may remain. Moves are not atomic: avoid concurrent changes to the source. `rm` deletes one exact key, including a trailing-slash marker, never prefix contents.

The default gateway origin is `https://s3.brainpod.io`; the CLI prepends the selected Pod name to its hostname. Override the base origin with `--s3-endpoint`, `BRAINPOD_S3_ENDPOINT`, or `brainpod config set s3-endpoint https://s3.example.test` (in that precedence order). For local development, use `http://s3.brainpod.localhost` with working wildcard DNS. HTTP is unencrypted and should only be used locally. The API endpoint does not change the S3 origin.

Bucket/object listings, PUT/delete, and multipart initiation, parts, completion, and abort use typed `aws-sdk-s3` operations and SDK XML serialization/deserialization. A gateway transport keeps logical buckets path-style beneath the Pod hostname, uses unsigned SDK auth with Bearer authentication added only at the gateway, removes the SDK's `x-id` query parameter and nonsemantic `x-amz-user-agent` telemetry header, disables optional checksums, and requires buffered, non-`aws-chunked` request bodies. Requests are not automatically retried. Response byte limits remain in place; XML parsing and service-error detection are delegated to the SDK.

Downloads explicitly accept the gateway's 307 redirect and use a separate unauthenticated HTTPS request to the short-lived provider URL. Both download requests remain implemented with `reqwest`, not SDK GetObject, to enforce this redirect contract. Brainpod Bearer credentials are never sent to the provider; further redirects are rejected. Presigned URLs are not printed. There is no AWS SigV4 client authentication, public HEAD, recursive/sync transfer or deletion, bucket administration, ranged reads, or metadata preservation for `cp`/`mv`. Multipart is reserved for explicit stdin streaming uploads. Transfer errors return a nonzero exit status; interrupted uploads can have an uncertain remote outcome, so inspect the key before retrying.

### Streaming transfers

`upload -` uses sequential **8 MiB** multipart parts (up to 10,000 parts, about 78 GiB), rather than buffering the entire object. It requires a multipart-capable endpoint and `s3:write`.

EOF alone cannot prove that an upstream producer succeeded, so stdin uploads require `--require-success-file PATH`. After EOF, completion is allowed only when that regular file contains exactly `commit` followed by one newline. Use a fresh private directory for each attempt and write the marker only after the entire producer pipeline succeeds. Keep the producer subshell's stdout open until after writing the marker, so EOF cannot race its creation.

```bash
set -euo pipefail
run_dir=$(mktemp -d)
trap 'rm -rf "$run_dir"' EXIT
(
  set -euo pipefail
  cat ./large-object.bin
  printf 'commit\n' > "$run_dir/success"
) | brainpod s3 upload - s3://uploads/large-object.bin \
      --require-success-file "$run_dir/success" \
      --metadata project=example
```

Generic object metadata is accepted on multipart initiation using repeated `--metadata KEY=VALUE`. Keys must be nonempty valid HTTP header names; values must be nonempty printable ASCII. Metadata names and values together may total at most 2048 bytes, and duplicate keys are rejected case-insensitively. Endpoint-specific metadata policies are enforced by the server.

Missing or invalid markers, input errors, rejected parts, completion errors reported by the SDK (including HTTP 200 `<Error>`), SIGINT, and SIGTERM fail the upload and attempt to abort it. Completion/network failures can have an uncertain outcome; inspect the exact key before retrying. Abrupt process termination or failed aborts can leave incomplete uploads requiring endpoint cleanup.

Stream an object to stdout without the small-copy limit:

```sh
brainpod s3 download s3://uploads/large-object.bin - > ./large-object.bin
```

`download` currently requires destination `-`. Stdout contains only raw object bytes, even with `--json`; diagnostics stay on stderr. Provider requests use unauthenticated HTTPS with redirects disabled. An interrupted transfer can emit a partial object, so check the complete pipeline's exit status; early consumer closure follows normal Unix broken-pipe behavior. Network inactivity is bounded, but streaming downloads have no whole-object timeout or size cap. Input/output use bounded worker handoffs so cancellation does not wait for an unresponsive pipe reader/writer.
