use super::tests::{detects, fixture, project, version};
use super::{FrameworkRole, scan};

#[test]
fn bare_java_and_csharp_sources_are_never_read() {
    for (file, language) in [
        ("Main.java", "java"),
        ("Program.cs", "csharp"),
        ("script.csx", "csharp"),
    ] {
        let dir = fixture(&[(file, "not valid source")]);
        let report = scan(dir.path(), None).unwrap();
        assert_eq!(report.stats.files_read, 0);
        assert_eq!(report.projects[0].languages[0].name, language);
        assert!(report.projects[0].languages[0].versions.is_empty());
    }
}

#[test]
fn dotnet_web_targets_language_and_sdk_versions_are_distinct() {
    let dir = fixture(&[
        (
            "global.json",
            r#"{"sdk":{"version":"10.0.403","rollForward":"latestFeature"}}"#,
        ),
        (
            "Directory.Build.props",
            "<Project><PropertyGroup><TargetFramework>net10.0</TargetFramework><LangVersion>14.0</LangVersion></PropertyGroup></Project>",
        ),
        (
            "src/Web/Web.csproj",
            "<Project Sdk='Microsoft.NET.Sdk.Web'><PropertyGroup><AssemblyName>Shop.Web</AssemblyName></PropertyGroup></Project>",
        ),
        ("src/Web/Program.cs", "source must not be read"),
    ]);
    let report = scan(dir.path(), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    let web = project(&report, "src/Web");
    assert!(detects(web, "aspnetcore"));
    assert!(version(web, "csharp", "net10.0", "Directory.Build.props"));
    assert!(version(web, "csharp", "14.0", "Directory.Build.props"));
    assert!(version(web, "csharp", "10.0.403", "global.json"));
    let versions = &web
        .languages
        .iter()
        .find(|l| l.name == "csharp")
        .unwrap()
        .versions;
    assert!(
        versions
            .iter()
            .any(|v| v.value == "net10.0" && v.kind == "target")
    );
    assert!(
        versions
            .iter()
            .any(|v| v.value == "14.0" && v.kind == "language")
    );
    assert!(
        versions
            .iter()
            .any(|v| v.value == "10.0.403" && v.kind == "sdk")
    );
    assert_eq!(report.stats.files_read, 3);
}

#[test]
fn msbuild_import_order_defaults_and_central_package_versions() {
    let dir = fixture(&[
        (
            "Directory.Build.props",
            "<Project><Import Project='eng/base.props'/><PropertyGroup><TargetFramework Condition=\"'$(TargetFramework)' == ''\">net10.0</TargetFramework><LangVersion>14.0</LangVersion></PropertyGroup></Project>",
        ),
        (
            "eng/base.props",
            "<Project><PropertyGroup><LangVersion>13.0</LangVersion></PropertyGroup></Project>",
        ),
        (
            "Directory.Packages.props",
            "<Project><PropertyGroup><FastVersion>8.3.0</FastVersion></PropertyGroup><ItemGroup><PackageVersion Include='FastEndpoints' Version='$(FastVersion)'/></ItemGroup></Project>",
        ),
        (
            "src/Web/Web.csproj",
            "<Project Sdk='Microsoft.NET.Sdk.Web'><ItemGroup><PackageReference Include='FastEndpoints'/><PackageReference Include='Some.Library'><Version>2.0</Version></PackageReference></ItemGroup></Project>",
        ),
    ]);
    let report = scan(dir.path(), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    let web = project(&report, "src/Web");
    assert!(version(web, "csharp", "net10.0", "Directory.Build.props"));
    assert!(version(web, "csharp", "14.0", "Directory.Build.props"));
    assert!(!version(web, "csharp", "13.0", "eng/base.props"));
    let detection = web
        .detections
        .iter()
        .find(|d| d.id == "fastendpoints")
        .unwrap();
    assert_eq!(detection.roles, [FrameworkRole::Uses]);
    assert!(
        detection
            .evidence
            .iter()
            .any(|e| e.source == "Directory.Packages.props" && e.value.as_deref() == Some("8.3.0"))
    );
    assert!(
        detection
            .evidence
            .iter()
            .any(|e| e.source == "src/Web/Web.csproj")
    );
}

#[test]
fn dotnet_solutions_find_members_without_counting_solution_folders() {
    for (file, content) in [
        (
            "App.sln",
            "Project(\"TYPE\") = \"Web, API\", \"src\\Web\\Web.csproj\", \"ID\"\nEndProject\nProject(\"FOLDER\") = \"src\", \"src\", \"ID2\"\nEndProject\n",
        ),
        (
            "App.slnx",
            "<Solution><Folder Name='/src/'><Project Path='src/Web/Web.csproj'/></Folder></Solution>",
        ),
    ] {
        let dir = fixture(&[
            (file, content),
            (
                "src/Web/Web.csproj",
                "<Project Sdk='Microsoft.NET.Sdk.Web'/>",
            ),
        ]);
        let report = scan(dir.path(), None).unwrap();
        assert!(report.diagnostics.is_empty(), "{report:#?}");
        assert!(detects(project(&report, "."), "dotnet-workspace"));
        assert_eq!(project(&report, ".").workspace_members, ["src/Web"]);
    }
}

#[test]
fn dotnet_target_frameworks_do_not_invent_a_csharp_language_version() {
    let dir = fixture(&[(
        "App.csproj",
        "<Project Sdk='Microsoft.NET.Sdk'><PropertyGroup><TargetFrameworks>net8.0;net10.0</TargetFrameworks></PropertyGroup></Project>",
    )]);
    let report = scan(dir.path(), None).unwrap();
    let p = project(&report, ".");
    assert!(version(p, "csharp", "net8.0", "App.csproj"));
    assert!(version(p, "csharp", "net10.0", "App.csproj"));
    assert!(p.languages[0].versions.iter().all(|v| v.kind != "language"));
    assert!(!detects(p, "aspnetcore"));
}

#[test]
fn msbuild_unknown_conditions_and_property_cycles_are_not_executed() {
    let dir = fixture(&[(
        "App.csproj",
        "<Project Sdk='Microsoft.NET.Sdk'><PropertyGroup><A>$(B)</A><B>$(A)</B><TargetFramework>$(A)</TargetFramework><LangVersion Condition=\"$([System.IO.File]::Exists('external'))\">14.0</LangVersion></PropertyGroup></Project>",
    )]);
    let report = scan(dir.path(), None).unwrap();
    let p = project(&report, ".");
    assert!(
        p.languages[0]
            .versions
            .iter()
            .any(|v| v.kind == "reference")
    );
    assert!(
        p.languages[0]
            .versions
            .iter()
            .any(|v| v.kind == "conditional-language")
    );
    assert!(p.languages[0].versions.iter().all(|v| v.kind != "target"));
}

#[test]
fn maven_spring_boot_parent_and_java_release_are_recognized() {
    let dir = fixture(&[(
        "pom.xml",
        "<m:project xmlns:m='http://maven.apache.org/POM/4.0.0'><m:parent><m:groupId>org.springframework.boot</m:groupId><m:artifactId>spring-boot-starter-parent</m:artifactId><m:version>4.1.0</m:version><m:relativePath/></m:parent><m:artifactId>petclinic</m:artifactId><m:properties><m:java.version>17</m:java.version><m:maven.compiler.release>${java.version}</m:maven.compiler.release></m:properties></m:project>",
    )]);
    let report = scan(dir.path(), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    let p = project(&report, ".");
    assert!(detects(p, "maven"));
    assert!(detects(p, "spring-boot"));
    assert!(version(p, "java", "17", "pom.xml"));
    assert!(
        p.languages[0]
            .versions
            .iter()
            .any(|v| v.value == "17" && v.kind == "release")
    );
}

#[test]
fn maven_modules_inherit_local_properties_and_managed_versions() {
    let dir = fixture(&[
        (
            "pom.xml",
            "<project><groupId>example</groupId><artifactId>platform</artifactId><version>1</version><properties><java.version>21</java.version><boot.version>4.0.0</boot.version></properties><modules><module>api</module></modules><dependencyManagement><dependencies><dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-web</artifactId><version>${boot.version}</version></dependency></dependencies></dependencyManagement></project>",
        ),
        (
            "api/pom.xml",
            "<project><parent><groupId>example</groupId><artifactId>platform</artifactId><version>1</version></parent><artifactId>api</artifactId><dependencies><dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-web</artifactId></dependency></dependencies></project>",
        ),
    ]);
    let report = scan(dir.path(), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    assert_eq!(project(&report, ".").workspace_members, ["api"]);
    assert!(!detects(project(&report, "."), "spring-boot"));
    let api = project(&report, "api");
    assert!(detects(api, "spring-boot"));
    assert!(version(api, "java", "21", "pom.xml"));
    let boot = api
        .detections
        .iter()
        .find(|d| d.id == "spring-boot")
        .unwrap();
    assert!(
        boot.evidence
            .iter()
            .any(|e| e.source == "pom.xml" && e.value.as_deref() == Some("4.0.0"))
    );
}

#[test]
fn maven_test_dependencies_and_plugin_management_are_not_runtime_frameworks() {
    let dir = fixture(&[(
        "pom.xml",
        "<project><artifactId>plain</artifactId><dependencies><dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-web</artifactId><scope>test</scope></dependency></dependencies><build><pluginManagement><plugins><plugin><groupId>org.springframework.boot</groupId><artifactId>spring-boot-maven-plugin</artifactId></plugin></plugins></pluginManagement></build></project>",
    )]);
    let report = scan(dir.path(), None).unwrap();
    assert!(!detects(project(&report, "."), "spring-boot"));
}

#[test]
fn xml_entities_cdata_namespaces_and_structure_are_checked() {
    let dir = fixture(&[(
        "App.csproj",
        "<Project Sdk='Microsoft.NET.Sdk.Web'><PropertyGroup><TargetFramework>net&#x31;0.0</TargetFramework><LangVersion><![CDATA[14.0]]></LangVersion><AssemblyName>A&amp;B</AssemblyName></PropertyGroup></Project>",
    )]);
    let report = scan(dir.path(), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    assert!(version(
        project(&report, "."),
        "csharp",
        "net10.0",
        "App.csproj"
    ));
    assert!(project(&report, ".").names.contains(&"A&B".to_owned()));
    for broken in [
        "<Project><PropertyGroup></Project></PropertyGroup>",
        "<Project Sdk='a' Sdk='b'/>",
        "<x:Project/>",
        "<!DOCTYPE Project [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><Project>&x;</Project>",
    ] {
        let dir = fixture(&[("App.csproj", broken)]);
        let report = scan(dir.path(), None).unwrap();
        assert_eq!(report.diagnostics.len(), 1, "{broken}: {report:#?}");
        assert_eq!(report.stats.files_read, 1);
    }
}

#[test]
fn gradle_groovy_and_kotlin_plugins_and_toolchains_are_static_facts() {
    for (file, content) in [
        (
            "build.gradle",
            "plugins { id 'org.springframework.boot' version '4.0.0' }\ngroup = 'example'\njava { toolchain { languageVersion = JavaLanguageVersion.of(21) } }\ndependencies { implementation 'org.springframework:spring-web:7.0.0' }\n",
        ),
        (
            "build.gradle.kts",
            "plugins { id(\"org.springframework.boot\") version \"4.0.0\" }\ngroup = \"example\"\njava { toolchain { languageVersion.set(JavaLanguageVersion.of(21)) } }\ndependencies { implementation(\"org.springframework:spring-web:7.0.0\") }\n",
        ),
    ] {
        let dir = fixture(&[(file, content)]);
        let report = scan(dir.path(), None).unwrap();
        assert!(report.diagnostics.is_empty(), "{file}: {report:#?}");
        let p = project(&report, ".");
        assert!(detects(p, "spring-boot"));
        assert!(detects(p, "spring"));
        assert!(version(p, "java", "21", file));
    }
}

#[test]
fn gradle_inactive_plugins_comments_and_test_dependencies_do_not_match() {
    let dir = fixture(&[(
        "build.gradle",
        "plugins { id 'org.springframework.boot' version '4.0.0' apply false }\n// plugins { id 'org.springframework.boot' }\ndependencies { testImplementation 'org.springframework:spring-web:7.0.0' }\ndef description = \"org.springframework.boot\"\n",
    )]);
    let report = scan(dir.path(), None).unwrap();
    assert!(!detects(project(&report, "."), "spring-boot"));
    assert!(!detects(project(&report, "."), "spring"));
}

#[test]
fn gradle_settings_literal_lists_and_source_only_modules_form_workspaces() {
    let dir = fixture(&[
        (
            "settings.gradle",
            "rootProject.name='platform'\nList projects = ['api', 'worker']\ninclude projects.toArray(new String[0])\n",
        ),
        ("build.gradle", "allprojects { group = 'example' }\n"),
        ("api/src/main/java/Main.java", "not read"),
        ("worker/build.gradle", "plugins { id 'java' }\n"),
    ]);
    let report = scan(dir.path(), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    assert_eq!(project(&report, ".").workspace_members, ["api", "worker"]);
    assert!(detects(project(&report, "api"), "gradle"));
    assert!(
        report
            .projects
            .iter()
            .all(|p| p.path != "api/src/main/java")
    );
}

#[test]
fn gradle_regex_literals_do_not_create_fake_strings_or_plugins() {
    let dir = fixture(&[(
        "build.gradle",
        r#"plugins { id 'java' }
def pattern = /"preemptedAt"\s*:\s*"([^"]+)"/
def other = $/"an "embedded" quote"/$
"#,
    )]);
    let report = scan(dir.path(), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    assert!(detects(project(&report, "."), "gradle"));
}

#[test]
fn mixed_java_csharp_monorepo_keeps_runtime_versions_in_the_right_services() {
    let dir = fixture(&[
        (
            "services/java/pom.xml",
            "<project><artifactId>api</artifactId><properties><java.version>21</java.version></properties></project>",
        ),
        (
            "services/dotnet/global.json",
            r#"{"sdk":{"version":"10.0.403"}}"#,
        ),
        (
            "services/dotnet/Web.csproj",
            "<Project Sdk='Microsoft.NET.Sdk.Web'><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>",
        ),
        ("services/dotnet/Program.cs", "not read"),
        (
            "services/frontend/package.json",
            r#"{"dependencies":{"next":"16"},"engines":{"node":">=24"}}"#,
        ),
    ]);
    let report = scan(dir.path(), None).unwrap();
    assert_eq!(report.projects.len(), 3);
    assert!(version(
        project(&report, "services/java"),
        "java",
        "21",
        "services/java/pom.xml"
    ));
    assert!(
        project(&report, "services/java")
            .languages
            .iter()
            .all(|l| l.name != "csharp")
    );
    assert!(version(
        project(&report, "services/dotnet"),
        "csharp",
        "10.0.403",
        "services/dotnet/global.json"
    ));
    assert!(
        project(&report, "services/frontend")
            .languages
            .iter()
            .all(|l| l.name != "java" && l.name != "csharp")
    );
}

#[test]
fn closest_global_json_selects_the_sdk_without_inheriting_sdk_package_catalogs() {
    let dir = fixture(&[
        (
            "global.json",
            r#"{"sdk":{"version":"10.0.100"},"msbuild-sdks":{"Aspire.AppHost.Sdk":"13.0.0"}}"#,
        ),
        ("nested/global.json", r#"{"sdk":{"version":"11.0.100"}}"#),
        (
            "nested/App.csproj",
            "<Project Sdk='Aspire.AppHost.Sdk'><PropertyGroup><TargetFramework>net11.0</TargetFramework></PropertyGroup></Project>",
        ),
    ]);
    let report = scan(dir.path(), None).unwrap();
    let app = project(&report, "nested");
    assert!(version(app, "csharp", "11.0.100", "nested/global.json"));
    assert!(!version(app, "csharp", "10.0.100", "global.json"));
    let aspire = app.detections.iter().find(|d| d.id == "aspire").unwrap();
    assert!(aspire.evidence.iter().all(|e| e.value.is_none()));
}

#[test]
fn java_pin_and_gradle_daemon_jdk_are_not_reported_as_target_release() {
    let dir = fixture(&[
        (
            "build.gradle",
            "plugins { id 'java' }\njava { sourceCompatibility = JavaVersion.VERSION_17 }\n",
        ),
        (
            "gradle/gradle-daemon-jvm.properties",
            "toolchainVersion=25\n",
        ),
        (".sdkmanrc", "java=25.0.1-tem\n"),
    ]);
    let report = scan(dir.path(), None).unwrap();
    let java = project(&report, ".")
        .languages
        .iter()
        .find(|l| l.name == "java")
        .unwrap();
    assert!(
        java.versions
            .iter()
            .any(|v| v.value == "17" && v.kind == "source")
    );
    assert!(
        java.versions
            .iter()
            .any(|v| v.value == "25" && v.kind == "build-toolchain")
    );
    assert!(
        java.versions
            .iter()
            .any(|v| v.value == "25.0.1-tem" && v.kind == "pin")
    );
    assert!(
        !java
            .versions
            .iter()
            .any(|v| v.value == "25" && v.kind == "target")
    );
}

#[test]
fn maven_relative_parents_with_different_coordinates_are_not_inherited() {
    let dir = fixture(&[
        (
            "pom.xml",
            "<project><groupId>example</groupId><artifactId>platform</artifactId><version>2</version><properties><java.version>25</java.version></properties></project>",
        ),
        (
            "api/pom.xml",
            "<project><parent><groupId>example</groupId><artifactId>platform</artifactId><version>1</version></parent><artifactId>api</artifactId></project>",
        ),
    ]);
    let report = scan(dir.path(), None).unwrap();
    assert!(!version(project(&report, "api"), "java", "25", "pom.xml"));
}

#[test]
fn classic_aspnet_and_aspire_sdk_declarations_are_separate_framework_facts() {
    let dir = fixture(&[
        (
            "legacy/Web.csproj",
            "<Project xmlns='http://schemas.microsoft.com/developer/msbuild/2003'><PropertyGroup><TargetFrameworkVersion>v4.8</TargetFrameworkVersion></PropertyGroup><ItemGroup><Reference Include='System.Web.Mvc, Version=5.2.0.0'/></ItemGroup></Project>",
        ),
        (
            "apphost/AppHost.csproj",
            "<Project Sdk='Aspire.AppHost.Sdk/13.0.0'><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>",
        ),
    ]);
    let report = scan(dir.path(), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    assert!(detects(project(&report, "legacy"), "aspnet"));
    assert!(!detects(project(&report, "legacy"), "aspnetcore"));
    assert!(version(
        project(&report, "legacy"),
        "csharp",
        "v4.8",
        "legacy/Web.csproj"
    ));
    assert!(detects(project(&report, "apphost"), "aspire"));
}

#[test]
fn gradle_project_directory_and_name_overrides_remain_distinct() {
    let dir = fixture(&[
        (
            "settings.gradle",
            "rootProject.name='platform'\ninclude ':web'\nproject(':web').projectDir = file('services/api')\nproject(':web').name = 'web-service'\n",
        ),
        ("build.gradle", "allprojects { group = 'example' }\n"),
        ("services/api/build.gradle", "plugins { id 'java' }\n"),
    ]);
    let report = scan(dir.path(), None).unwrap();
    assert_eq!(project(&report, ".").workspace_members, ["services/api"]);
    assert_eq!(project(&report, "services/api").names, ["web-service"]);
}

#[test]
fn dotnet_build_outputs_are_pruned_without_skipping_rust_source_bins() {
    let dir = fixture(&[
        ("Web/Web.csproj", "<Project Sdk='Microsoft.NET.Sdk.Web'/>"),
        ("Web/obj/generated/Fake.csproj", "<invalid"),
        ("Web/bin/generated/Fake.csproj", "<invalid"),
        (
            "rust/Cargo.toml",
            "[package]\nname='tool'\nversion='1.0.0'\n",
        ),
        ("rust/src/bin/main.rs", "not read"),
        (".gradle/cache/build.gradle", "invalid"),
    ]);
    let report = scan(dir.path(), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{report:#?}");
    assert_eq!(report.projects.len(), 2);
    assert_eq!(report.stats.files_read, 2);
    assert_eq!(report.stats.skipped_directories, 3);
    assert_eq!(report.stats.files, 3);
}

#[test]
fn msbuild_host_and_configuration_conditions_remain_conditional() {
    let dir = fixture(&[(
        "App.csproj",
        "<Project Sdk='Microsoft.NET.Sdk'><PropertyGroup><TargetFramework>net10.0</TargetFramework><LangVersion Condition=\"'$(Configuration)' != 'Debug'\">13.0</LangVersion><LangVersion Condition=\"'$(Configuration)' == 'Debug'\">14.0</LangVersion></PropertyGroup></Project>",
    )]);
    let report = scan(dir.path(), None).unwrap();
    let versions = &project(&report, ".").languages[0].versions;
    assert!(
        versions
            .iter()
            .any(|v| v.value == "13.0" && v.kind == "conditional-language")
    );
    assert!(
        versions
            .iter()
            .any(|v| v.value == "14.0" && v.kind == "conditional-language")
    );
    assert!(versions.iter().all(|v| v.kind != "language"));
}

#[test]
fn disabled_central_package_management_does_not_supply_a_version() {
    let dir = fixture(&[
        (
            "Directory.Packages.props",
            "<Project><ItemGroup><PackageVersion Include='FastEndpoints' Version='8.3.0'/></ItemGroup></Project>",
        ),
        (
            "App.csproj",
            "<Project Sdk='Microsoft.NET.Sdk.Web'><PropertyGroup><ManagePackageVersionsCentrally>false</ManagePackageVersionsCentrally></PropertyGroup><ItemGroup><PackageReference Include='FastEndpoints'/></ItemGroup></Project>",
        ),
    ]);
    let report = scan(dir.path(), None).unwrap();
    let framework = project(&report, ".")
        .detections
        .iter()
        .find(|d| d.id == "fastendpoints")
        .unwrap();
    assert!(framework.evidence.iter().all(|e| e.value.is_none()));
}
