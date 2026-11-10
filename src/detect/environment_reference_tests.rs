use std::fs;

use super::environment::{Declaration, Format, Origin, Scope, parse};
use super::tests::fixture;
use super::{Report, render, scan, scan_options};

fn with_env(files: &[(&str, &str)]) -> Report {
    let dir = fixture(files);
    scan_options(dir.path(), None, super::EnvScan::Source).unwrap()
}

fn references(report: &Report) -> Vec<&Declaration> {
    report
        .environment
        .as_ref()
        .unwrap()
        .iter()
        .filter(|d| d.format == Format::CodeReference)
        .collect()
}

fn names(report: &Report) -> Vec<(&str, &str)> {
    references(report)
        .iter()
        .map(|d| {
            (
                d.name.as_deref().unwrap(),
                d.code.as_ref().unwrap().provider.as_str(),
            )
        })
        .collect()
}

fn find<'a>(report: &'a Report, name: &str) -> &'a Declaration {
    references(report)
        .into_iter()
        .find(|d| d.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("{name} missing: {report:#?}"))
}

fn assert_redacted(report: &Report) {
    let json = serde_json::to_string(report).unwrap();
    let text = render(&serde_json::to_value(report).unwrap()).join("\n");
    for output in [json, text] {
        for forbidden in ["DO_NOT_DISCLOSE", "process.env", "import.meta"] {
            assert!(!output.contains(forbidden), "{output}");
        }
    }
}

#[test]
fn dockerfile_args_are_build_scoped_names_with_default_presence_only() {
    let mut diagnostics = Vec::new();
    let env = parse(
        Format::Dockerfile,
        "Dockerfile",
        "ARG BASE=DO_NOT_DISCLOSE\nFROM node:${BASE}\nARG TOKEN NODE_ENV=DO_NOT_DISCLOSE\nENV PATH=/app\nARG\nARG \"BROKEN=DO_NOT_DISCLOSE\n",
        &mut diagnostics,
    );
    let args = env
        .iter()
        .filter(|d| d.semantics.scope == Some(Scope::Build))
        .map(|d| {
            (
                d.name.as_deref().unwrap(),
                d.location.as_str(),
                d.semantics.has_default,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        args,
        [
            ("BASE", "stage:0/line:1/0", Some(true)),
            ("TOKEN", "stage:1/line:3/0", Some(false)),
            ("NODE_ENV", "stage:1/line:3/1", Some(true)),
        ]
    );
    assert_eq!(env.len(), 4);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(!format!("{env:?}{diagnostics:?}").contains("DO_NOT_DISCLOSE"));
}

#[test]
fn framework_prefixes_match_whole_uppercase_identifiers_per_project() {
    let report = with_env(&[
        (
            "web/package.json",
            r#"{"dependencies": {"next": "15.0.0"}}"#,
        ),
        (
            "web/app/page.tsx",
            "const a = process.env.NEXT_PUBLIC_API_URL + \"DO_NOT_DISCLOSE\";\nconst b = process.env.NEXT_PUBLIC_;\nconst c = NEXT_PUBLIC_lower + MY_NEXT_PUBLIC_HIDDEN + NEXT_PUBLIC_Mixed;\n// NEXT_PUBLIC_API_URL again\n",
        ),
        (
            "web/lib/env.ts",
            "export const url = process.env.NEXT_PUBLIC_API_URL;\n",
        ),
        (
            "spa/src/App.vue",
            "<script setup>\nconst api = import.meta.env.VITE_API_BASE\n</script>\n",
        ),
        ("spa/package.json", "{}"),
        (
            "scripts/run.py",
            "os.environ['REACT_APP_TITLE']\nprint('$EXPO_PUBLIC_KEY')\n",
        ),
        ("docs/README.md", "NEXT_PUBLIC_DOCS_ONLY"),
        ("web/node_modules/pkg/index.js", "NEXT_PUBLIC_VENDORED"),
        ("web/dist/app.js", "NEXT_PUBLIC_BUILD_OUTPUT"),
        ("web/public/app.min.js", "NEXT_PUBLIC_MINIFIED"),
        (
            "nuxt/server/api.ts",
            "useRuntimeConfig(); process.env.NUXT_PUBLIC_SITE_URL; NUXT_API_SECRET",
        ),
    ]);
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    let mut found = names(&report);
    found.sort();
    assert_eq!(
        found,
        [
            ("EXPO_PUBLIC_KEY", "expo"),
            ("NEXT_PUBLIC_API_URL", "nextjs"),
            ("NUXT_API_SECRET", "nuxt"),
            ("NUXT_PUBLIC_SITE_URL", "nuxt"),
            ("REACT_APP_TITLE", "create-react-app"),
            ("VITE_API_BASE", "vite"),
        ]
    );
    let next = find(&report, "NEXT_PUBLIC_API_URL");
    assert_eq!(next.project.as_deref(), Some("web"));
    assert_eq!(next.source, "web/app/page.tsx");
    assert_eq!(next.location, "line:1");
    assert_eq!(next.state, "referenced");
    assert_eq!(next.semantics.scope, Some(Scope::Build));
    let code = next.code.as_ref().unwrap();
    assert_eq!(code.origin, Origin::Framework);
    assert_eq!(code.occurrence_count, 3);
    assert_eq!(
        code.occurrences
            .iter()
            .map(|o| (o.source.as_str(), o.line))
            .collect::<Vec<_>>(),
        [
            ("web/app/page.tsx", 1),
            ("web/app/page.tsx", 4),
            ("web/lib/env.ts", 1)
        ]
    );
    assert_eq!(
        find(&report, "NUXT_PUBLIC_SITE_URL").semantics.scope,
        Some(Scope::Runtime)
    );
    assert_eq!(
        find(&report, "VITE_API_BASE").project.as_deref(),
        Some("spa")
    );
    assert_redacted(&report);
    let json = serde_json::to_value(&report).unwrap();
    let record = json["environment"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "NEXT_PUBLIC_API_URL")
        .unwrap();
    assert_eq!(record["format"], "code-reference");
    assert_eq!(record["origin"], "framework");
    assert_eq!(record["provider"], "nextjs");
    assert_eq!(record["occurrenceCount"], 3);
    let text = render(&json);
    let line = text
        .iter()
        .position(|l| l.starts_with("env NEXT_PUBLIC_API_URL\t"))
        .unwrap();
    assert_eq!(
        text[line + 1],
        "  scope: build, origin: framework, provider: nextjs, occurrenceCount: 3"
    );
}

#[test]
fn generic_prefixes_require_the_framework_and_generic_names_a_platform_file() {
    let files = [
        (
            "astro/package.json",
            r#"{"dependencies": {"astro": "5.0.0"}}"#,
        ),
        (
            "astro/src/pages/index.astro",
            "import.meta.env.PUBLIC_SITE_KEY",
        ),
        ("plain/package.json", "{}"),
        (
            "plain/index.js",
            "const PUBLIC_KEY = crypto.PUBLIC_KEY_PEM;",
        ),
        (
            "edge/handler.ts",
            "VERCEL_URL; VERCEL_NOT_DOCUMENTED; DEPLOY_URL; URL; CONTEXT; FLY_REGION; PRIMARY_REGION; RENDER_EXTERNAL_URL; RENDER; DYNO; CF_PAGES_URL; K_SERVICE",
        ),
    ];
    let report = with_env(&files);
    let mut found = names(&report);
    found.sort();
    assert_eq!(
        found,
        [
            ("CF_PAGES_URL", "cloudflare-pages"),
            ("DYNO", "heroku"),
            ("FLY_REGION", "fly"),
            ("K_SERVICE", "cloud-run"),
            ("PUBLIC_SITE_KEY", "astro"),
            ("RENDER_EXTERNAL_URL", "render"),
            ("VERCEL_URL", "vercel"),
        ]
    );
    let vercel = find(&report, "VERCEL_URL");
    assert_eq!(vercel.code.as_ref().unwrap().origin, Origin::Platform);
    assert_eq!(vercel.semantics.scope, Some(Scope::BuildAndRuntime));
    let mut gated = files.to_vec();
    gated.extend([
        ("netlify.toml", "[build]\ncommand = 'make'\n"),
        ("deploy/fly.toml", "app = 'x'\n"),
        ("render.yaml", "services: []\n"),
    ]);
    let report = with_env(&gated);
    let found = names(&report);
    for expected in [
        ("DEPLOY_URL", "netlify"),
        ("PRIMARY_REGION", "fly"),
        ("RENDER", "render"),
    ] {
        assert!(found.contains(&expected), "{found:?}");
    }
    for never in ["URL", "CONTEXT", "VERCEL_NOT_DOCUMENTED", "PUBLIC_KEY"] {
        assert!(found.iter().all(|(name, _)| *name != never), "{found:?}");
    }
}

#[test]
fn code_references_are_opt_in_capped_bounded_deterministic_and_redacted() {
    let many = (0..25)
        .map(|i| format!("v{i} = NEXT_PUBLIC_MANY // DO_NOT_DISCLOSE\n"))
        .collect::<String>();
    let dir = fixture(&[
        ("package.json", "{}"),
        ("src/many.js", &many),
        ("src/a.ts", "NEXT_PUBLIC_SHARED"),
    ]);
    fs::write(
        dir.path().join("src/latin1.js"),
        b"// caf\xe9 DO_NOT_DISCLOSE\nVITE_LATIN1_OK\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("src/huge.js"),
        format!(
            "NEXT_PUBLIC_TOO_LARGE {}",
            "x".repeat(super::MAX_FILE_BYTES as usize)
        ),
    )
    .unwrap();
    let default = scan(dir.path(), None).unwrap();
    assert_eq!(default.stats.files_read, 1);
    assert!(default.diagnostics.is_empty());
    let report = scan_options(dir.path(), None, super::EnvScan::Source).unwrap();
    assert_eq!(
        scan_options(dir.path(), None, super::EnvScan::Source).unwrap(),
        report
    );
    assert_eq!(report.stats.files_read, 5);
    assert_eq!(default.projects, report.projects);
    let many = find(&report, "NEXT_PUBLIC_MANY").code.as_ref().unwrap();
    assert_eq!(many.occurrence_count, 25);
    assert_eq!(
        many.occurrences.len(),
        super::environment::references::MAX_OCCURRENCES
    );
    assert!(
        references(&report)
            .iter()
            .any(|d| d.name.as_deref() == Some("VITE_LATIN1_OK"))
    );
    assert!(
        references(&report)
            .iter()
            .all(|d| d.name.as_deref() != Some("NEXT_PUBLIC_TOO_LARGE"))
    );
    assert_eq!(report.diagnostics.len(), 1, "{report:#?}");
    assert_eq!(report.diagnostics[0].path, "src/huge.js");
    assert_redacted(&report);

    let rules = super::Rules::builtin().unwrap();
    for index in 0..80 {
        let path = dir.path().join(format!("services/s{index}/src"));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("index.ts"), "NEXT_PUBLIC_SERVICE").unwrap();
    }
    let serial = super::inventory(dir.path(), rules, 1, super::EnvScan::Source).unwrap();
    let parallel = super::inventory(dir.path(), rules, 4, super::EnvScan::Source).unwrap();
    assert_eq!(serial.stats, parallel.stats);
    for (a, b) in serial.nodes.iter().zip(&parallel.nodes) {
        assert_eq!(a.code_hits, b.code_hits);
    }
}

#[test]
fn code_reference_catalog_is_data_validated_and_extensible() {
    let dir = fixture(&[("src/app.ts", "ACME_PUBLIC_TOKEN; NEXT_PUBLIC_STILL")]);
    let extra = r#"
[[catalog.code_references.patterns]]
origin = "framework"
provider = "acme"
prefixes = ["ACME_PUBLIC_"]
scope = "build"
docs = ["https://example.com/acme#env"]
"#;
    let report = scan_options(dir.path(), Some(extra), super::EnvScan::Source).unwrap();
    let mut found = names(&report);
    found.sort();
    assert_eq!(
        found,
        [
            ("ACME_PUBLIC_TOKEN", "acme"),
            ("NEXT_PUBLIC_STILL", "nextjs")
        ]
    );
    for invalid in [
        extra.replace("ACME_PUBLIC_", "acme_public_"),
        extra.replace("https://example.com/acme#env", "not-a-url"),
        extra.replace("origin = \"framework\"", "origin = \"guess\""),
        format!("{extra}signals = [\"dir/file\"]\n"),
        "[[catalog.environment]]\nformat = \"code-reference\"\nfiles = [\"*.ts\"]\n".to_owned(),
    ] {
        assert!(
            scan_options(dir.path(), Some(&invalid), super::EnvScan::Source).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn every_cataloged_provider_matches_a_documented_name() {
    let cases = [
        ("NEXT_PUBLIC_A", "nextjs", Origin::Framework),
        ("VITE_A", "vite", Origin::Framework),
        ("REACT_APP_A", "create-react-app", Origin::Framework),
        ("NUXT_PUBLIC_A", "nuxt", Origin::Framework),
        ("EXPO_PUBLIC_A", "expo", Origin::Framework),
        ("GATSBY_A", "gatsby", Origin::Framework),
        ("VUE_APP_A", "vue-cli", Origin::Framework),
        ("REDWOOD_ENV_A", "redwood", Origin::Framework),
        ("STORYBOOK_A", "storybook", Origin::Framework),
        ("SANITY_STUDIO_A", "sanity", Origin::Framework),
        ("UMI_APP_A", "umi", Origin::Framework),
        ("PLASMO_PUBLIC_A", "plasmo", Origin::Framework),
        ("WXT_A", "wxt", Origin::Framework),
        ("TAURI_ENV_A", "tauri", Origin::Framework),
        ("QUASAR_A", "quasar", Origin::Framework),
        ("RENDERER_VITE_A", "electron-vite", Origin::Framework),
        ("VERCEL_GIT_COMMIT_SHA", "vercel", Origin::Platform),
        ("DEPLOY_PRIME_URL", "netlify", Origin::Platform),
        ("RENDER_GIT_COMMIT", "render", Origin::Platform),
        ("RAILWAY_PUBLIC_DOMAIN", "railway", Origin::Platform),
        ("HEROKU_APP_NAME", "heroku", Origin::Platform),
        ("CF_PAGES_BRANCH", "cloudflare-pages", Origin::Platform),
        ("GAE_SERVICE", "app-engine", Origin::Platform),
        ("KOYEB_PUBLIC_DOMAIN", "koyeb", Origin::Platform),
        ("K_REVISION", "cloud-run", Origin::Platform),
        ("FLY_APP_NAME", "fly", Origin::Platform),
    ];
    let source = cases
        .iter()
        .map(|(name, _, _)| format!("read({name});\n"))
        .collect::<String>();
    let report = with_env(&[("src/all.ts", &source)]);
    let mut found = names(&report);
    found.sort();
    let mut expected = cases
        .iter()
        .map(|(name, provider, _)| (*name, *provider))
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(found, expected);
    for (name, provider, origin) in cases {
        assert_eq!(
            find(&report, name).code.as_ref().unwrap().origin,
            origin,
            "{provider}"
        );
    }
}

#[test]
fn with_env_alone_reads_no_source_files_and_emits_no_code_references() {
    let dir = fixture(&[
        ("package.json", r#"{"dependencies": {"next": "15.0.0"}}"#),
        (".env.example", "NEXT_PUBLIC_DECLARED=DO_NOT_DISCLOSE"),
        ("netlify.toml", "[build.environment]\nNODE_VERSION = '22'\n"),
        (
            "app/page.tsx",
            "process.env.NEXT_PUBLIC_API_URL; VERCEL_URL",
        ),
        ("lib/util.py", "os.environ['REACT_APP_TITLE']"),
        (
            "src/huge.js",
            &"x".repeat(super::MAX_FILE_BYTES as usize + 10),
        ),
    ]);
    let declarations = scan_options(dir.path(), None, super::EnvScan::Declarations).unwrap();
    assert_eq!(declarations.stats.files_read, 3);
    assert!(declarations.diagnostics.is_empty(), "{declarations:#?}");
    let environment = declarations.environment.as_ref().unwrap();
    assert!(
        environment
            .iter()
            .all(|d| d.format != Format::CodeReference)
    );
    assert_eq!(
        environment
            .iter()
            .filter_map(|d| d.name.as_deref())
            .collect::<Vec<_>>(),
        ["NEXT_PUBLIC_DECLARED", "NODE_VERSION"]
    );
    let source = scan_options(dir.path(), None, super::EnvScan::Source).unwrap();
    assert_eq!(source.stats.files_read, 6);
    assert_eq!(source.projects, declarations.projects);
    let mut found = names(&source);
    found.sort();
    assert_eq!(
        found,
        [
            ("NEXT_PUBLIC_API_URL", "nextjs"),
            ("REACT_APP_TITLE", "create-react-app"),
            ("VERCEL_URL", "vercel")
        ]
    );
    assert_redacted(&source);
}

#[test]
fn with_source_requires_with_env() {
    use clap::Parser;
    let parse = |args: &[&str]| {
        crate::Opts::try_parse_from(args).map(|opts| match opts.command {
            crate::cmd::Command::Detect(args) => (args.with_env, args.with_source),
            _ => panic!("not detect"),
        })
    };
    assert_eq!(parse(&["brainpod", "detect"]).unwrap(), (false, false));
    assert_eq!(
        parse(&["brainpod", "detect", "--with-env"]).unwrap(),
        (true, false)
    );
    assert_eq!(
        parse(&[
            "brainpod",
            "detect",
            ".",
            "--with-env",
            "--with-source",
            "--json"
        ])
        .unwrap(),
        (true, true)
    );
    let error = parse(&["brainpod", "detect", "--with-source"]).unwrap_err();
    assert_eq!(
        error.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
    assert!(error.to_string().contains("--with-env"), "{error}");
}
