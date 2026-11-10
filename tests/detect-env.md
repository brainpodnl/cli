# Name-only environment detection

```sh
brainpod detect . --with-env
brainpod detect ./services --with-env --json
brainpod detect . --with-env --with-source --json
```

Without the flag, environment adapters are not run, no additional files are read, and the `environment` field is absent. With it, the field is present even when empty. Project/framework/runtime detection is unchanged.

**`--with-env` reads metadata files only**: the base five formats and the platform manifests below. It never reads application source. **`--with-source`** (accepted only together with `--with-env`) adds [code references](#code-references): every source file with a cataloged extension is searched for a fixed list of platform-injected names and framework prefixes. This multiplies reads; see [local results](#local-results-2026-11-10). Without flags, reads and output are byte-identical to the base branch.

## Why the base five

This is a practical initial selection, **not a measured global top-five ranking**. Research on 2026-11-10 used public, read-only documentation and GitHub repository metadata:

- **Dotenv:** the cross-language `.env` convention; [motdotla/dotenv](https://github.com/motdotla/dotenv) had 20,563 stars. Its [parser rules](https://github.com/motdotla/dotenv#what-rules-does-the-parsing-engine-follow) explicitly describe assignments, comments, quotes and multiline values.
- **Compose:** [docker/compose](https://github.com/docker/compose) had 38,674 stars. The [Compose specification](https://github.com/compose-spec/compose-spec/blob/main/spec.md#environment) explicitly distinguishes `environment`, pass-through keys and `env_file` declarations.
- **Dockerfile:** Docker is described as near-universal in the [2025 Stack Overflow technology survey](https://survey.stackoverflow.co/2025/technology/). The [official frontend reference](https://github.com/moby/buildkit/blob/master/frontend/dockerfile/docs/reference.md#env) distinguishes persisted `ENV` declarations from build-only `ARG` (BuildKit had 10,382 stars).
- **Kubernetes:** [kubernetes/kubernetes](https://github.com/kubernetes/kubernetes) had 128,969 stars. Its [environment documentation](https://kubernetes.io/docs/tasks/inject-data-application/define-environment-variable-container/) explicitly defines `env` and `envFrom` without requiring cluster queries.
- **GitHub Actions:** an explicit, declarative CI format with [documented workflow/job/step `env` maps](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#env). [actions/runner](https://github.com/actions/runner) had 6,386 stars; the pinned local corpus independently supplied environment declarations in 42 of 51 repositories. This sample is web-project-biased, not market-share evidence.

Stars are adoption proxies, not comparable format-usage counts. No private repository content was sent to a service. Context7 was unavailable; primary documentation was read directly.

## Exact coverage

Filename/path routing is declared in `catalog.environment` in [`rules.toml`](../src/detect/rules.toml), compiled alongside the existing catalog. First matching entry wins. Syntax stays in the five base adapters in [`environment.rs`](../src/detect/environment.rs) and the platform adapters in [`environment/platforms.rs`](../src/detect/environment/platforms.rs). No executable configuration language or config-key guessing is involved. Platform entries come after the base entries, so a platform file inside a Kubernetes metadata directory (for example `deploy/app.yaml`) keeps Kubernetes routing.

| Format | Candidates | Supported declarations |
|---|---|---|
| `dotenv` | `.env`, `.env.*`, `*.env`, `*.env.example`, `*.env.sample`, `*.env.template`; excludes `*.enc` and `.env.vault` | `NAME=value`, optional `export`, whitespace, empty values, comments, single/double/backtick quotes, escaped quotes and quoted multiline values. BOM/CRLF accepted. Bare names, colon assignment and shell programs are not supported. |
| `compose` | `compose.yml/yaml`, `docker-compose.yml/yaml` and dotted variants such as `compose.override.yaml` | `services.*.environment` scalar maps or string lists (`NAME=value` or bare `NAME`); null/bare entries are `external`. `env_file` string/list/path-mapping entries become unresolved references, not file reads. |
| `dockerfile` | `Dockerfile`, `Dockerfile.*`, `*.Dockerfile` | Case-insensitive `ENV` and `ARG`, multiple assignments, legacy `ENV NAME value`, quotes, escaped spaces, backslash/backtick continuations, continued comments, and `escape` directives. Each `FROM` advances a numbered stage; `ARG` before the first `FROM` is stage 0. [`ARG`](https://docs.docker.com/reference/dockerfile/#arg) records carry `scope: "build"` and `hasDefault` (whether `=default` is present; the default is never returned). `ENV` records are unchanged. `RUN` assignments and base-image environments are not inferred. |
| `kubernetes` | YAML/JSON in directories named `k8s`, `kubernetes`, `kube`, `manifests`, `deploy`, `deployment`, `deployments`; also `pod.yml/yaml`, `deployment.yml/yaml`, `kubernetes-*.yml/yaml`, `*.k8s.yml/yaml/json`, `*.kube.yml/yaml/json` | Core `v1` Pod/List/ReplicationController, `apps/v1` Deployment/StatefulSet/DaemonSet/ReplicaSet, `batch/v1` Job/CronJob. Known pod-spec paths only; regular/init/ephemeral containers; explicit `env[].name`, `valueFrom`, `envFrom` and prefixes. Lists and multi-document streams supported. |
| `github-actions` | `.github/workflows/*.yml/yaml` at any repository depth | Only workflow `env`, `jobs.*.env` and `jobs.*.steps[].env` maps. Whole-map expressions are unresolved references. `with`, matrix keys, scripts and writes to `$GITHUB_ENV` are not inferred. |
| `app-json` | `app.json`, `scalingo.json` | `env` and `environments.<name>.env`. A string is a literal value; an object may state `required`, `value` and `generator`. Files without `env` (for example Expo `app.json`) produce nothing. Parsed as JSON; a duplicate key keeps the last value, as JSON parsers on these platforms do. |
| `heroku-yml` | `heroku.yml` | `setup.config` (provisioning config vars, `runtime`) and `build.config` (build-time only, `build`). |
| `render` | `render.yaml` | `envVars` of `services` and `envVarGroups` at the root, in `ungrouped`, and in `projects[].environments[]`. |
| `fly` | `fly.toml`, `fly.*.toml` | `[env]` (`runtime`). `[build.args]` are Docker build arguments and are not read. |
| `digitalocean` | `.do/app.yaml`, `.do/deploy.template.yaml` at any depth | App-level `envs` and the `envs` of `services`, `static_sites`, `workers`, `jobs` and `functions`. A Deploy button template's spec under `spec` is supported. |
| `vercel` | `vercel.json`, `vercel.toml` | Legacy `env` (functions, `runtime`) and `build.env` (`build`). |
| `netlify` | `netlify.toml` | `build.environment`, `context.<name>.environment` (`build`) and `template.environment`. |
| `app-engine` | `app.yaml` with a top-level `runtime` | `env_variables` (`runtime`) and `build_env_variables` (`build`). |
| `copilot` | `copilot/<name>/manifest.yml/yaml` at any depth, except `type: Environment` | `variables`, `secrets` and `env_file` at the top level, in `sidecars.<name>` and in `environments.<name>` overrides. |

TOML (`fly.toml`, `netlify.toml`, `vercel.toml`) uses the existing `toml` dependency, which already parses detection rules and manifests; its parser rejects excessive nesting (a 5,000-level array is a redacted diagnostic, not a crash). `app.json`, `scalingo.json` and `vercel.json` use `serde_json`, whose recursion limit is 128. YAML supports flow/block collections, quoted/block scalars and bounded anchors/merge keys. Duplicate mapping keys, unsupported tags, malformed input or budget breaches reject the entire YAML stream with a redacted diagnostic. No includes or property interpolation are enabled. Kubernetes candidates are intentionally narrower than all YAML: application data, Rails fixtures, Spring properties, arbitrary JSON, CRDs, Helm rendering and Kustomize evaluation are not scanned/inferred as environments. Use an extra catalog entry for a nonconventional manifest location.

Names use the conservative common subset `[A-Za-z_][A-Za-z0-9_.-]*`. Unsupported/dynamic names are skipped with diagnostics. This is not a claim to accept every name allowed by every ecosystem. Kubernetes resource/key references additionally allow a leading digit. Simple downward-API field/resource paths are retained as reference keys; complex field selectors are left unresolved.

A `RUN`/`COPY`/`ADD` body containing `<<` conservatively stops inspection of that Dockerfile at the containing instruction with a diagnostic, preventing script bodies from becoming false `ENV` declarations. Instructions after the heredoc and experimental `ENV` flags are not covered. Custom Dockerfile frontends, Compose includes/extends/overlays, precedence, image stages/inheritance, workflow activation and environment installation are not evaluated.

## Platform deployment manifests

Deployment files written for other hosts are the best structured source of per-variable intent. They often state whether a variable is required, secret, generated, or wired to a database or service. Primary documentation was read on 2026-11-10. A format is included only when its documentation defines variable declarations inside the repository.

Included:

- **Heroku, Dokku and Scalingo `app.json` / `scalingo.json`:** [Heroku app.json schema](https://devcenter.heroku.com/articles/app-json-schema#env), [Dokku app.json](https://dokku.com/docs/appendices/file-formats/app-json/#env), [Scalingo manifest](https://doc.scalingo.com/platform/app/app-manifest) and [scalingo.json schema](https://developers.scalingo.com/scalingo-json-schema). Scalingo also reads `app.json` when no `scalingo.json` exists.
- **Heroku `heroku.yml`:** [setup config vars and build-time variables](https://devcenter.heroku.com/articles/build-docker-images-heroku-yml#setup-define-your-app-s-environment).
- **Render `render.yaml`:** [Blueprint reference, environment variables](https://render.com/docs/blueprint-spec#environment-variables).
- **Fly.io `fly.toml`:** [the `[env]` section](https://fly.io/docs/reference/configuration/#the-env-variables-section). Secrets are set with `fly secrets` outside the repository.
- **DigitalOcean App Platform:** [app spec `envs`](https://docs.digitalocean.com/products/app-platform/reference/app-spec/), [Deploy button template](https://docs.digitalocean.com/products/app-platform/how-to/add-deploy-do-button/#configure-environment-variables), [bindable variables](https://docs.digitalocean.com/products/app-platform/how-to/use-environment-variables/#use-bindable-variables), and the [dev config default `.do/app.yaml`](https://docs.digitalocean.com/products/app-platform/reference/dev-config/).
- **Vercel `vercel.json` / `vercel.toml`:** the [legacy `env` and `build.env`](https://vercel.com/docs/project-configuration/vercel-json#env) properties, which Vercel recommends against but still supports. [`vercel.toml` supports the same fields](https://vercel.com/docs/project-configuration/vercel-toml).
- **Netlify `netlify.toml`:** [build and context environments](https://docs.netlify.com/build/configure-builds/file-based-configuration/#deploy-contexts), [which are available to builds but not Functions](https://docs.netlify.com/build/environment-variables/overview/#configuration-options-and-limitations), and [template variables for the Deploy button](https://docs.netlify.com/deploy/create-deploys/#file-based-template-configuration).
- **Google App Engine `app.yaml`:** [`env_variables` and `build_env_variables`](https://cloud.google.com/appengine/docs/standard/reference/app-yaml#runtime_and_app_elements). The mandatory `runtime` element distinguishes App Engine files from other `app.yaml` files.
- **AWS Copilot workload manifests:** [`variables`, `secrets` and `env_file`](https://aws.github.io/copilot-cli/docs/manifest/lb-web-service/) and [secrets](https://aws.github.io/copilot-cli/docs/developing/secrets/). Copilot CLI reached end of support on 2026-06-12, but existing manifests remain in repositories.

Excluded:

- **Railway:** `railway.json`/`railway.toml` [Config as Code](https://docs.railway.com/reference/config-as-code) only carries build/deploy settings, has no variables, and is deprecated (no longer read after 2026-12-01). Its replacement, [Infrastructure as Code](https://docs.railway.com/infrastructure-as-code), is executable TypeScript/Python/Go evaluated by the CLI. Executing or interpreting it is out of scope.
- **Koyeb:** no in-repository manifest. The [Deploy to Koyeb button](https://www.koyeb.com/docs/build-and-deploy/deploy-to-koyeb-button) passes `env[NAME]=value` URL parameters.
- **README Deploy-button URLs** (Vercel `env` query parameters, Netlify URL hashes, Koyeb `env[...]`): unstructured links, not manifests. **`vercel.ts`** is executable configuration.
- **Fly.io `fly.json`/`fly.yaml`:** alternate formats produced by `fly config save --json/--yaml`, with no fixed documented filename. They are not routed.
- **Heroku add-ons:** add-ons such as `heroku-postgresql` set config vars (for example `DATABASE_URL`) on the platform side. `app.json` does not name them, so nothing is inferred.

### Semantic fields

Platform records add the following optional fields. Each is present **only when the manifest states it**, and existing fields keep their meaning. Base-format records only gain them for Dockerfile `ARG` (above).

| Field | Meaning | Set by |
|---|---|---|
| `required` | Stated requirement. | `app.json`/`scalingo.json` `required`. Heroku and Dokku document a default of `true` when it is omitted, and Scalingo documents none; that default is **not** applied. Netlify `template.environment` (documented as required) gives `true`. |
| `secret` | The platform treats the value as sensitive. `false` means the format explicitly classifies it as plain. | `true`: Render `sync: false` ([prompted secret values](https://render.com/docs/blueprint-spec#prompting-for-secret-values)) and `generateValue`, `app.json` `generator: "secret"`, DigitalOcean `type: SECRET`, Copilot `secrets`, Vercel `@name` secret references. `false`: DigitalOcean `type: GENERAL`, Copilot `variables`. |
| `generated` | The platform generates the value. | `app.json` `generator` (Heroku/Dokku `secret`; Scalingo `secret`, `url`, `template`), Render `generateValue: true`. |
| `hasDefault` | Whether the file supplies a literal value (fixed or default). **The value is never returned.** `false` means the entry exists without a value and must come from elsewhere. | `true`: literal map entries, `app.json` strings and `value`, Render `value`, DigitalOcean `value`, Dockerfile `ARG NAME=…`. `false`: `app.json` objects without `value`, Render `sync: false`, DigitalOcean entries without `value`, YAML `NAME:` with a null value, Netlify `template.environment` (its value is a UI label), `ARG NAME`. Omitted for references and generated values. |
| `scope` | `runtime`, `build` or `build-and-runtime`, when the docs fix it. | DigitalOcean `RUN_TIME`/`BUILD_TIME`/`RUN_AND_BUILD_TIME`; Vercel `env`/`build.env`; Netlify toml environments; Fly `[env]`; App Engine `env_variables`/`build_env_variables`; `heroku.yml` `setup.config`/`build.config`; Copilot containers; Dockerfile `ARG`. |

Wiring to another resource uses the existing `reference` shape with `state: "unresolved"`:

- **Render:** `fromDatabase` → `database` (target = database name, key = property such as `connectionString`). `fromService` with `property` → `service`, or `key-value` for `type: keyvalue`/`redis`. `fromService` with `envVarKey` → `service-env`. `fromGroup` → a nameless `env-group` record; group contents in other files are not followed. Render ignores `sync: false` inside environment groups, so those entries are skipped with a diagnostic.
- **DigitalOcean:** a whole-value binding `${db.VAR}` becomes `database` when `db` is listed in the spec's `databases` (including 32-character prefixes). Otherwise `${component.VAR}`/`${_self.VAR}` becomes `service`, and `${APP_URL}` becomes `app` with key `APP_URL`. The pool form `${db.pool.VAR}` keeps the database and the variable. Any other value containing `${` becomes `interpolation` with no target. `type: SECRET` values are never inspected.
- **Vercel** `@name` → `platform-secret`. **Copilot** string secrets → `ssm-parameter`, `secretsmanager` → `secrets-manager`, `from_cfn` → `cloudformation-export`, `env_file` → `env-file`.
- A target or key is kept only when it is a simple name (`[A-Za-z0-9_.-]`). SSM parameter paths, ARNs and anything with `:` or `/` are omitted.

Environment, context and sidecar names used as location segments must be simple names; others are skipped with a diagnostic.

## Code references

Some variables are never declared in repository files: platforms inject them at build or run time, and frameworks expose prefixed variables to client code by convention. `--with-env --with-source` therefore runs a literal search over source files; `--with-env` alone does not, and does not read source files at all. This is **not** a read-site scanner. Only the names and prefixes in `catalog.code_references` in [`rules.toml`](../src/detect/rules.toml) are recognised; arbitrary `process.env.X`/`os.environ[...]` reads are not. Patterns are data: an extra `--rules` file can add patterns, extensions and exclusions without code changes.

- **Files:** basenames with extensions `js jsx mjs cjs ts tsx mts cts vue svelte astro py rb go rs php java kt kts cs ex exs`, excluding `*.min.js`, `*.min.mjs`, `*.min.cjs` and `*.bundle.js`. The existing directory exclusions apply (`node_modules`, `vendor`, `dist`, `build`, `.next`, `.nuxt`, `.output`, `target`, virtualenvs and similar), as do symlink skipping, the 128-level depth limit and the 1 MiB per-file limit. Oversized source files are skipped with the diagnostic `source exceeds 1 MiB read limit; environment references not scanned`. Bytes are searched directly, so non-UTF-8 files are scanned without a diagnostic.
- **Matching:** the file is split into whole ASCII identifier tokens (`[A-Za-z0-9_]+`, bounded by any other byte, so `$VITE_X` and `env.VITE_X` match). A token matches an exact catalog name, or starts with a catalog prefix and continues with one or more `[A-Z0-9_]` characters (`\bPREFIX[A-Z0-9_]+\b`). The prefix alone, lowercase or mixed-case continuations, and longer identifiers that merely contain a prefix (`MY_NEXT_PUBLIC_X`) do not match. Exact names win over prefixes, and the longest prefix wins (`NUXT_PUBLIC_` before `NUXT_`).
- **Generic names:** names that collide with ordinary code are never cataloged: `URL`, `CONTEXT`, `HEAD`, `BRANCH`, `PORT`, `CI`, `NODE_ENV`, `HOSTNAME`, `DATABASE_URL`, `WEB_CONCURRENCY`, `ACCOUNT_ID` and the OpenTelemetry names. Moderately generic documented names are cataloged with `signals`: they are reported only when the platform's manifest basename exists anywhere in the scan (for example Netlify `DEPLOY_URL` requires a `netlify.toml`). The generic `PUBLIC_` prefix uses `requires` and is reported only when the owning project is detected as SvelteKit or Astro. Rsbuild, Qwik, Farm and Modern.js prefixes have no detection rule to gate on and are not cataloged.
- **Records:** one record per `(project, name)` with `format: "code-reference"`, `state: "referenced"`, `origin` (`platform` or `framework`), `provider` (the pattern id) and `scope` when documented. `source`/`location` (`line:N`) point to the first occurrence. `occurrences` lists sorted `{source, line}` pairs, **capped at 20**, and `occurrenceCount` is the uncapped total. Project ownership is the nearest detected ancestor, as for declarations. No code text, surrounding context or value is retained or returned.
- **Attribution is by convention, not proof:** a `NEXT_PUBLIC_*` reference in a non-Next.js package is still attributed to `nextjs`. Vercel's framework-prefixed system variables (such as `NEXT_PUBLIC_VERCEL_URL`) are reported under the framework prefix. Fly documents `FLY_` as a reserved prefix, but only its documented names are cataloged because users also set `FLY_*` CLI tokens.

Patterns and their sources (research 2026-11-10):

| Origin | Provider | Matches (scope) | Gate | Documentation |
|---|---|---|---|---|
| framework | `nextjs` | `NEXT_PUBLIC_*` (build) | — | [1](https://nextjs.org/docs/app/guides/environment-variables#bundling-environment-variables-for-the-browser) |
| framework | `vite` | `VITE_*` (build) | — | [1](https://vite.dev/guide/env-and-mode#env-variables) [2](https://docs.solidjs.com/configuration/environment-variables#public-environment-variables) [3](https://tanstack.com/start/latest/docs/framework/react/guide/environment-variables) |
| framework | `create-react-app` | `REACT_APP_*` (build) | — | [1](https://create-react-app.dev/docs/adding-custom-environment-variables/) |
| framework | `nuxt` | `NUXT_PUBLIC_*`, `NUXT_*` (runtime) | — | [1](https://nuxt.com/docs/4.x/guide/going-further/runtime-config#environment-variables) |
| framework | `sveltekit` | `PUBLIC_*` (unstated) | owning project detected as `sveltekit` | [1](https://svelte.dev/docs/kit/$env-static-public) [2](https://svelte.dev/docs/kit/$env-dynamic-public) |
| framework | `astro` | `PUBLIC_*` (build) | owning project detected as `astro` | [1](https://docs.astro.build/en/guides/environment-variables/#vites-built-in-support) |
| framework | `expo` | `EXPO_PUBLIC_*` (build) | — | [1](https://docs.expo.dev/guides/environment-variables/#reading-environment-variables-from-env-files) |
| framework | `gatsby` | `GATSBY_*` (build) | — | [1](https://www.gatsbyjs.com/docs/how-to/local-development/environment-variables/#accessing-environment-variables-in-the-browser) |
| framework | `vue-cli` | `VUE_APP_*` (build) | — | [1](https://cli.vuejs.org/guide/mode-and-env.html#environment-variables) |
| framework | `redwood` | `REDWOOD_ENV_*` (build) | — | [1](https://docs.redwoodjs.com/docs/environment-variables#option-2-prefixing-with-redwood_env_) |
| framework | `storybook` | `STORYBOOK_*` (build) | — | [1](https://storybook.js.org/docs/configure/environment-variables) |
| framework | `sanity` | `SANITY_STUDIO_*` (build) | — | [1](https://www.sanity.io/docs/studio/environment-variables#exposed-variables) |
| framework | `umi` | `UMI_APP_*` (build) | — | [1](https://umijs.org/en-US/docs/guides/env-variables#use-environment-variables-in-browser) |
| framework | `plasmo` | `PLASMO_PUBLIC_*` (build) | — | [1](https://docs.plasmo.com/framework/env#custom-environment-variables) |
| framework | `wxt` | `WXT_*` (build) | — | [1](https://wxt.dev/guide/essentials/config/environment-variables#dotenv-files) |
| framework | `tauri` | `TAURI_ENV_*` (build) | — | [1](https://v2.tauri.app/reference/environment-variables/#tauri-cli-hook-commands) |
| framework | `quasar` | `QCLI_*`, `QUASAR_*` (build) | — | [1](https://quasar.dev/quasar-cli-vite/handling-import-meta-env#from-dotenv-files) [2](https://quasar.dev/quasar-cli-vite/handling-import-meta-env#values-provided-by-quasar-cli) |
| framework | `electron-vite` | `MAIN_VITE_*`, `PRELOAD_VITE_*`, `RENDERER_VITE_*` (build) | — | [1](https://electron-vite.org/guide/env-and-mode#global-env-variables) |
| platform | `vercel` | `VERCEL_OIDC_TOKEN`, `VERCEL_HASH_SALT`, `VERCEL_GIT_PREVIOUS_SHA` (build); `VERCEL`, `VERCEL_ENV`, `VERCEL_TARGET_ENV`, `VERCEL_URL`, `VERCEL_BRANCH_URL`, `VERCEL_PROJECT_PRODUCTION_URL`, `VERCEL_DEPLOYMENT_ID`, `VERCEL_PROJECT_ID`, `VERCEL_PROJECT_NAME`, `VERCEL_SKEW_PROTECTION_ENABLED`, `VERCEL_AUTOMATION_BYPASS_SECRET`, `VERCEL_GIT_PROVIDER`, `VERCEL_GIT_REPO_SLUG`, `VERCEL_GIT_REPO_OWNER`, `VERCEL_GIT_REPO_ID`, `VERCEL_GIT_COMMIT_REF`, `VERCEL_GIT_COMMIT_SHA`, `VERCEL_GIT_COMMIT_MESSAGE`, `VERCEL_GIT_COMMIT_AUTHOR_LOGIN`, `VERCEL_GIT_COMMIT_AUTHOR_NAME`, `VERCEL_GIT_PULL_REQUEST_ID` (build-and-runtime); `VERCEL_REGION` (runtime) | — | [1](https://vercel.com/docs/environment-variables/system-environment-variables#system-environment-variables) |
| platform | `netlify` | `NETLIFY`, `NETLIFY_PREVIEW_SERVER`, `CACHED_COMMIT_REF`, `DEPLOY_PRIME_URL`, `INCOMING_HOOK_TITLE`, `INCOMING_HOOK_URL`, `INCOMING_HOOK_BODY` (build); `BUILD_ID`, `REPOSITORY_URL`, `COMMIT_REF`, `PULL_REQUEST`, `REVIEW_ID`, `DEPLOY_URL`, `DEPLOY_ID` (build); `SITE_NAME`, `SITE_ID` (build-and-runtime) | names `BUILD_ID`, `REPOSITORY_URL`, `COMMIT_REF`, `PULL_REQUEST`, `REVIEW_ID`, `DEPLOY_URL`, `DEPLOY_ID` need `netlify.toml`; names `SITE_NAME`, `SITE_ID` need `netlify.toml` | [1](https://docs.netlify.com/build/configure-builds/environment-variables/#read-only-variables) |
| platform | `render` | `RENDER_CPU_COUNT`, `RENDER_DISCOVERY_SERVICE`, `RENDER_EXTERNAL_HOSTNAME`, `RENDER_EXTERNAL_URL`, `RENDER_GIT_BRANCH`, `RENDER_GIT_COMMIT`, `RENDER_GIT_REPO_SLUG`, `RENDER_INSTANCE_ID`, `RENDER_SERVICE_ID`, `RENDER_SERVICE_NAME`, `RENDER_SERVICE_TYPE` (build-and-runtime); `RENDER_WEB_CONCURRENCY` (runtime); `RENDER`, `IS_PULL_REQUEST` (build-and-runtime) | names `RENDER`, `IS_PULL_REQUEST` need `render.yaml` | [1](https://render.com/docs/environment-variables#all-runtimes) |
| platform | `railway` | `RAILWAY_PUBLIC_DOMAIN`, `RAILWAY_PRIVATE_DOMAIN`, `RAILWAY_TCP_PROXY_DOMAIN`, `RAILWAY_TCP_PROXY_PORT`, `RAILWAY_TCP_APPLICATION_PORT`, `RAILWAY_PROJECT_NAME`, `RAILWAY_PROJECT_ID`, `RAILWAY_ENVIRONMENT_NAME`, `RAILWAY_ENVIRONMENT_ID`, `RAILWAY_SERVICE_NAME`, `RAILWAY_SERVICE_ID`, `RAILWAY_REPLICA_ID`, `RAILWAY_REPLICA_REGION`, `RAILWAY_DEPLOYMENT_ID`, `RAILWAY_SNAPSHOT_ID`, `RAILWAY_VOLUME_NAME`, `RAILWAY_VOLUME_MOUNT_PATH`, `RAILWAY_GIT_COMMIT_SHA`, `RAILWAY_GIT_AUTHOR`, `RAILWAY_GIT_BRANCH`, `RAILWAY_GIT_REPO_NAME`, `RAILWAY_GIT_REPO_OWNER`, `RAILWAY_GIT_COMMIT_MESSAGE` (build-and-runtime) | — | [1](https://docs.railway.com/variables/reference#railway-provided-variables) |
| platform | `heroku` | `DYNO`, `HEROKU_APP_ID`, `HEROKU_APP_NAME`, `HEROKU_APP_DEFAULT_DOMAIN_NAME`, `HEROKU_DYNO_ID`, `HEROKU_RELEASE_CREATED_AT`, `HEROKU_RELEASE_VERSION`, `HEROKU_SLUG_COMMIT`, `HEROKU_SLUG_DESCRIPTION`, `HEROKU_BUILD_COMMIT`, `HEROKU_BUILD_DESCRIPTION` (runtime) | — | [1](https://devcenter.heroku.com/articles/dyno-metadata#dyno-metadata) |
| platform | `cloudflare-pages` | `CF_PAGES`, `CF_PAGES_COMMIT_SHA`, `CF_PAGES_BRANCH`, `CF_PAGES_URL` (build) | — | [1](https://developers.cloudflare.com/pages/configuration/build-configuration/#environment-variables) |
| platform | `app-engine` | `GAE_APPLICATION`, `GAE_DEPLOYMENT_ID`, `GAE_ENV`, `GAE_INSTANCE`, `GAE_MEMORY_MB`, `GAE_RUNTIME`, `GAE_SERVICE`, `GAE_VERSION` (runtime); `GOOGLE_CLOUD_PROJECT` (runtime) | names `GOOGLE_CLOUD_PROJECT` need `app.yaml` | [1](https://cloud.google.com/appengine/docs/standard/nodejs/runtime#environment_variables) |
| platform | `koyeb` | `KOYEB_APP_NAME`, `KOYEB_APP_ID`, `KOYEB_ORGANIZATION_NAME`, `KOYEB_ORGANIZATION_ID`, `KOYEB_SERVICE_NAME`, `KOYEB_SERVICE_ID`, `KOYEB_SERVICE_PRIVATE_DOMAIN`, `KOYEB_PUBLIC_DOMAIN`, `KOYEB_PRIVILEGED`, `KOYEB_DOCKER_REF`, `KOYEB_GIT_SHA`, `KOYEB_GIT_BRANCH`, `KOYEB_GIT_REPOSITORY`, `KOYEB_GIT_COMMIT_MESSAGE`, `KOYEB_GIT_COMMIT_AUTHOR` (build-and-runtime); `KOYEB_REGION`, `KOYEB_REGIONAL_DEPLOYMENT_ID`, `KOYEB_INSTANCE_ID`, `KOYEB_INSTANCE_TYPE`, `KOYEB_INSTANCE_MEMORY_MB`, `KOYEB_REPLICA_INDEX`, `KOYEB_HYPERVISOR_ID`, `KOYEB_DC`, `KOYEB_GPU_COUNT` (runtime) | — | [1](https://www.koyeb.com/docs/build-and-deploy/environment-variables#koyeb-provided-variables) |
| platform | `cloud-run` | `K_SERVICE`, `K_REVISION`, `K_CONFIGURATION`, `CLOUD_RUN_JOB`, `CLOUD_RUN_EXECUTION`, `CLOUD_RUN_TASK_INDEX`, `CLOUD_RUN_TASK_ATTEMPT`, `CLOUD_RUN_TASK_COUNT`, `CLOUD_RUN_WORKER_POOL`, `CLOUD_RUN_REVISION`, `FUNCTION_TARGET`, `FUNCTION_SIGNATURE_TYPE` (runtime) | — | [1](https://cloud.google.com/run/docs/container-contract#env-vars) |
| platform | `fly` | `FLY_APP_NAME`, `FLY_MACHINE_ID`, `FLY_ALLOC_ID`, `FLY_REGION`, `FLY_PUBLIC_IP`, `FLY_IMAGE_REF`, `FLY_MACHINE_VERSION`, `FLY_PRIVATE_IP`, `FLY_PROCESS_GROUP`, `FLY_VM_MEMORY_MB`, `FLY_TEST_MACHINE_IP` (runtime); `PRIMARY_REGION`, `RELEASE_COMMAND` (runtime) | names `PRIMARY_REGION`, `RELEASE_COMMAND` need `fly.toml` | [1](https://fly.io/docs/machines/runtime-environment/#environment-variables) |

## Output, provenance and privacy

Each `environment` record has:

- `name` when explicitly known; otherwise an unresolved reference record without a name.
- `source`: scan-root-relative metadata filename (for code references, the first source file); `format`: one of the base five, a platform format above, or `code-reference`.
- `location`: one-based `line:N` for dotenv and code references, numbered stage/one-based line/assignment index for Dockerfile, or zero-based `document:N` plus escaped JSON-pointer-style paths for YAML/JSON/TOML.
- `project`: nearest detected ancestor project/workspace path, or `null`. This is directory ownership, **not evidence that the application uses the variable**. Workflow files normally belong to the repository root, not projects mentioned in scripts.
- `state`: `declared`, `external` (Compose pass-through), `unresolved`, or `referenced` (code reference).
- Optional `reference`: kind and statically known target/key/prefix. Dynamic expressions and absolute/dynamic file targets are omitted, not interpolated or echoed.
- Optional `required`, `secret`, `generated`, `hasDefault` and `scope` ([semantic fields](#semantic-fields)), and for code references `origin`, `provider`, `occurrences` and `occurrenceCount`. The JSON schema is additive: existing records keep exactly their previous fields.

Records are stably sorted by source; within a source they retain adapter order (JSON/YAML/TOML mapping keys are sorted). Code references follow declarations of the same source. Repeated list entries, repeated lines and the same name in different files/services/stages/scopes remain separate. There is no last-wins merge, collision resolution or workspace-to-member copying. Existing `project.workspaces` supplies workspace membership separately. Identical names do not establish dataflow between declarations.

Literal environment values, including defaults and generated or secret entries, are never returned in JSON/text, diagnostics or retained declaration facts. Source-file bytes are only tokenised into identifiers; code text is never retained. File bytes and YAML values exist transiently in parser memory, then are discarded. No values are loaded into the process, expanded, sourced, executed, fetched or saved. Current process environment variables are not inspected. ConfigMap/Secret data keys are not enumerated, even when those resources are present locally; `envFrom` remains an unresolved declaration reference. Referenced env files are not followed; matching files independently discovered in the inventory retain their own source/project provenance.

The existing directory exclusions, symlink skipping, 128-level depth limit and 1 MiB per-file read limit apply to manifests and source files alike. Explicit symlink scan roots are rejected with the flag. Reads stop at 1 MiB + 1 byte for oversized-file diagnostics; no total repository-size cap is imposed. YAML is limited to depth 48, 256 documents, 100,000 events, 50,000 nodes, 128 anchors/aliases and 2 MiB of expanded scalar content per stream. Diagnostics never quote environment input and are deduplicated per source/message. Static metadata scanning assumes the directory tree is not concurrently replaced during traversal.

## Reproduction

No dependencies or downloaded project code need to be installed/executed. The local Cargo cache lacked `serde-saphyr` offline, so the Rust commands below ran without `--offline`. The five repositories added for platform manifests need one explicit `--fetch` into a snapshot cache; later runs are offline:

```sh
cargo test --locked -j 4
cargo clippy --locked -j 4 --all-targets -- -D warnings
cargo fmt --all -- --check
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_audit_detect.py'
cargo build --release --locked -j 4
python3 tests/audit_detect.py --list

cache=/tmp/bp-envplat/cache
python3 tests/audit_detect.py --binary target/release/brainpod --cache "$cache" \
  --runs 5 --output /tmp/detect-default.json   # add --fetch once for a fresh cache
python3 tests/audit_detect.py --binary target/release/brainpod --cache "$cache" \
  --with-env --runs 5 --output /tmp/detect-with-env.json
python3 tests/audit_detect.py --binary target/release/brainpod --cache "$cache" \
  --with-env --with-source --runs 5 --output /tmp/detect-with-source.json
```

`environmentChecks` in the pinned corpus run in both opt-in modes. They sample reviewed names/sources/formats/project ownership, unresolved references, platform semantic fields and Dockerfile `ARG` in mixed repositories. Code-reference checks are kept separately as `sourceChecks` and run only with `--with-source`. The audit runner resolves verified cache directory aliases before passing an explicit scan root. Synthetic Rust tests cover malformed input, secret and default redaction, no evaluation, quoting/multiline declarations, duplicate/scoped names, symlinks, read/depth/parser limits, every platform format's semantic mapping, `ARG`, every code-reference pattern family, generic-name gating, identifier boundaries, exclusions, the occurrence cap and serial/parallel determinism.

## Local results (2026-11-10)

Validated on this branch: 231 Rust tests passed (3 ignored benchmarks not run), 13 Python audit-runner tests passed, formatting passed and the release build succeeded. `cargo clippy --all-targets -- -D warnings` with clippy 0.1.98 reports four errors in unchanged code: `clippy::question_mark` in `src/detect/deno.rs` and `clippy::result_large_err` in generated tunnel protobuf code. The base branch reports the same four with this toolchain. With only those two lints allowed, clippy is clean. Native OS CI and the Brainpod starter branches were not rerun.

- **56/56 pinned repositories** (51 existing plus five added for platform manifests), five deterministic runs per mode:
  - No flags: **300/300** assertions.
  - `--with-env`: **348/348**, including **48/48** declaration checks (22 new).
  - `--with-env --with-source`: **353/353**, adding the **5/5** `sourceChecks`.
- All 56 no-flag JSON reports are byte-identical to the base binary at `27f09ed`, and the `projects` arrays in both opt-in modes equal the no-flag arrays.
- Reads and latency (sum over the 56 repositories; median and mean of per-repository five-run warm medians, timed under concurrent host load including CLI startup and serialization):

| Mode | Files read | Bytes read | Median ms | Mean ms |
|---|---:|---:|---:|---:|
| No flags | 5,239 | 7,344,517 | 15.66 | 33.13 |
| `--with-env` on the base branch (#21) | 6,756 | 14,383,727 | 17.49 | 35.90 |
| `--with-env` | 6,822 | 15,090,783 | 18.04 | 36.39 |
| `--with-env --with-source` | 277,283 | 1,769,906,284 | 78.29 | 115.13 |

  `--with-env` reads 66 more files than #21: the platform manifests.
- Records with `--with-env`: 7,494 Actions, 3,177 dotenv, 2,545 Compose, 1,127 Dockerfile (`ENV` plus `ARG`), 22 Kubernetes, 70 Render, 55 `app-json`, 15 Copilot, 7 Vercel, 5 Netlify, 2 DigitalOcean and 1 App Engine. `--with-source` adds 818 code references in 24 repositories. These counts include examples, duplicate names, unresolved references and separate scopes; they are **not** distinct-variable counts or precision/recall measurements. Fly.io, `heroku.yml` and Netlify template variables have synthetic coverage only; no pinned repository declares them.
- New diagnostics compared with #21's `--with-env`:
  - `--with-env`: two unsupported `ARG` names, both in Rails ERB `Dockerfile.tt` templates.
  - `--with-source`: those two plus 24 source files over 1 MiB (generated bundles and fixtures).
  - The `app.json` that #21's YAML path rejected for a duplicate key now parses as JSON.

Raw local results are in `/tmp/bp-envplat/{detect-default,detect-with-env,detect-with-source,compare}.json`, and the comparison script is `/tmp/bp-envplat/compare.py`. These cache-dependent artifacts are not committed.
