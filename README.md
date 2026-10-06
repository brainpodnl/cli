<picture>
  <source media="(prefers-color-scheme: dark)" srcset=".github/hero-dark.gif">
  <img src=".github/hero.gif" alt="Brainpod — build it in Claude, Cursor or Codex, host it in Europe" width="900">
</picture>

# Brainpod CLI

A non-interactive CLI for managing Brainpod pods, images, blueprints, revisions, resources, deployments, tunnels, and events. Its default output is deterministic line-oriented text suitable for LLMs and shell tools. Add `--json` to receive machine-readable JSON; login, tunnels, and event watches use NDJSON.

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

The result is `result/bin/brainpod.exe`. API commands and tunnels work natively on Windows. Railpack image builds are unavailable because Railpack does not publish a Windows binary; use a Dockerfile or run image builds under WSL2.

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

brainpod --pod <pod> tunnel <resource> [<listen-address>] [--port <port>] [--skip-preflight]

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

`brainpod tunnel` creates a two-hour tunnel session and forwards local TCP connections until Ctrl-C is pressed. Select the pod with `--pod`, `BRAINPOD_POD`, or the configured default, then identify the target by name, URN, or stable UUID: a deployed PostgreSQL, MariaDB, Valkey, or Microsoft SQL Server resource, or an app that declares ports. For example, `brainpod --pod my-pod tunnel db` resolves `db` through the API before opening the tunnel.

A session reaches exactly one remote port. Databases use their engine port and ignore `--port`. An app that declares a single port needs no `--port`; an app that declares several requires one, and the error lists the ports it exposes. To reach two ports of the same app, run two tunnels.

The listener defaults to `127.0.0.1` and the remote port, so `brainpod --pod my-pod tunnel web` on an app serving 8080 listens on `127.0.0.1:8080`. Pass an explicit address such as `127.0.0.1:15432` to override it.

Before accepting connections the command prints a banner with the local-to-remote port mapping and a client command. Database targets also preflight their managed credentials and print the username, database, password, and DSN, so GUI clients such as DBeaver can be configured; pass `--skip-preflight` to suppress that. Apps have no managed credentials, so nothing is fetched and no password is printed.

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

An app tunnel prints the same frame without the credential block:

```text
╭─ ◆ Brainpod tunnel
│
│  App
│  Local      127.0.0.1:8080
│  Remote     App:8080
│
├─ Client
│  curl http://127.0.0.1:8080/
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
