# Brainpod CLI

A non-interactive CLI for managing Brainpod pods, revisions, resources, deployments, and events. Its default output is deterministic line-oriented text suitable for LLMs and shell tools. Add `--json` to receive one JSON document matching the API response.

Image building is intentionally outside the current scope.

## Configuration

The default configuration file is `~/.config/brainpod/config.toml`. `XDG_CONFIG_HOME` is respected, and `BRAINPOD_CONFIG` can override the complete path.

```sh
brainpod config set api-key brain_example
brainpod config set pod my-pod
brainpod config set endpoint https://api.brainpod.io
brainpod config show
brainpod config path
```

Configuration uses TOML:

```toml
endpoint = "https://api.brainpod.io"
api_key = "brain_example"
pod = "my-pod"
```

Values are resolved in this order:

1. Global flags: `--endpoint`, `--api-key`, `--pod`
2. `BRAINPOD_API_ENDPOINT`, `BRAINPOD_API_KEY`, `BRAINPOD_POD`
3. The configuration file
4. The default API endpoint, `https://api.brainpod.io`

The config file is written with mode `0600` on Unix. `config show` never reveals the API key.

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

Errors go to stderr and return a non-zero exit code. With `--json`, errors also use JSON and API errors retain the API's stable error code, request ID, and details. Account-limit validation errors include instructions and an `upgradeUrl` pointing to `https://brainpod.io/onboarding?upgrade=1`.

## Commands

```text
brainpod whoami
brainpod pod list
brainpod pod create [--display-name <name>]
brainpod pod get <pod>

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

brainpod --pod <pod> events --kind <app|http-access|k8s> --resource <name> \
  [--level <trace|debug|info|warn|error>] [--search <text>] \
  [--range <5m|15m|30m|1h|24h|7d>] [--cursor <cursor>]
```

Pod-scoped commands use `--pod`, `BRAINPOD_POD`, or the configured default pod. Resource kinds are `app`, `config`, `route`, `postgres`, `mariadb`, `valkey`, and `disk`. Namespace is currently fixed to the API-supported `default` namespace.

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
