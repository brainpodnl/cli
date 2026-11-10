use super::environment::{Declaration, Format, Scope, Semantics, parse};
use super::tests::fixture;
use super::{Report, render, scan, scan_options};

fn with_env(files: &[(&str, &str)]) -> Report {
    let dir = fixture(files);
    scan_options(dir.path(), None, true).unwrap()
}

fn declarations(report: &Report) -> &[Declaration] {
    report.environment.as_deref().unwrap()
}

fn find<'a>(report: &'a Report, source: &str, name: &str) -> &'a Declaration {
    declarations(report)
        .iter()
        .find(|d| d.source == source && d.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("{source}: {name} missing: {report:#?}"))
}

fn names<'a>(report: &'a Report, source: &str) -> Vec<&'a str> {
    declarations(report)
        .iter()
        .filter(|d| d.source == source)
        .filter_map(|d| d.name.as_deref())
        .collect()
}

fn adapter(format: Format, source: &str, text: &str) -> (Vec<Declaration>, Vec<super::Diagnostic>) {
    let mut diagnostics = Vec::new();
    let declarations = parse(format, source, text, &mut diagnostics);
    (declarations, diagnostics)
}

fn assert_redacted(report: &Report) {
    let json = serde_json::to_string(report).unwrap();
    let text = render(&serde_json::to_value(report).unwrap()).join("\n");
    for output in [json, text] {
        assert!(!output.contains("DO_NOT_DISCLOSE"), "{output}");
    }
}

const NONE: Semantics = Semantics {
    required: None,
    secret: None,
    generated: None,
    has_default: None,
    scope: None,
};

#[test]
fn app_json_and_scalingo_map_required_generated_and_default_presence() {
    let app = r#"{
  "name": "example",
  "stack": "heroku-22",
  "stack": "heroku-24",
  "env": {
    "PLAIN": "DO_NOT_DISCLOSE",
    "TOKEN": {"description": "API token", "required": true},
    "OPTIONAL": {"value": "DO_NOT_DISCLOSE", "required": false},
    "SECRET_KEY_BASE": {"generator": "secret"},
    "EMPTY_DEFAULT": {"value": ""}
  },
  "environments": {"test": {"env": {"PLAIN": "DO_NOT_DISCLOSE"}}},
  "addons": ["heroku-postgresql"]
}"#;
    let scalingo =
        r#"{"env": {"ADMIN_URL": {"generator": "url", "template": "%URL%/DO_NOT_DISCLOSE"}}}"#;
    let report = with_env(&[
        ("app.json", app),
        ("scalingo.json", scalingo),
        ("mobile/app.json", r#"{"expo": {"name": "not-a-manifest"}}"#),
    ]);
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    assert_eq!(
        names(&report, "app.json"),
        [
            "EMPTY_DEFAULT",
            "OPTIONAL",
            "PLAIN",
            "SECRET_KEY_BASE",
            "TOKEN",
            "PLAIN"
        ]
    );
    assert!(names(&report, "mobile/app.json").is_empty());
    let semantics = |source, name| find(&report, source, name).semantics;
    assert_eq!(
        semantics("app.json", "PLAIN"),
        Semantics {
            has_default: Some(true),
            ..NONE
        }
    );
    assert_eq!(
        semantics("app.json", "TOKEN"),
        Semantics {
            required: Some(true),
            has_default: Some(false),
            ..NONE
        }
    );
    assert_eq!(
        semantics("app.json", "OPTIONAL"),
        Semantics {
            required: Some(false),
            has_default: Some(true),
            ..NONE
        }
    );
    assert_eq!(
        semantics("app.json", "SECRET_KEY_BASE"),
        Semantics {
            secret: Some(true),
            generated: Some(true),
            ..NONE
        }
    );
    assert_eq!(
        semantics("scalingo.json", "ADMIN_URL"),
        Semantics {
            generated: Some(true),
            ..NONE
        }
    );
    let test = declarations(&report)
        .iter()
        .filter(|d| d.name.as_deref() == Some("PLAIN"))
        .map(|d| d.location.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        test,
        [
            "document:0/env/PLAIN",
            "document:0/environments/test/env/PLAIN"
        ]
    );
    assert!(
        declarations(&report)
            .iter()
            .all(|d| d.format == Format::AppJson)
    );
    assert_redacted(&report);
}

#[test]
fn render_blueprint_maps_references_generated_prompted_groups_and_projects() {
    let yaml = r#"
services:
  - type: web
    name: api
    envVars:
      - key: API_BASE_URL
        value: DO_NOT_DISCLOSE
      - key: APP_SECRET
        generateValue: true
      - key: STRIPE_KEY
        sync: false
      - key: DATABASE_URL
        fromDatabase: {name: elephant, property: connectionString}
      - key: REDIS_URL
        fromService: {type: keyvalue, name: lightning, property: connectionString}
      - key: WORKER_HOST
        fromService: {type: pserv, name: worker, property: hostport}
      - key: MINIO_PASSWORD
        fromService: {type: pserv, name: minio, envVarKey: MINIO_ROOT_PASSWORD}
      - fromGroup: shared-settings
databases:
  - name: elephant
envVarGroups:
  - name: shared-settings
    envVars:
      - key: CONCURRENCY
        value: 2
      - key: IGNORED
        sync: false
projects:
  - name: app
    environments:
      - name: production
        services:
          - name: web
            envVars: [{key: PROJECT_SCOPED, value: DO_NOT_DISCLOSE}]
"#;
    let report = with_env(&[("render.yaml", yaml)]);
    assert_eq!(
        names(&report, "render.yaml"),
        [
            "API_BASE_URL",
            "APP_SECRET",
            "STRIPE_KEY",
            "DATABASE_URL",
            "REDIS_URL",
            "WORKER_HOST",
            "MINIO_PASSWORD",
            "CONCURRENCY",
            "PROJECT_SCOPED"
        ]
    );
    assert_eq!(report.diagnostics.len(), 1, "{report:#?}");
    assert!(report.diagnostics[0].message.contains("sync: false"));
    let get = |name| find(&report, "render.yaml", name);
    assert_eq!(
        get("API_BASE_URL").semantics,
        Semantics {
            has_default: Some(true),
            ..NONE
        }
    );
    assert_eq!(
        get("APP_SECRET").semantics,
        Semantics {
            secret: Some(true),
            generated: Some(true),
            ..NONE
        }
    );
    assert_eq!(
        get("STRIPE_KEY").semantics,
        Semantics {
            secret: Some(true),
            has_default: Some(false),
            ..NONE
        }
    );
    for (name, kind, target, key) in [
        ("DATABASE_URL", "database", "elephant", "connectionString"),
        ("REDIS_URL", "key-value", "lightning", "connectionString"),
        ("WORKER_HOST", "service", "worker", "hostport"),
        (
            "MINIO_PASSWORD",
            "service-env",
            "minio",
            "MINIO_ROOT_PASSWORD",
        ),
    ] {
        let declaration = get(name);
        assert_eq!(declaration.state, "unresolved");
        assert_eq!(declaration.semantics, NONE);
        let reference = declaration.reference.as_ref().unwrap();
        assert_eq!(
            (
                reference.kind.as_str(),
                reference.target.as_deref(),
                reference.key.as_deref()
            ),
            (kind, Some(target), Some(key))
        );
    }
    let group = declarations(&report)
        .iter()
        .find(|d| d.name.is_none())
        .unwrap();
    assert_eq!(group.location, "document:0/services/0/envVars/7");
    let reference = group.reference.as_ref().unwrap();
    assert_eq!(reference.kind, "env-group");
    assert_eq!(reference.target.as_deref(), Some("shared-settings"));
    assert_eq!(
        get("PROJECT_SCOPED").location,
        "document:0/projects/0/environments/0/services/0/envVars/0"
    );
    assert_redacted(&report);
}

#[test]
fn toml_and_json_platform_files_record_scopes_templates_and_vercel_secrets() {
    let report = with_env(&[
        (
            "fly.toml",
            "app = 'example'\n[env]\n  LOG_LEVEL = 'DO_NOT_DISCLOSE'\n  PORT = 8080\n[build.args]\n  NOT_ENV = 'DO_NOT_DISCLOSE'\n",
        ),
        ("fly.staging.toml", "[env]\nSTAGING = 'DO_NOT_DISCLOSE'\n"),
        (
            "netlify.toml",
            r#"
[build]
  command = "make"
  environment = { NODE_VERSION = "DO_NOT_DISCLOSE" }
[context.production.environment]
  API_URL = "DO_NOT_DISCLOSE"
[context.deploy-preview]
  environment = { API_URL = "DO_NOT_DISCLOSE" }
[template.environment]
  SECRET_TOKEN = "DO_NOT_DISCLOSE label"
[[redirects]]
  from = "/a"
  to = "/b"
  [redirects.headers]
    X-Api-Key = "DO_NOT_DISCLOSE"
"#,
        ),
        (
            "web/vercel.json",
            r#"{"env": {"MY_KEY": "DO_NOT_DISCLOSE", "SECRET": "@my-secret-name"}, "build": {"env": {"BUILD_KEY": "DO_NOT_DISCLOSE"}}}"#,
        ),
        ("site/vercel.toml", "[env]\nTOML_KEY = 'DO_NOT_DISCLOSE'\n"),
    ]);
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    assert_eq!(names(&report, "fly.toml"), ["LOG_LEVEL", "PORT"]);
    assert_eq!(names(&report, "fly.staging.toml"), ["STAGING"]);
    assert_eq!(
        find(&report, "fly.toml", "PORT").semantics,
        Semantics {
            has_default: Some(true),
            scope: Some(Scope::Runtime),
            ..NONE
        }
    );
    assert_eq!(
        names(&report, "netlify.toml"),
        ["NODE_VERSION", "API_URL", "API_URL", "SECRET_TOKEN"]
    );
    let netlify = declarations(&report)
        .iter()
        .filter(|d| d.source == "netlify.toml")
        .map(|d| (d.location.as_str(), d.semantics))
        .collect::<Vec<_>>();
    let build = Semantics {
        has_default: Some(true),
        scope: Some(Scope::Build),
        ..NONE
    };
    assert_eq!(
        netlify,
        [
            ("document:0/build/environment/NODE_VERSION", build),
            (
                "document:0/context/deploy-preview/environment/API_URL",
                build
            ),
            ("document:0/context/production/environment/API_URL", build),
            (
                "document:0/template/environment/SECRET_TOKEN",
                Semantics {
                    required: Some(true),
                    has_default: Some(false),
                    ..NONE
                }
            ),
        ]
    );
    let secret = find(&report, "web/vercel.json", "SECRET");
    assert_eq!(secret.state, "unresolved");
    assert_eq!(
        secret.semantics,
        Semantics {
            secret: Some(true),
            scope: Some(Scope::Runtime),
            ..NONE
        }
    );
    let reference = secret.reference.as_ref().unwrap();
    assert_eq!(reference.kind, "platform-secret");
    assert_eq!(reference.target.as_deref(), Some("my-secret-name"));
    assert_eq!(
        find(&report, "web/vercel.json", "BUILD_KEY")
            .semantics
            .scope,
        Some(Scope::Build)
    );
    assert_eq!(names(&report, "site/vercel.toml"), ["TOML_KEY"]);
    assert_redacted(&report);
}

#[test]
fn digitalocean_specs_map_type_scope_and_bindable_references() {
    let spec = r#"
name: example
envs:
  - key: APP_WIDE
    value: DO_NOT_DISCLOSE
    scope: RUN_AND_BUILD_TIME
services:
  - name: api
    envs:
      - key: DATABASE_URL
        value: ${elephant.DATABASE_URL}
        scope: RUN_TIME
      - key: POOL_URL
        value: ${elephant.pool.DATABASE_URL}
      - key: PUBLIC_URL
        value: ${APP_URL}
      - key: SELF_URL
        value: ${_self.PRIVATE_URL}
      - key: PARTIAL
        value: postgres://DO_NOT_DISCLOSE@${elephant.HOSTNAME}/db
      - key: API_TOKEN
        value: EV[1:DO_NOT_DISCLOSE]
        type: SECRET
      - key: OPAQUE_SECRET
        value: ${DO_NOT_DISCLOSE.DO_NOT_DISCLOSE}
        type: SECRET
static_sites:
  - name: web
    envs:
      - key: BUILD_ONLY
        value: DO_NOT_DISCLOSE
        scope: BUILD_TIME
        type: GENERAL
databases:
  - name: elephant
    engine: PG
"#;
    let template = r#"
spec:
  name: sample
  services:
    - name: web
      envs:
        - key: EMAIL
          scope: RUN_TIME
        - key: DB_PASSWORD
          value: DO_NOT_DISCLOSE
          type: SECRET
"#;
    let report = with_env(&[
        (".do/app.yaml", spec),
        (".do/deploy.template.yaml", template),
        ("other/deploy.template.yaml", template),
    ]);
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    assert!(names(&report, "other/deploy.template.yaml").is_empty());
    let get = |name| find(&report, ".do/app.yaml", name);
    assert_eq!(
        get("APP_WIDE").semantics,
        Semantics {
            has_default: Some(true),
            scope: Some(Scope::BuildAndRuntime),
            ..NONE
        }
    );
    for (name, kind, target, key) in [
        (
            "DATABASE_URL",
            "database",
            Some("elephant"),
            Some("DATABASE_URL"),
        ),
        (
            "POOL_URL",
            "database",
            Some("elephant"),
            Some("DATABASE_URL"),
        ),
        ("PUBLIC_URL", "app", None, Some("APP_URL")),
        ("SELF_URL", "service", Some("_self"), Some("PRIVATE_URL")),
        ("PARTIAL", "interpolation", None, None),
    ] {
        let declaration = get(name);
        assert_eq!(declaration.state, "unresolved", "{name}");
        assert_eq!(declaration.semantics.has_default, None, "{name}");
        let reference = declaration.reference.as_ref().unwrap();
        assert_eq!(
            (
                reference.kind.as_str(),
                reference.target.as_deref(),
                reference.key.as_deref()
            ),
            (kind, target, key),
            "{name}"
        );
    }
    assert_eq!(get("DATABASE_URL").semantics.scope, Some(Scope::Runtime));
    for name in ["API_TOKEN", "OPAQUE_SECRET"] {
        assert_eq!(get(name).state, "declared");
        assert_eq!(
            get(name).semantics,
            Semantics {
                secret: Some(true),
                has_default: Some(true),
                ..NONE
            }
        );
    }
    assert_eq!(
        get("BUILD_ONLY").semantics,
        Semantics {
            secret: Some(false),
            has_default: Some(true),
            scope: Some(Scope::Build),
            ..NONE
        }
    );
    let template = |name| find(&report, ".do/deploy.template.yaml", name);
    assert_eq!(
        template("EMAIL").location,
        "document:0/spec/services/0/envs/0"
    );
    assert_eq!(
        template("EMAIL").semantics,
        Semantics {
            has_default: Some(false),
            scope: Some(Scope::Runtime),
            ..NONE
        }
    );
    assert_eq!(template("DB_PASSWORD").semantics.secret, Some(true));
    assert_redacted(&report);
}

#[test]
fn app_engine_requires_runtime_and_heroku_yml_separates_build_config() {
    let report = with_env(&[
        (
            "app.yaml",
            "runtime: python312\nenv_variables:\n  BUCKET_NAME: DO_NOT_DISCLOSE\nbuild_env_variables:\n  GOOGLE_NODE_RUN_SCRIPTS: ''\n",
        ),
        (
            "config/app.yaml",
            "env_variables: {NOT_APP_ENGINE: DO_NOT_DISCLOSE}",
        ),
        (
            "heroku.yml",
            "setup:\n  addons: [{plan: heroku-postgresql, as: DATABASE}]\n  config: {S3_BUCKET: DO_NOT_DISCLOSE}\nbuild:\n  docker: {web: Dockerfile}\n  config: {RAILS_ENV: DO_NOT_DISCLOSE}\nrun:\n  web: echo NOT_ENV=DO_NOT_DISCLOSE\n",
        ),
    ]);
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    assert_eq!(
        names(&report, "app.yaml"),
        ["BUCKET_NAME", "GOOGLE_NODE_RUN_SCRIPTS"]
    );
    assert!(names(&report, "config/app.yaml").is_empty());
    assert_eq!(
        find(&report, "app.yaml", "BUCKET_NAME").semantics.scope,
        Some(Scope::Runtime)
    );
    assert_eq!(
        find(&report, "app.yaml", "GOOGLE_NODE_RUN_SCRIPTS").semantics,
        Semantics {
            has_default: Some(true),
            scope: Some(Scope::Build),
            ..NONE
        }
    );
    assert_eq!(names(&report, "heroku.yml"), ["S3_BUCKET", "RAILS_ENV"]);
    assert_eq!(
        find(&report, "heroku.yml", "RAILS_ENV").semantics.scope,
        Some(Scope::Build)
    );
    assert!(declarations(&report).iter().all(|d| d.project.is_none()));
    assert_redacted(&report);
}

#[test]
fn copilot_workloads_map_variables_secrets_overrides_and_env_files() {
    let manifest = r#"
name: api
type: Load Balanced Web Service
variables:
  LOG_LEVEL: DO_NOT_DISCLOSE
  QUEUE_URL: {from_cfn: queue-export}
  UNSET:
secrets:
  GITHUB_TOKEN: GH_TOKEN_SECRET
  NESTED_PATH: /copilot/DO_NOT_DISCLOSE/secret
  DB_SECRET: {secretsmanager: mysql}
env_file: ./api.env
sidecars:
  proxy:
    variables: {PROXY_MODE: DO_NOT_DISCLOSE}
environments:
  prod:
    variables: {LOG_LEVEL: DO_NOT_DISCLOSE}
"#;
    let report = with_env(&[
        ("copilot/api/manifest.yml", manifest),
        (
            "copilot/environments/prod/manifest.yml",
            "name: prod\ntype: Environment\nvariables: {NOT_A_WORKLOAD: DO_NOT_DISCLOSE}\n",
        ),
        ("services/manifest.yml", manifest),
    ]);
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    let source = "copilot/api/manifest.yml";
    assert_eq!(
        names(&report, source),
        [
            "LOG_LEVEL",
            "QUEUE_URL",
            "UNSET",
            "DB_SECRET",
            "GITHUB_TOKEN",
            "NESTED_PATH",
            "PROXY_MODE",
            "LOG_LEVEL"
        ]
    );
    assert!(names(&report, "copilot/environments/prod/manifest.yml").is_empty());
    assert!(names(&report, "services/manifest.yml").is_empty());
    let runtime_secret = Semantics {
        secret: Some(true),
        scope: Some(Scope::Runtime),
        ..NONE
    };
    assert_eq!(
        find(&report, source, "UNSET").semantics,
        Semantics {
            secret: Some(false),
            has_default: Some(false),
            scope: Some(Scope::Runtime),
            ..NONE
        }
    );
    for (name, kind, target) in [
        ("GITHUB_TOKEN", "ssm-parameter", Some("GH_TOKEN_SECRET")),
        ("NESTED_PATH", "ssm-parameter", None),
        ("DB_SECRET", "secrets-manager", Some("mysql")),
    ] {
        let declaration = find(&report, source, name);
        assert_eq!(declaration.semantics, runtime_secret);
        let reference = declaration.reference.as_ref().unwrap();
        assert_eq!(
            (reference.kind.as_str(), reference.target.as_deref()),
            (kind, target)
        );
    }
    assert_eq!(
        find(&report, source, "QUEUE_URL")
            .reference
            .as_ref()
            .unwrap()
            .kind,
        "cloudformation-export"
    );
    assert_eq!(
        find(&report, source, "PROXY_MODE").semantics,
        Semantics {
            secret: Some(false),
            has_default: Some(true),
            scope: Some(Scope::Runtime),
            ..NONE
        }
    );
    let file = declarations(&report)
        .iter()
        .find(|d| d.name.is_none())
        .unwrap();
    assert_eq!(
        file.reference.as_ref().unwrap().target.as_deref(),
        Some("./api.env")
    );
    assert_redacted(&report);
}

#[test]
fn malformed_platform_inputs_are_redacted_diagnostics() {
    for (format, source, text) in [
        (
            Format::AppJson,
            "app.json",
            r#"{"env": {"KEY": ["DO_NOT_DISCLOSE"]}}"#,
        ),
        (Format::AppJson, "app.json", r#"{"env": "DO_NOT_DISCLOSE"}"#),
        (
            Format::AppJson,
            "app.json",
            r#"{"env": {"KEY": "DO_NOT_DISCLOSE""#,
        ),
        (
            Format::Render,
            "render.yaml",
            "services: [{envVars: [{value: DO_NOT_DISCLOSE}]}]",
        ),
        (
            Format::Render,
            "render.yaml",
            "services: [{envVars: {KEY: DO_NOT_DISCLOSE}}]",
        ),
        (Format::Fly, "fly.toml", "[env]\nKEY = 'DO_NOT_DISCLOSE"),
        (Format::Fly, "fly.toml", "[env]\nKEY = ['DO_NOT_DISCLOSE']"),
        (
            Format::Netlify,
            "netlify.toml",
            "[build]\nenvironment = 'DO_NOT_DISCLOSE'",
        ),
        (
            Format::Vercel,
            "vercel.json",
            r#"{"env": {"DO NOT DISCLOSE": "x"}}"#,
        ),
        (
            Format::Digitalocean,
            ".do/app.yaml",
            "envs: [{value: DO_NOT_DISCLOSE}]",
        ),
        (
            Format::AppEngine,
            "app.yaml",
            "runtime: go\nenv_variables: [DO_NOT_DISCLOSE]",
        ),
        (
            Format::Copilot,
            "copilot/a/manifest.yml",
            "type: Worker Service\nsecrets: {KEY: [DO_NOT_DISCLOSE]}",
        ),
        (
            Format::HerokuYml,
            "heroku.yml",
            "build: {config: {KEY: {nested: DO_NOT_DISCLOSE}}}",
        ),
    ] {
        let (declarations, diagnostics) = adapter(format, source, text);
        assert!(
            !diagnostics.is_empty(),
            "{format:?} {text}: {declarations:?}"
        );
        let output = format!("{declarations:?}{diagnostics:?}");
        assert!(!output.contains("DO_NOT_DISCLOSE"), "{output}");
    }
    let nested = format!("[env]\nKEY = {}1{}", "[".repeat(5000), "]".repeat(5000));
    let (declarations, diagnostics) = adapter(Format::Fly, "fly.toml", &nested);
    assert!(declarations.is_empty());
    assert_eq!(diagnostics.len(), 1);
}

#[test]
fn platform_routing_is_opt_in_deterministic_and_keeps_existing_formats() {
    let files = [
        ("package.json", "{}"),
        (".env", "EXISTING=DO_NOT_DISCLOSE"),
        ("fly.toml", "[env]\nFLY=DO_NOT_DISCLOSE\n"),
        (
            "render.yaml",
            "services: [{envVars: [{key: RENDER, sync: false}]}]",
        ),
        ("app.json", r#"{"env": {"HEROKU": {"required": false}}}"#),
        (
            "k8s/app.yaml",
            "apiVersion: v1\nkind: Pod\nruntime: not-app-engine\nspec: {containers: [{env: [{name: KUBE, value: DO_NOT_DISCLOSE}]}]}",
        ),
    ];
    let dir = fixture(&files);
    let default = scan(dir.path(), None).unwrap();
    assert_eq!(default.stats.files_read, 1);
    assert!(
        serde_json::to_value(&default)
            .unwrap()
            .get("environment")
            .is_none()
    );
    let report = scan_options(dir.path(), None, true).unwrap();
    assert_eq!(scan_options(dir.path(), None, true).unwrap(), report);
    assert_eq!(report.stats.files_read, 6);
    assert_eq!(default.projects, report.projects);
    let kube = declarations(&report)
        .iter()
        .find(|d| d.name.as_deref() == Some("KUBE"))
        .unwrap();
    assert_eq!(kube.format, Format::Kubernetes);
    let json = serde_json::to_value(&report).unwrap();
    let existing = json["environment"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "EXISTING")
        .unwrap();
    let mut keys = existing
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        ["format", "location", "name", "project", "source", "state"]
    );
    let render_record = json["environment"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "RENDER")
        .unwrap();
    assert_eq!(render_record["secret"], true);
    assert_eq!(render_record["hasDefault"], false);
    let text = render(&json);
    let line = text
        .iter()
        .position(|line| line.starts_with("env RENDER\t"))
        .unwrap();
    assert_eq!(text[line + 1], "  secret: true, hasDefault: false");
    let existing = text
        .iter()
        .position(|line| line.starts_with("env EXISTING\t"))
        .unwrap();
    assert!(text[existing + 1].starts_with("env "));
    assert_redacted(&report);
}
