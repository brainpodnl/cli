use std::fs;

use super::environment::{Declaration, Format, parse};
use super::tests::fixture;
use super::{Report, render, scan, scan_options};

fn with_env(files: &[(&str, &str)]) -> Report {
    let dir = fixture(files);
    scan_options(dir.path(), None, true).unwrap()
}

fn names(report: &Report) -> Vec<&str> {
    report
        .environment
        .as_ref()
        .unwrap()
        .iter()
        .filter_map(|d| d.name.as_deref())
        .collect()
}

fn adapter(format: Format, text: &str) -> (Vec<Declaration>, Vec<super::Diagnostic>) {
    let mut diagnostics = Vec::new();
    let declarations = parse(format, "metadata", text, &mut diagnostics);
    (declarations, diagnostics)
}

#[test]
fn dotenv_names_quotes_comments_multiline_duplicates_and_templates() {
    let report = with_env(&[
        (
            ".env.example",
            "\u{feff}# comment\n export ONE = secret # comment\nTWO='a#b'\nTHREE=\"a\\\"b\"\nMULTI=\"private\nFAKE=not-a-key\nend\"\nBACK=`text\nALSO_FAKE=no\nend`\nEMPTY=\nONE=second\n",
        ),
        ("config/test.env.template", "TEMPLATE=hidden\n"),
        (".env.production.local", "PRODUCTION=hidden\n"),
        (".env.enc", "ENCRYPTED=no\n"),
        (".env.vault", "VAULT=no\n"),
        ("source.ts", "process.env.SOURCE_INFERENCE_IS_FORBIDDEN"),
    ]);
    assert_eq!(
        names(&report),
        [
            "ONE",
            "TWO",
            "THREE",
            "MULTI",
            "BACK",
            "EMPTY",
            "ONE",
            "PRODUCTION",
            "TEMPLATE"
        ]
    );
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    assert_eq!(report.stats.files_read, 3);
    assert!(
        report
            .environment
            .as_ref()
            .unwrap()
            .iter()
            .all(|d| d.project.as_deref() == Some("."))
    );
}

#[test]
fn malformed_inputs_never_echo_secret_values() {
    for (format, text) in [
        (
            Format::Dotenv,
            "BAD NAME=DO_NOT_DISCLOSE\nBROKEN='DO_NOT_DISCLOSE\nFAKE=DO_NOT_DISCLOSE",
        ),
        (Format::Dockerfile, "ENV KEY=\"DO_NOT_DISCLOSE\nENV KEY\n"),
        (
            Format::Compose,
            "services: {web: {environment: {KEY: [DO_NOT_DISCLOSE]}}}",
        ),
        (
            Format::Compose,
            "services: {web: {environment: {KEY: DO_NOT_DISCLOSE, KEY: DO_NOT_DISCLOSE}}}",
        ),
        (Format::Kubernetes, "apiVersion: DO_NOT_DISCLOSE\nkind: ["),
        (
            Format::GithubActions,
            "env: {KEY: !unknown DO_NOT_DISCLOSE}",
        ),
    ] {
        let (declarations, diagnostics) = adapter(format, text);
        assert!(!diagnostics.is_empty(), "{format:?}: {declarations:?}");
        let output = format!("{declarations:?}{diagnostics:?}");
        assert!(!output.contains("DO_NOT_DISCLOSE"), "{output}");
    }
}

#[test]
fn compose_maps_lists_external_values_and_unresolved_files() {
    let report = with_env(&[(
        "compose.override.yaml",
        r#"
services:
  api:
    environment:
      TOKEN: secret
      EMPTY: ''
      INHERITED:
    env_file:
      - ./private.env
      - path: '${PATH:-DO_NOT_DISCLOSE}'
        required: false
  worker:
    environment:
      - TOKEN=other-secret
      - INHERITED
      - MULTILINE=first
      - BAD NAME=hidden
    labels:
      DO_NOT_INFER: hidden
"#,
    )]);
    assert_eq!(
        names(&report),
        [
            "EMPTY",
            "INHERITED",
            "TOKEN",
            "TOKEN",
            "INHERITED",
            "MULTILINE"
        ]
    );
    let env = report.environment.as_ref().unwrap();
    assert_eq!(env.iter().filter(|d| d.state == "external").count(), 2);
    let references = env
        .iter()
        .filter_map(|d| d.reference.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 2);
    assert_eq!(references[0].target.as_deref(), Some("./private.env"));
    assert_eq!(references[1].target, None);
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("DO_NOT_DISCLOSE")
    );
}

#[test]
fn docker_env_is_not_arg_or_run_and_records_all_stages_not_effective_image() {
    let (env, diagnostics) = adapter(
        Format::Dockerfile,
        "FROM alpine AS base\nARG BUILD_ONLY=hidden\nRUN echo ENV FAKE=hidden\nENV A=one B=\"two words\" \\\n C=three\\ words\nENV OLD legacy value D=not-a-declaration\nFROM base\nenv A=overridden\n",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        env.iter()
            .filter_map(|d| d.name.as_deref())
            .collect::<Vec<_>>(),
        ["A", "B", "C", "OLD", "A"]
    );
    assert!(env[0].location.starts_with("stage:1/"));
    assert!(env[4].location.starts_with("stage:2/"));
    let (env, diagnostics) = adapter(
        Format::Dockerfile,
        "FROM alpine\nRUN <<EOF\nENV FAKE=secret\nEOF\nENV AFTER=secret\n",
    );
    assert!(env.is_empty());
    assert_eq!(diagnostics.len(), 1);
    let (env, diagnostics) = adapter(
        Format::Dockerfile,
        "# escape=`\nFROM windows\nENV FIRST=hidden `\n SECOND=hidden\n",
    );
    assert_eq!(env.len(), 2);
    assert!(diagnostics.is_empty());
}

#[test]
fn actions_only_explicit_workflow_job_and_step_env_in_workflow_paths() {
    let yaml = r#"
on: push
env:
  TOP: secret
jobs:
  test:
    env: {TOP: other, JOB: secret}
    steps:
      - env:
          STEP: '${{ secrets.DO_NOT_DISCLOSE }}'
        run: echo SOURCE_INFERENCE_FORBIDDEN >> $GITHUB_ENV
      - env: '${{ fromJSON(secrets.DO_NOT_DISCLOSE) }}'
        with: {NOT_ENV: hidden}
"#;
    let report = with_env(&[
        (".github/workflows/test.yml", yaml),
        ("not-a-workflow.yml", yaml),
    ]);
    assert_eq!(names(&report), ["TOP", "JOB", "TOP", "STEP"]);
    assert_eq!(report.environment.as_ref().unwrap().len(), 5);
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    let output = serde_json::to_string(&report).unwrap();
    assert!(!output.contains("DO_NOT_DISCLOSE"));
    assert!(!output.contains("SOURCE_INFERENCE_FORBIDDEN"));
}

#[test]
fn kubernetes_workloads_lists_multidoc_and_references_without_secret_data() {
    let report = with_env(&[(
        "deploy/app.yaml",
        r#"
apiVersion: v1
kind: Secret
metadata: {name: credentials}
stringData: {NEVER_INFER: DO_NOT_DISCLOSE}
---
apiVersion: apps/v1
kind: Deployment
spec:
  template:
    spec:
      initContainers:
        - name: init
          env: [{name: INIT, value: hidden}]
      containers:
        - name: app
          env:
            - {name: TOKEN, valueFrom: {secretKeyRef: {name: credentials, key: password}}}
            - {name: CONFIG, valueFrom: {configMapKeyRef: {name: config, key: data}}}
            - {name: POD_NAME, valueFrom: {fieldRef: {fieldPath: metadata.name}}}
          envFrom:
            - secretRef: {name: credentials}
              prefix: APP_
            - configMapRef: {name: config}
---
apiVersion: v1
kind: List
items:
  - apiVersion: batch/v1
    kind: CronJob
    spec:
      jobTemplate:
        spec:
          template:
            spec:
              containers:
                - env: [{name: CRON, value: hidden}]
"#,
    )]);
    assert_eq!(
        names(&report),
        ["TOKEN", "CONFIG", "POD_NAME", "INIT", "CRON"]
    );
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    let env = report.environment.as_ref().unwrap();
    assert_eq!(env.iter().filter(|d| d.name.is_none()).count(), 2);
    let secret = env
        .iter()
        .find(|d| d.name.as_deref() == Some("TOKEN"))
        .unwrap()
        .reference
        .as_ref()
        .unwrap();
    assert_eq!(secret.target.as_deref(), Some("credentials"));
    assert_eq!(secret.key.as_deref(), Some("password"));
    assert!(env.iter().any(|d| {
        d.reference
            .as_ref()
            .is_some_and(|r| r.prefix.as_deref() == Some("APP_"))
    }));
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("DO_NOT_DISCLOSE")
    );
}

#[test]
fn yaml_anchors_merge_keys_flow_and_block_scalars() {
    let report = with_env(&[(
        "compose.yaml",
        r#"
x-env: &common
  KEY: |
    secret
    FAKE=not-a-declaration
services:
  api:
    environment:
      <<: *common
      EXTRA: >-
        multiline secret
  worker:
    environment: *common
"#,
    )]);
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    assert_eq!(names(&report), ["EXTRA", "KEY", "KEY"]);
}

#[test]
fn nearest_project_owns_declarations_without_workspace_inheritance() {
    let report = with_env(&[
        ("package.json", r#"{"workspaces":["apps/*"]}"#),
        (".env", "ROOT=secret"),
        ("apps/web/package.json", "{}"),
        ("apps/web/.env", "LOCAL=secret"),
        ("apps/web/config/.env", "CONFIG=secret"),
        ("apps/api/deno.json", "{}"),
        ("apps/api/.env.example", "LOCAL=secret"),
        ("shared/.env", "SHARED=secret"),
    ]);
    let owners = report
        .environment
        .as_ref()
        .unwrap()
        .iter()
        .map(|d| (d.name.as_deref().unwrap(), d.project.as_deref().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(
        owners,
        [
            ("ROOT", "."),
            ("LOCAL", "apps/api"),
            ("LOCAL", "apps/web"),
            ("CONFIG", "apps/web"),
            ("SHARED", ".")
        ]
    );
    let report = with_env(&[
        ("unowned/.env", "UNOWNED=secret"),
        ("api/package.json", "{}"),
    ]);
    assert_eq!(report.environment.as_ref().unwrap()[0].project, None);
}

#[test]
fn opt_in_preserves_default_reads_output_and_diagnostics() {
    let dir = fixture(&[
        ("package.json", "{}"),
        (".env", "INVALID SECRET"),
        ("Dockerfile", "ENV MALFORMED"),
        ("deployment.yaml", "[invalid YAML"),
    ]);
    let default = scan(dir.path(), None).unwrap();
    assert_eq!(default.stats.files_read, 1);
    assert!(default.diagnostics.is_empty());
    let json = serde_json::to_value(&default).unwrap();
    assert!(json.get("environment").is_none());
    assert_eq!(render(&json), [".\tjavascript\tnode"]);
    fs::remove_file(dir.path().join(".env")).unwrap();
    fs::remove_file(dir.path().join("Dockerfile")).unwrap();
    fs::remove_file(dir.path().join("deployment.yaml")).unwrap();
    let clean = scan(dir.path(), None).unwrap();
    assert_eq!(default.projects, clean.projects);
    assert_eq!(default.stats.files_read, clean.stats.files_read);
    assert_eq!(default.stats.bytes_read, clean.stats.bytes_read);
}

#[test]
fn deterministic_json_text_and_no_evaluation_or_values() {
    let dir = fixture(&[
        ("package.json", "{}"),
        (
            ".env",
            "NAME=$(touch SHOULD_NOT_EXIST)\nOTHER=${NAME:-DO_NOT_DISCLOSE}\nQUOTED='DO_NOT_DISCLOSE'",
        ),
        (
            ".github/workflows/ci.yml",
            "env: {CI_NAME: '${{ secrets.DO_NOT_DISCLOSE }}'}",
        ),
    ]);
    fs::write(
        dir.path().join(".env"),
        format!(
            "NAME=$(touch {})\nOTHER=${{NAME:-DO_NOT_DISCLOSE}}\nQUOTED='DO_NOT_DISCLOSE'",
            dir.path().join("SHOULD_NOT_EXIST").display()
        ),
    )
    .unwrap();
    let report = scan_options(dir.path(), None, true).unwrap();
    assert_eq!(scan_options(dir.path(), None, true).unwrap(), report);
    let value = serde_json::to_value(&report).unwrap();
    let text = render(&value).join("\n");
    for output in [serde_json::to_string(&report).unwrap(), text] {
        assert!(output.contains("NAME"));
        for forbidden in [
            "DO_NOT_DISCLOSE",
            "touch",
            "SHOULD_NOT_EXIST",
            "${",
            "secrets.",
        ] {
            assert!(!output.contains(forbidden), "{output}");
        }
    }
    assert!(!dir.path().join("SHOULD_NOT_EXIST").exists());
}

#[test]
fn size_and_yaml_depth_alias_limits_are_bounded_and_redacted() {
    let dir = fixture(&[(
        ".env",
        &format!("KEY={}", "s".repeat(super::MAX_FILE_BYTES as usize + 100)),
    )]);
    let report = scan_options(dir.path(), None, true).unwrap();
    assert!(report.environment.as_ref().unwrap().is_empty());
    assert_eq!(report.stats.bytes_read, super::MAX_FILE_BYTES as usize + 1);
    assert_eq!(report.diagnostics.len(), 1);
    let deeply_nested = format!("{}secret{}", "[".repeat(60), "]".repeat(60));
    let (_, diagnostics) = adapter(Format::Kubernetes, &deeply_nested);
    assert_eq!(diagnostics.len(), 1);
    let aliases = format!("x: &x [secret]\ny: [{}]", vec!["*x"; 140].join(","));
    let (_, diagnostics) = adapter(Format::Compose, &aliases);
    assert_eq!(diagnostics.len(), 1);
}

#[cfg(unix)]
#[test]
fn no_symlinks_no_referenced_file_reads_no_root_escape() {
    use std::os::unix::fs::symlink;
    let outside = fixture(&[("private.env", "ESCAPED=DO_NOT_DISCLOSE")]);
    let dir = fixture(&[(
        "compose.yaml",
        "services: {app: {env_file: [../../private.env, /private.env, '${PRIVATE:-DO_NOT_DISCLOSE}']}}",
    )]);
    symlink(outside.path().join("private.env"), dir.path().join(".env")).unwrap();
    symlink(outside.path(), dir.path().join("linked")).unwrap();
    let report = scan_options(dir.path(), None, true).unwrap();
    assert!(names(&report).is_empty());
    assert_eq!(report.stats.files_read, 1);
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("DO_NOT_DISCLOSE")
    );
}

#[test]
fn directory_depth_limit_applies_to_environment_metadata() {
    let dir = fixture(&[(".env", "ROOT=secret")]);
    let deep = (0..130).fold(dir.path().to_path_buf(), |path, _| path.join("d"));
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join(".env"), "TOO_DEEP=secret").unwrap();
    let report = scan_options(dir.path(), None, true).unwrap();
    assert_eq!(names(&report), ["ROOT"]);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.message.contains("depth limit"))
    );
}

#[test]
fn cli_flag_is_opt_in_and_accepts_paths_with_json() {
    use clap::Parser;
    for (args, expected) in [
        (vec!["brainpod", "detect"], false),
        (
            vec!["brainpod", "detect", "some/project", "--with-env", "--json"],
            true,
        ),
    ] {
        let opts = crate::Opts::try_parse_from(args).unwrap();
        let crate::cmd::Command::Detect(args) = opts.command else {
            panic!("not detect");
        };
        assert_eq!(args.with_env, expected);
    }
}

#[test]
fn declarative_environment_catalog_can_register_another_path_without_code() {
    let dir = fixture(&[
        ("config/application.vars", "EXPLICIT=secret"),
        ("elsewhere.vars", "NOT_SELECTED=secret"),
    ]);
    let extra = r#"
[[catalog.environment]]
format = "dotenv"
files = ["*.vars"]
paths = ["config/**"]
"#;
    let report = scan_options(dir.path(), Some(extra), true).unwrap();
    assert_eq!(names(&report), ["EXPLICIT"]);
    assert_eq!(report.stats.files_read, 1);
    assert_eq!(
        scan_options(dir.path(), Some(extra), false)
            .unwrap()
            .stats
            .files_read,
        0
    );
    for invalid in [
        extra.replace("*.vars", "bad/*.vars"),
        extra.replace("config/**", "../**"),
    ] {
        assert!(scan_options(dir.path(), Some(&invalid), true).is_err());
    }
}

#[test]
fn kubernetes_json_only_in_declared_metadata_paths_and_known_api_groups() {
    let pod = r#"{"apiVersion":"v1","kind":"Pod","spec":{"ephemeralContainers":[{"env":[{"name":"DEBUG","value":"secret"}]}]}}"#;
    let report = with_env(&[
        ("k8s/pod.json", pod),
        ("app.k8s.json", pod),
        ("data.json", pod),
        (
            "manifests/not-kubernetes.yaml",
            "apiVersion: example.com/v1\nkind: Deployment\nspec: {template: {spec: {containers: [{env: [{name: NOT_ENV}]}]}}}",
        ),
    ]);
    assert_eq!(names(&report), ["DEBUG", "DEBUG"]);
    assert_eq!(report.stats.files_read, 3);
    assert!(report.diagnostics.is_empty());
}

#[test]
fn docker_continuations_skip_comments_without_inferring_nested_instructions() {
    let (declarations, diagnostics) = adapter(
        Format::Dockerfile,
        "FROM alpine\nENV ONE=secret \\\n# comment\n\n TWO=secret\nRUN echo \\\n ENV FAKE=secret\n",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        declarations
            .iter()
            .filter_map(|d| d.name.as_deref())
            .collect::<Vec<_>>(),
        ["ONE", "TWO"]
    );
}

#[cfg(unix)]
#[test]
fn rejects_explicit_symlink_scan_root() {
    let dir = fixture(&[("project/.env", "KEY=secret")]);
    std::os::unix::fs::symlink(dir.path().join("project"), dir.path().join("link")).unwrap();
    assert!(scan_options(&dir.path().join("link"), None, true).is_err());
}

#[test]
fn parallel_environment_inventory_is_identical_to_serial_and_retains_no_values() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..128 {
        let path = dir.path().join(format!("services/s{index}/config/deep"));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join(".env"), "KEY=DO_NOT_DISCLOSE").unwrap();
    }
    let rules = super::Rules::builtin().unwrap();
    let serial = super::inventory(dir.path(), rules, 1, true).unwrap();
    let parallel = super::inventory(dir.path(), rules, 4, true).unwrap();
    assert_eq!(serial.stats, parallel.stats);
    assert_eq!(serial.nodes.len(), parallel.nodes.len());
    for (a, b) in serial.nodes.iter().zip(&parallel.nodes) {
        assert_eq!(a.environment, b.environment);
        assert!(a.contents.is_empty());
        assert!(!format!("{:?}", a.environment).contains("DO_NOT_DISCLOSE"));
    }
    assert_eq!(serial.stats.files_read, 128);
}

#[test]
fn yaml_include_is_rejected_without_reading_its_target() {
    let report = with_env(&[
        (
            ".github/workflows/test.yml",
            "env: !include ../../private.data",
        ),
        ("private.data", "DO_NOT_DISCLOSE=DO_NOT_DISCLOSE"),
    ]);
    assert!(names(&report).is_empty());
    assert_eq!(report.stats.files_read, 1);
    assert_eq!(report.diagnostics.len(), 1);
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("DO_NOT_DISCLOSE")
    );
}
