# Brainpod CLI

A non-interactive CLI for managing Brainpod pods, images, blueprints, revisions, resources, deployments, and events. Its default output is deterministic line-oriented text suitable for LLMs and shell tools. Add `--json` to receive machine-readable JSON; event watches use NDJSON.

The CLI builds application images locally from an existing Dockerfile or with Railpack, then pushes them directly to the selected pod's private Brainpod registry namespace. Image builds probe the API's cluster architectures, prefer amd64 and then arm64, and store the selected default architecture in the configuration. Use `--platform linux/arm64` for a one-off override.

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
brainpod config set registry-endpoint https://registry.brainpod.io
brainpod config set architecture arm64
brainpod config show
brainpod config path
```

Configuration uses TOML:

```toml
endpoint = "https://api.brainpod.io"
registry_endpoint = "https://registry.brainpod.io"
api_token = "brain_example"
pod = "my-pod"
architecture = "amd64"
```

Values are resolved in this order:

1. Global flags: `--endpoint`, `--registry-endpoint`, `--api-token`, `--pod`
2. `BRAINPOD_API_ENDPOINT`, `BRAINPOD_REGISTRY_ENDPOINT`, `BRAINPOD_API_TOKEN`, `BRAINPOD_POD`
3. The configuration file
4. The defaults `https://api.brainpod.io` and `https://registry.brainpod.io`

For image builds, `--platform` overrides the configured architecture. Without it, the CLI probes the available clusters, prefers `amd64` and then `arm64`, and stores the selected architecture in the configuration.

`brainpod login` uses `https://brainpod.io` as its dashboard and supports overriding it with `BRAINPOD_DASHBOARD_ENDPOINT` for local or test environments.

The config file is written with mode `0600` on Unix. `config show` never reveals the API token.

## Output contract

Text is the default. Each endpoint has a purpose-built renderer. Collection endpoints use plain tables without terminal control sequences, while detail and mutation endpoints use concise labeled sections:

```text
NAME    HEAD STATUS  HEAD                                           DEPLOYED
------  -----------  ---------------------------------------------  ---------------------------------------------
my-pod  ready        v12 (1a2b3c4d-1111-2222-3333-444455556666)    v11 (7e8f9a0b-1111-2222-3333-444455556666)
```

JSON mode bypasses text rendering and emits the complete API response as one JSON value:

```sh
brainpod pod list --json
brainpod --json resource list
```

Event watches are streamed as newline-delimited JSON so each event is available immediately. Each line contains the SSE event name, event ID, and decoded data.

Text event output uses color for timestamps, levels, platform events, and HTTP statuses when stdout is a terminal. JSON and redirected output never contain ANSI color sequences.

Errors go to stderr and return a non-zero exit code. With `--json`, errors also use JSON and API errors retain the API's stable error code, request ID, and details. Account-limit validation errors include instructions and an `upgradeUrl` pointing to `https://brainpod.io/onboarding?upgrade=1`.

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

brainpod --pod <pod> resource list [--revision <uuid> | --at <timestamp>]
brainpod --pod <pod> resource get <kind> <name> [--revision <uuid> | --at <timestamp>]
brainpod --pod <pod> resource create --file <path|-> [--dry-run]
brainpod --pod <pod> resource replace <kind> <name> --file <path|->
brainpod --pod <pod> resource delete <kind> <name>

brainpod --pod <pod> deploy [--summary <text>]
brainpod --pod <pod> redeploy

brainpod --pod <pod> events --resource <urn> [--kind <app|http-access|platform>] \
  [--level <trace|debug|info|warn|error>] [--search <text>] \
  [--range <5m|15m|30m|1h|24h|7d>] [--cursor <cursor>]
brainpod --pod <pod> events --watch --resource <urn> \
  [--kind <app|http-access|platform>] [--level <trace|debug|info|warn|error>] \
  [--search <text>] [--range <5m|15m|30m|1h|24h|7d>] [--cursor <cursor>] \
  [--duration <1-20>] [--last-event-id <id>]
```

Events use the resource URN returned by resource list, get, or mutation responses, such as `urn:brain:app:default:api`. Omit `--kind` to return every stream available for that resource. `--level` requires `--kind app`.

Event watches flush text or JSON output as messages arrive and reconnect after each server-imposed stream duration, continuing until interrupted. The per-request duration defaults to 10 seconds. Reconnects use the latest SSE event ID to avoid replaying emitted events. Use `--last-event-id` to set the initial event ID; `--cursor` resumes the initial request from an API event cursor.

`brainpod cluster list` lists active clusters and their supported architectures.

Pod-scoped commands use `--pod`, `BRAINPOD_POD`, or the configured default pod. Resource kinds are `app`, `config`, `route`, `postgres`, `mariadb`, `valkey`, and `disk`. Namespace is currently fixed to the API-supported `default` namespace.

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
