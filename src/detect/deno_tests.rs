use super::scan;
use super::tests::{detects, fixture, project, version};

#[test]
fn deno_jsonc_detects_runtime_and_package_versions_without_conflation() {
    let dir = fixture(&[
        (
            "deno.jsonc",
            r#"{ // package version is not a Deno runtime pin
            "name":"@acme/service", "version":"9.9.9", "imports":{"web":"jsr:@hono/hono@^4",},
        }"#,
        ),
        (".tool-versions", "deno 2.5.6\nnodejs 24.1.0\n"),
        ("main.ts", "throw new Error('must not execute');"),
    ]);
    let report = scan(dir.path(), None).unwrap();
    let root = project(&report, ".");
    assert!(detects(root, "deno"));
    assert!(detects(root, "hono"));
    assert_eq!(root.runtimes.len(), 1);
    assert_eq!(root.runtimes[0].name, "deno");
    assert_eq!(root.runtimes[0].source, "deno.jsonc");
    assert_eq!(root.runtimes[0].versions[0].value, "2.5.6");
    assert_eq!(root.runtimes[0].versions[0].source, ".tool-versions");
    assert!(version(root, "javascript", "24.1.0", ".tool-versions"));
    assert!(root.languages.iter().all(|language| {
        language.name != "deno"
            && language
                .versions
                .iter()
                .all(|version| version.value != "9.9.9" && version.value != "2.5.6")
    }));
    assert_eq!(report.stats.files_read, 2);
    assert!(report.diagnostics.is_empty());
}

#[test]
fn mixed_deno_npm_and_pnpm_workspaces_preserve_boundaries_and_member_overrides() {
    let dir = fixture(&[
        (
            "deno.json",
            r#"{"workspace":["./packages/*","!packages/excluded"],"imports":{"web":"jsr:@fresh/core@^2"}}"#,
        ),
        (".deno-version", "2.5.0"),
        ("package.json", r#"{"workspaces":["node-only/*"]}"#),
        ("pnpm-workspace.yaml", "packages: ['pnpm-only/*']\n"),
        ("packages/deno/deno.json", "{}"),
        ("packages/deno/main.ts", ""),
        (
            "packages/hybrid/deno.jsonc",
            r#"{"imports":{"web":"npm:hono@^4"},}"#,
        ),
        (
            "packages/hybrid/package.json",
            r#"{"name":"hybrid","engines":{"node":">=22","deno":">=2.4"}}"#,
        ),
        ("packages/npm/package.json", r#"{"name":"npm-only"}"#),
        ("packages/excluded/package.json", "{}"),
        ("node-only/member/package.json", "{}"),
        ("pnpm-only/member/package.json", "{}"),
        ("outside/package.json", "{}"),
    ]);
    let report = scan(dir.path(), None).unwrap();
    let root = project(&report, ".");
    assert!(detects(root, "deno-workspace"));
    assert!(detects(root, "node-workspace"));
    for name in [
        "packages/deno",
        "packages/hybrid",
        "packages/npm",
        "node-only/member",
        "pnpm-only/member",
    ] {
        assert!(
            root.workspace_members.iter().any(|path| path == name),
            "{name}"
        );
    }
    assert!(
        !root
            .workspace_members
            .iter()
            .any(|path| path == "packages/excluded" || path == "outside")
    );
    for name in ["packages/deno", "packages/npm", "node-only/member"] {
        let member = project(&report, name);
        assert_eq!(member.runtimes[0].name, "deno");
        assert!(
            member.runtimes[0]
                .versions
                .iter()
                .any(|version| version.value == "2.5.0"
                    && version.kind == "pin"
                    && version.source == ".deno-version")
        );
        assert!(detects(member, "fresh"));
    }
    assert!(project(&report, "pnpm-only/member").runtimes.is_empty());
    let deno_workspace = root
        .workspaces
        .iter()
        .find(|workspace| workspace.ecosystem == "deno")
        .unwrap();
    assert!(
        !deno_workspace
            .members
            .iter()
            .any(|member| member.starts_with("node-only/") || member.starts_with("pnpm-only/"))
    );
    let hybrid = project(&report, "packages/hybrid");
    assert!(detects(hybrid, "hono"));
    assert!(!detects(hybrid, "fresh"));
    assert!(hybrid.runtimes[0].versions.iter().any(
        |version| version.value == ">=2.4" && version.source == "packages/hybrid/package.json"
    ));
    assert!(version(
        hybrid,
        "javascript",
        ">=22",
        "packages/hybrid/package.json"
    ));
    for name in ["outside", "packages/excluded"] {
        assert!(project(&report, name).runtimes.is_empty());
        assert!(!detects(project(&report, name), "fresh"));
    }
}

#[test]
fn deno_local_import_maps_keep_their_own_evidence_and_member_aliases_can_block() {
    let dir = fixture(&[
        (
            "deno.json",
            r#"{"workspace":["member"],"importMap":"config/custom-map.jsonc"}"#,
        ),
        (
            "config/custom-map.jsonc",
            r#"{"imports":{"web":"https://deno.land/x/fresh@1.7.3/mod.ts",},}"#,
        ),
        (
            "member/deno.json",
            r#"{"imports":{"web":"./not-a-framework.ts"}}"#,
        ),
    ]);
    let report = scan(dir.path(), None).unwrap();
    let root = project(&report, ".");
    let fresh = root.detections.iter().find(|d| d.id == "fresh").unwrap();
    assert!(
        fresh
            .evidence
            .iter()
            .any(|e| e.source == "config/custom-map.jsonc" && e.value.as_deref() == Some("1.7.3"))
    );
    assert!(!detects(project(&report, "member"), "fresh"));
    assert_eq!(report.stats.files_read, 3);
    assert!(report.diagnostics.is_empty());
}

#[test]
fn deno_json_precedes_jsonc_and_invalid_metadata_is_diagnostic() {
    let dir = fixture(&[
        ("deno.json", "{}"),
        (
            "deno.jsonc",
            r#"{"workspace":["foo"],"imports":{"x":"npm:hono@4"}}"#,
        ),
        ("foo/package.json", "{}"),
    ]);
    let report = scan(dir.path(), None).unwrap();
    let root = project(&report, ".");
    assert!(detects(root, "deno"));
    assert!(!detects(root, "hono"));
    assert!(!detects(root, "deno-workspace"));
    for value in ["[]", "{broken", "{ /* unterminated"] {
        let dir = fixture(&[("deno.jsonc", value)]);
        let report = scan(dir.path(), None).unwrap();
        assert_eq!(report.diagnostics.len(), 1);
        assert!(report.projects.is_empty());
    }
}

#[test]
fn deno_catalogs_reuse_resolution_and_package_json_takes_precedence() {
    let dir = fixture(&[
        (
            "deno.json",
            r#"{"workspace":["packages/*"],"catalog":{"typescript":"^5.4"},"catalogs":{"old":{"typescript":"^5.3"}}}"#,
        ),
        (
            "package.json",
            r#"{"catalog":{"typescript":"^5.9"},"catalogs":{"old":{"typescript":"^5.8"}}}"#,
        ),
        (
            "packages/npm/package.json",
            r#"{"devDependencies":{"typescript":"catalog:"}}"#,
        ),
        (
            "packages/deno/deno.json",
            r#"{"imports":{"typescript":"catalog:old"}}"#,
        ),
    ]);
    let report = scan(dir.path(), None).unwrap();
    for (path, expected) in [("packages/npm", "^5.9"), ("packages/deno", "^5.8")] {
        assert!(version(
            project(&report, path),
            "typescript",
            expected,
            "package.json"
        ));
    }
    assert!(report.diagnostics.is_empty());
}

#[test]
fn import_maps_do_not_escape_root_fetch_urls_or_read_oversized_files() {
    for target in [
        "../outside.json",
        "https://example.invalid/imports.json",
        "/tmp/imports.json",
        "missing.json",
    ] {
        let dir = fixture(&[(
            "deno.json",
            &format!(
                r#"{{"importMap":{}}}"#,
                serde_json::to_string(target).unwrap()
            ),
        )]);
        let report = scan(dir.path(), None).unwrap();
        assert_eq!(report.stats.files_read, 1);
        assert_eq!(report.diagnostics.len(), 1);
    }
    let dir = fixture(&[
        ("deno.json", r#"{"importMap":"huge.json"}"#),
        ("huge.json", &" ".repeat(1024 * 1024 + 1)),
    ]);
    let report = scan(dir.path(), None).unwrap();
    assert_eq!(report.stats.files_read, 2);
    assert!(report.diagnostics[0].message.contains("1 MiB"));
}

#[cfg(unix)]
#[test]
fn import_maps_do_not_follow_file_or_parent_symlinks() {
    for target in ["link.json", "link-dir/map.json"] {
        let outside = fixture(&[("map.json", r#"{"imports":{"x":"npm:hono@4"}}"#)]);
        let dir = fixture(&[("deno.json", &format!(r#"{{"importMap":"{target}"}}"#))]);
        std::os::unix::fs::symlink(
            outside.path().join("map.json"),
            dir.path().join("link.json"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link-dir")).unwrap();
        let report = scan(dir.path(), None).unwrap();
        assert_eq!(report.stats.files_read, 1);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("symlinks"))
        );
        assert!(!detects(project(&report, "."), "hono"));
    }
}

#[test]
fn hybrid_imports_preserve_peer_development_and_jsr_constraints() {
    let dir = fixture(&[
        (
            "deno.json",
            r#"{"imports":{"web":"jsr:@hono/hono@^4.8.3"}}"#,
        ),
        (
            "package.json",
            r#"{"peerDependencies":{"hono":">=4"},"devDependencies":{"hono":"^4.11.5"}}"#,
        ),
    ]);
    let report = scan(dir.path(), None).unwrap();
    let hono = project(&report, ".")
        .detections
        .iter()
        .find(|d| d.id == "hono")
        .unwrap();
    for (value, source) in [
        (">=4", "package.json"),
        ("^4.11.5", "package.json"),
        ("^4.8.3", "deno.json"),
    ] {
        assert!(
            hono.evidence
                .iter()
                .any(|e| e.source == source && e.value.as_deref() == Some(value))
        );
    }
}

#[test]
fn text_output_includes_an_unversioned_runtime_for_npm_only_members() {
    let dir = fixture(&[
        ("deno.json", r#"{"workspace":["member"]}"#),
        ("member/package.json", "{}"),
    ]);
    let report = scan(dir.path(), None).unwrap();
    let text = super::render(&serde_json::to_value(&report).unwrap()).join("\n");
    assert!(text.contains("runtime deno (deno.json)"));
    assert!(project(&report, "member").runtimes[0].versions.is_empty());
}
