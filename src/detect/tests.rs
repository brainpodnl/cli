use std::fs;
use std::path::Path;
use std::time::Instant;

use super::{Project, Report, scan};

fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let path = dir.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    dir
}

fn project<'a>(report: &'a Report, path: &str) -> &'a Project {
    report
        .projects
        .iter()
        .find(|p| p.path == path)
        .unwrap_or_else(|| panic!("missing {path}: {report:#?}"))
}

fn detects(project: &Project, id: &str) -> bool {
    project.detections.iter().any(|d| d.id == id)
}

fn version(project: &Project, language: &str, value: &str, source: &str) -> bool {
    project.languages.iter().any(|l| {
        l.name == language
            && l.versions
                .iter()
                .any(|v| v.value == value && v.source == source)
    })
}

#[test]
fn frameworks_and_plain_projects() {
    let cases = [
        (
            "laravel",
            "composer.json",
            r#"{"require":{"php":"^8.3","laravel/framework":"^12","symfony/console":"^7"}}"#,
            "php",
        ),
        (
            "symfony",
            "composer.json",
            r#"{"require":{"symfony/framework-bundle":"^7.2"}}"#,
            "php",
        ),
        (
            "symfony",
            "composer.json",
            r#"{"require":{"symfony/symfony":"^6.4"}}"#,
            "php",
        ),
        (
            "cakephp",
            "composer.json",
            r#"{"require":{"cakephp/cakephp":"^5"}}"#,
            "php",
        ),
        (
            "codeigniter",
            "composer.json",
            r#"{"require":{"codeigniter4/framework":"^4"}}"#,
            "php",
        ),
        (
            "composer",
            "composer.json",
            r#"{"require":{"symfony/console":"^7"}}"#,
            "php",
        ),
        (
            "go-module",
            "go.mod",
            "module example.com/service\ngo 1.24\n",
            "go",
        ),
        (
            "gin",
            "go.mod",
            "module example.com/api\ngo 1.24\nrequire (\n github.com/gin-gonic/gin v1.10.0\n)\n",
            "go",
        ),
        (
            "axum",
            "Cargo.toml",
            "[package]\nname='api'\nrust-version='1.85'\n[dependencies]\naxum='0.8'\n",
            "rust",
        ),
        (
            "rocket",
            "Cargo.toml",
            "[package]\nname='api'\n[dependencies]\nrocket={version='0.5',features=['json']}\n",
            "rust",
        ),
        (
            "cargo",
            "Cargo.toml",
            "[package]\nname='plain'\nversion='0.1.0'\nedition='2024'\n",
            "rust",
        ),
        (
            "rails",
            "Gemfile",
            "source 'https://rubygems.org'\nruby '3.3.1'\ngem 'rails', '~> 8.0'\n",
            "ruby",
        ),
        (
            "sinatra",
            "Gemfile",
            "gem(\"sinatra\", \"~> 4.0\")\n",
            "ruby",
        ),
        ("hanami", "Gemfile", "gem 'hanami', '~> 2.2'\n", "ruby"),
        (
            "django",
            "requirements.txt",
            "# FastAPI\nDjango>=5.1,<6\n",
            "python",
        ),
        (
            "fastapi",
            "pyproject.toml",
            "[project]\nname='api'\nrequires-python='>=3.12'\ndependencies=['FastAPI[standard]>=0.115; python_version >= \"3.12\"']\n",
            "python",
        ),
        (
            "flask",
            "Pipfile",
            "[requires]\npython_version='3.12'\n[packages]\nflask='>=3'\n",
            "python",
        ),
        (
            "django",
            "pyproject.toml",
            "[tool.poetry.dependencies]\npython='^3.12'\ndjango={version='^5.1'}\n",
            "python",
        ),
        (
            "nextjs",
            "package.json",
            r#"{"dependencies":{"next":"^15","react":"^19"},"engines":{"node":">=22"}}"#,
            "javascript",
        ),
        (
            "nuxtjs",
            "package.json",
            r#"{"dependencies":{"nuxt":"^3","vue":"^3"}}"#,
            "javascript",
        ),
        (
            "astro",
            "package.json",
            r#"{"devDependencies":{"astro":"^5"}}"#,
            "javascript",
        ),
        (
            "sveltekit",
            "package.json",
            r#"{"devDependencies":{"@sveltejs/kit":"^2"}}"#,
            "javascript",
        ),
        (
            "express",
            "package.json",
            r#"{"dependencies":{"express":"^5"}}"#,
            "javascript",
        ),
    ];
    for (expected, file, text, language) in cases {
        let dir = fixture(&[(file, text)]);
        let report = scan(dir.path(), None).unwrap();
        assert!(
            report.diagnostics.is_empty(),
            "{expected}: {:?}",
            report.diagnostics
        );
        assert_eq!(report.projects.len(), 1, "{expected}: {report:#?}");
        let p = &report.projects[0];
        assert_eq!(p.detections[0].id, expected, "{p:#?}");
        assert!(p.languages.iter().any(|l| l.name == language));
    }
}

#[test]
fn bare_sources_never_need_to_be_read() {
    for (extension, language) in [
        ("php", "php"),
        ("go", "go"),
        ("rs", "rust"),
        ("rb", "ruby"),
        ("py", "python"),
        ("js", "javascript"),
        ("tsx", "typescript"),
    ] {
        let dir = fixture(&[(&format!("main.{extension}"), "not even valid source")]);
        let report = scan(dir.path(), None).unwrap();
        assert_eq!(report.stats.files_read, 0);
        assert_eq!(report.projects.len(), 1);
        assert_eq!(report.projects[0].languages[0].name, language);
        assert!(report.projects[0].languages[0].versions.is_empty());
    }
}

#[test]
fn complex_polyglot_monorepo() {
    let dir = fixture(&[
        (
            "package.json",
            r#"{"name":"platform","private":true,"workspaces":["apps/*","packages/*"]}"#,
        ),
        (".nvmrc", "22.14.0\n"),
        (".tool-versions", "python 3.13.2\nruby 3.4.1\n"),
        (
            "go.work",
            "go 1.24.0\nuse (\n ./services/gateway\n ./services/jobs\n)\n",
        ),
        (
            "Cargo.toml",
            "[workspace]\nmembers=['crates/*']\nexclude=['crates/excluded']\n[workspace.package]\nrust-version='1.85'\n[workspace.dependencies]\nweb={package='axum',version='0.8'}\nrocket='0.5'\n",
        ),
        ("rust-toolchain.toml", "[toolchain]\nchannel='1.86.0'\n"),
        (
            "apps/web/package.json",
            r#"{"name":"web","dependencies":{"next":"^15","react":"^19"},"devDependencies":{"typescript":"^5.7"},"engines":{"node":">=20"}}"#,
        ),
        ("apps/web/tsconfig.json", "{}"),
        ("apps/web/src/app/page.tsx", "export default () => null"),
        ("apps/web/src/lib/helper.ts", "export const x = 1"),
        (
            "apps/docs/package.json",
            r#"{"dependencies":{"astro":"^5"}}"#,
        ),
        (
            "apps/store/package.json",
            r#"{"dependencies":{"nuxt":"^3"}}"#,
        ),
        (
            "packages/ui/package.json",
            r#"{"name":"ui","dependencies":{"react":"^19"}}"#,
        ),
        (
            "services/gateway/go.mod",
            "module example.com/gateway\ngo 1.23.0\ntoolchain go1.24.1\n",
        ),
        ("services/gateway/cmd/api/main.go", "package main"),
        (
            "services/jobs/go.mod",
            "module example.com/jobs\ngo 1.24.0\n",
        ),
        ("services/jobs/main.go", "package main"),
        (
            "crates/api/Cargo.toml",
            "[package]\nname='api'\nrust-version.workspace=true\n[dependencies]\nweb.workspace=true\n",
        ),
        ("crates/api/src/main.rs", "fn main() {}"),
        (
            "crates/rocket/Cargo.toml",
            "[package]\nname='rocket-api'\n[dependencies]\nrocket.workspace=true\n",
        ),
        ("crates/excluded/Cargo.toml", "[package]\nname='excluded'\n"),
        (
            "services/php/composer.json",
            r#"{"require":{"php":"^8.3","laravel/framework":"^12","symfony/console":"^7"}}"#,
        ),
        (
            "services/php/package.json",
            r#"{"devDependencies":{"vite":"^6"},"engines":{"node":">=22"}}"#,
        ),
        ("services/php/app/Models/User.php", "<?php"),
        (
            "services/symfony/composer.json",
            r#"{"require":{"symfony/framework-bundle":"^7"}}"#,
        ),
        (
            "services/cake/composer.json",
            r#"{"require":{"cakephp/cakephp":"^5"}}"#,
        ),
        (
            "services/codeigniter/composer.json",
            r#"{"require":{"codeigniter4/framework":"^4"}}"#,
        ),
        (
            "services/rails/Gemfile",
            "ruby '>= 3.3'\ngem 'rails', '~> 8.0'\n",
        ),
        ("services/rails/app/models/user.rb", "class User; end"),
        ("services/django/requirements.txt", "Django==5.1.6\n"),
        ("services/django/project/settings.py", "DEBUG = False"),
        (
            "services/fastapi/pyproject.toml",
            "[project]\nname='fastapi-service'\nrequires-python='>=3.12'\ndependencies=['fastapi>=0.115']\n",
        ),
        ("services/fastapi/app/main.py", "# service"),
        ("tools/plain/main.go", "package main"),
        ("tools/script/task.py", "print('hello')"),
        (
            "node_modules/fake/package.json",
            r#"{"dependencies":{"next":"1"}}"#,
        ),
        (
            "services/php/vendor/fake/composer.json",
            r#"{"require":{"symfony/framework-bundle":"1"}}"#,
        ),
        ("target/fake/Cargo.toml", "invalid"),
        (".venv/fake/requirements.txt", "flask==1"),
    ]);
    let report = scan(dir.path(), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    assert_eq!(report.projects.len(), 19, "{report:#?}");
    let root = project(&report, ".");
    for detection in ["node-workspace", "go-workspace", "cargo-workspace"] {
        assert!(detects(root, detection));
    }
    assert_eq!(
        root.workspace_members,
        [
            "apps/docs",
            "apps/store",
            "apps/web",
            "crates/api",
            "crates/rocket",
            "packages/ui",
            "services/gateway",
            "services/jobs"
        ]
    );
    for (path, detection) in [
        ("apps/web", "nextjs"),
        ("apps/docs", "astro"),
        ("apps/store", "nuxtjs"),
        ("crates/api", "axum"),
        ("crates/rocket", "rocket"),
        ("services/php", "laravel"),
        ("services/symfony", "symfony"),
        ("services/cake", "cakephp"),
        ("services/codeigniter", "codeigniter"),
        ("services/rails", "rails"),
        ("services/django", "django"),
        ("services/fastapi", "fastapi"),
    ] {
        assert!(detects(project(&report, path), detection), "{path}");
    }
    assert!(!detects(project(&report, "services/php"), "symfony"));
    assert_eq!(
        project(&report, "services/php")
            .languages
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>(),
        ["javascript", "php"]
    );
    assert!(version(
        project(&report, "apps/web"),
        "javascript",
        "22.14.0",
        ".nvmrc"
    ));
    assert!(version(
        project(&report, "apps/web"),
        "javascript",
        ">=20",
        "apps/web/package.json"
    ));
    assert!(version(
        project(&report, "apps/web"),
        "typescript",
        "^5.7",
        "apps/web/package.json"
    ));
    assert!(version(
        project(&report, "crates/api"),
        "rust",
        "1.85",
        "Cargo.toml"
    ));
    assert!(version(
        project(&report, "crates/api"),
        "rust",
        "1.86.0",
        "rust-toolchain.toml"
    ));
    assert!(version(
        project(&report, "services/django"),
        "python",
        "3.13.2",
        ".tool-versions"
    ));
    assert!(version(
        project(&report, "services/rails"),
        "ruby",
        ">= 3.3",
        "services/rails/Gemfile"
    ));
    assert_eq!(report.stats.skipped_directories, 4);
    assert_eq!(scan(dir.path(), None).unwrap(), report);
}

#[test]
fn avoids_false_framework_matches() {
    let dir = fixture(&[
        (
            "composer.json",
            r#"{"description":"laravel/framework","require":{"symfony/console":"^7"},"require-dev":{"laravel/framework":"^12"}}"#,
        ),
        (
            "Cargo.toml",
            "[package]\nname='axum'\n[dev-dependencies]\nrocket='0.5'\n[workspace.dependencies]\naxum='0.8'\n",
        ),
        (
            "Gemfile",
            "# gem 'rails'\ngem 'rails-html-sanitizer'\ngem 'sinatra-contrib'\n",
        ),
        (
            "requirements.txt",
            "# django==5\ndjango-filter>=24\nfastapi-utils==0.8\n",
        ),
        (
            "package.json",
            r#"{"scripts":{"start":"next start"},"dependencies":{"next-themes":"*","@astrojs/check":"*"}}"#,
        ),
    ]);
    let report = scan(dir.path(), None).unwrap();
    assert!(
        report
            .projects
            .iter()
            .flat_map(|p| &p.detections)
            .all(|d| d.kind != super::Kind::Framework),
        "{report:#?}"
    );
}

#[test]
fn custom_patterns_and_priority() {
    let dir = fixture(&[
        ("package.json", r#"{"dependencies":{"next":"15"}}"#),
        ("special.config", ""),
        ("disabled", ""),
    ]);
    let extra = r#"
[[rules]]
id = 'company-next'
language = 'javascript'
kind = 'framework'
priority = 999
all = ['dep:npm:next', 'file:special.*']
none = ['file:blocked']
[[rules]]
id = 'must-not-match'
language = 'javascript'
kind = 'framework'
priority = 1000
any = ['file:special.config']
none = ['file:disabled']
"#;
    let report = scan(dir.path(), Some(extra)).unwrap();
    assert_eq!(report.projects[0].detections[0].id, "company-next");
    assert!(!detects(&report.projects[0], "must-not-match"));
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["schemaVersion"], 1);
    assert!(super::render(&json)[0].contains("company-next"));
    for invalid in [
        extra.replace("company-next", "nextjs"),
        extra.replace("file:blocked", "unsupported:x"),
        extra.replace("priority = 999", "prio = 999"),
    ] {
        assert!(scan(dir.path(), Some(&invalid)).is_err());
    }
}

#[test]
fn malformed_and_oversized_manifests_are_diagnostic_not_fatal() {
    let dir = fixture(&[
        ("broken/package.json", "["),
        ("valid/composer.json", "{}"),
        ("invalid/Cargo.toml", "["),
    ]);
    fs::write(dir.path().join("broken/composer.json"), [0xff, 0xfe]).unwrap();
    fs::write(
        dir.path().join("broken/Gemfile"),
        vec![b'a'; super::MAX_FILE_BYTES as usize + 20],
    )
    .unwrap();
    let report = scan(dir.path(), None).unwrap();
    assert_eq!(report.diagnostics.len(), 4, "{report:#?}");
    assert!(detects(project(&report, "valid"), "composer"));
    assert!(report.stats.bytes_read <= super::MAX_FILE_BYTES as usize + 100);
}

#[test]
fn version_constraints_keep_provenance_and_do_not_invent_versions() {
    let dir = fixture(&[
        (
            "composer.json",
            r#"{"require":{"php":"^8.2","laravel/framework":"^12"},"config":{"platform":{"php":"8.3.14"}}}"#,
        ),
        (
            "Cargo.toml",
            "[package]\nname='plain'\nversion='9.9.9'\nedition='2024'\n",
        ),
        ("Gemfile", "gem 'rails', '~> 8.0', '>= 8.0.1'\n"),
        (".python-version", "3.12.8\n"),
        (
            "app/pyproject.toml",
            "[project]\nrequires-python='>=3.11,<4'\n",
        ),
    ]);
    let report = scan(dir.path(), None).unwrap();
    let p = project(&report, ".");
    assert!(version(p, "php", "^8.2", "composer.json"));
    assert!(version(p, "php", "8.3.14", "composer.json"));
    assert!(
        p.languages
            .iter()
            .find(|l| l.name == "rust")
            .unwrap()
            .versions
            .is_empty()
    );
    let rails = p.detections.iter().find(|d| d.id == "rails").unwrap();
    assert_eq!(rails.evidence[0].value.as_deref(), Some("~> 8.0, >= 8.0.1"));
    assert!(version(
        project(&report, "app"),
        "python",
        "3.12.8",
        ".python-version"
    ));
}

#[test]
fn python_uv_workspace_and_workspace_exclusions() {
    let dir = fixture(&[
        (
            "pyproject.toml",
            "[tool.uv.workspace]\nmembers=['services/*']\nexclude=['services/ignored']\n",
        ),
        (
            "services/api/pyproject.toml",
            "[project]\nname='api'\ndependencies=['fastapi==0.115']\n",
        ),
        (
            "services/ignored/pyproject.toml",
            "[project]\nname='ignored'\n",
        ),
    ]);
    let report = scan(dir.path(), None).unwrap();
    assert_eq!(project(&report, ".").workspace_members, ["services/api"]);
}

#[test]
fn glob_matching_respects_directory_boundaries() {
    for (pattern, value, expected) in [
        ("services/*", "services/api", true),
        ("services/*", "services/api/nested", false),
        ("services/**/api", "services/a/b/api", true),
        ("services/**/api", "different/api", false),
        ("services/**/api", "services/api", true),
        ("./crates/*", "crates/api", true),
    ] {
        assert_eq!(
            super::rules::path_glob(pattern, value),
            expected,
            "{pattern}: {value}"
        );
    }
}

#[cfg(unix)]
#[test]
fn symlinks_are_not_followed() {
    let dir = fixture(&[("real/go.mod", "module example.com/api\ngo 1.24\n")]);
    std::os::unix::fs::symlink(dir.path(), dir.path().join("real/loop")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("real/go.mod"), dir.path().join("go.mod")).unwrap();
    let report = scan(dir.path(), None).unwrap();
    assert_eq!(report.projects.len(), 1);
    assert_eq!(report.stats.files_read, 1);
    assert_eq!(report.stats.directories, 2);
}

#[test]
fn source_volume_does_not_increase_manifest_reads() {
    let dir = fixture(&[("go.mod", "module example.com/api\ngo 1.24\n")]);
    for i in 0..200 {
        let path = dir.path().join(format!("pkg/p{i}"));
        fs::create_dir_all(&path).unwrap();
        for j in 0..10 {
            fs::write(path.join(format!("file{j}.go")), "package p").unwrap();
        }
    }
    let report = scan(dir.path(), None).unwrap();
    assert_eq!(report.projects.len(), 1);
    assert_eq!(report.stats.files_read, 1);
    assert_eq!(report.stats.files, 2001);
}

#[test]
fn cli_is_local_and_authentication_free() {
    use clap::Parser;
    let opts = crate::Opts::try_parse_from([
        "brainpod",
        "detect",
        "./services",
        "--json",
        "--rules",
        "custom.toml",
    ])
    .unwrap();
    assert!(!crate::cmd::needs_client(&opts.command));
    assert!(!crate::cmd::needs_api_token(&opts.command));
    assert!(opts.json);
    let crate::cmd::Command::Detect(args) = opts.command else {
        panic!("expected detect");
    };
    assert_eq!(args.path, Path::new("./services"));
    assert_eq!(args.rules.as_deref(), Some(Path::new("custom.toml")));
}

#[test]
#[ignore = "manual release-mode performance measurement"]
fn benchmark_polyglot_scan() {
    let dir = tempfile::tempdir().unwrap();
    let manifests = [
        (
            "composer.json",
            r#"{"require":{"php":"^8.3","laravel/framework":"^12"}}"#,
            "php",
        ),
        ("go.mod", "module example.com/api\ngo 1.24\n", "go"),
        (
            "Cargo.toml",
            "[package]\nname='api'\n[dependencies]\naxum='0.8'\n",
            "rs",
        ),
        ("Gemfile", "ruby '3.4'\ngem 'rails', '~> 8.0'\n", "rb"),
        (
            "pyproject.toml",
            "[project]\nrequires-python='>=3.12'\ndependencies=['fastapi>=0.115']\n",
            "py",
        ),
        (
            "package.json",
            r#"{"dependencies":{"next":"^15","react":"^19"},"engines":{"node":">=22"}}"#,
            "js",
        ),
        (
            "package.json",
            r#"{"dependencies":{"nuxt":"^3"},"devDependencies":{"typescript":"^5.7"}}"#,
            "ts",
        ),
    ];
    for i in 0..200 {
        let path = dir.path().join(format!("services/s{i}"));
        fs::create_dir_all(path.join("src")).unwrap();
        let (file, manifest, extension) = manifests[i % manifests.len()];
        fs::write(path.join(file), manifest).unwrap();
        for j in 0..100 {
            fs::write(
                path.join(format!("src/file{j}.{extension}")),
                vec![b'x'; 1024],
            )
            .unwrap();
        }
    }
    let mut samples = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        let report = scan(dir.path(), None).unwrap();
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(report.projects.len(), 200);
        assert_eq!(report.stats.files_read, 200);
        assert_eq!(report.stats.files, 20200);
        std::hint::black_box(report);
    }
    let first = samples[0];
    samples.sort_by(f64::total_cmp);
    eprintln!(
        "200 polyglot services, 20,200 files (20 warm filesystem runs): first {first:.3} ms, median {:.3} ms, p95 {:.3} ms; 200 manifest reads, zero source reads",
        samples[10], samples[18]
    );
}
