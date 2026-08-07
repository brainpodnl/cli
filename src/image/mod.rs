use std::fmt;
use std::fs::Permissions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::ValueEnum;
use flate2::read::GzDecoder;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::{Builder, NamedTempFile, TempDir};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

mod registry;

use registry::Registry;

const RAILPACK_VERSION: &str = "v0.35.0";
const RAILPACK_FRONTEND: &str = "ghcr.io/railwayapp/railpack-frontend:v0.35.0";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const PROCESS_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const NETWORK_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const NON_ROOT_DOCKERFILE: &str = r#"FROM base
RUN groupadd --gid 1000 railpack \
    && useradd --uid 1000 --gid 1000 --home-dir /home/railpack --create-home --shell /bin/false railpack \
    && if [ -d /root ]; then cp -a /root/. /home/railpack/; fi \
    && chown -R 1000:1000 /home/railpack \
    && if [ -d /mise ]; then chown -R 1000:1000 /mise; fi
ENV HOME=/home/railpack
USER 1000:1000
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum BuildMethod {
    Auto,
    Railpack,
    Dockerfile,
}

impl BuildMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Railpack => "railpack",
            Self::Dockerfile => "dockerfile",
        }
    }
}

impl fmt::Display for BuildMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str((*self).as_str())
    }
}

struct RailpackAsset {
    target: &'static str,
    sha256: &'static str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    manifests: Vec<Descriptor>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Descriptor {
    pub media_type: String,
    pub digest: String,
    pub size: u64,
    pub platform: Option<OciPlatform>,
}

#[derive(Clone, Deserialize)]
pub(super) struct OciPlatform {
    architecture: String,
    os: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Manifest {
    pub config: Descriptor,
    pub layers: Vec<Descriptor>,
}

#[derive(Deserialize)]
struct ImageConfiguration {
    config: RuntimeConfiguration,
}

#[derive(Deserialize)]
struct RuntimeConfiguration {
    #[serde(rename = "User")]
    user: Option<String>,
}

pub(super) struct ImageLayout {
    pub root: PathBuf,
    pub descriptor: Descriptor,
    pub manifest: Manifest,
    pub manifest_bytes: Vec<u8>,
    runtime_user: Option<String>,
}

pub async fn build(
    image: String,
    context: PathBuf,
    tag: String,
    method: BuildMethod,
    output: Option<PathBuf>,
    platform: String,
    pod: &str,
    api_token: &str,
    registry_endpoint: &str,
) -> Result<Value> {
    validate_repository(&image)?;
    validate_namespace(pod)?;
    validate_tag(&tag)?;

    let context = context
        .canonicalize()
        .with_context(|| format!("failed to resolve build context {}", context.display()))?;
    if !context.is_dir() {
        return Err(anyhow!(
            "build context is not a directory: {}",
            context.display()
        ));
    }

    let method = resolve_method(method, &context)?;
    let registry = Registry::new(registry_endpoint, api_token)?;
    registry.authenticate().await?;
    let repository = format!("{pod}/{image}");
    let tagged_reference = format!("{}/{repository}:{tag}", registry.authority());

    let (temporary, output) = temporary_directory(output.as_deref())?;
    ensure_builder().await?;

    let final_layout = match method {
        BuildMethod::Railpack => build_railpack(&context, &temporary, &platform).await?,
        BuildMethod::Dockerfile => build_dockerfile(&context, &temporary, &platform).await?,
        BuildMethod::Auto => return Err(anyhow!("automatic builder selection was not resolved")),
    };
    let layout = load_layout(&final_layout, &platform).await?;
    if method == BuildMethod::Railpack && layout.runtime_user.as_deref() != Some("1000:1000") {
        return Err(anyhow!(
            "built Railpack image does not enforce the non-root user 1000:1000"
        ));
    }
    let retained_output = if let Some(output) = output {
        retain_layout(final_layout.clone(), output.clone()).await?;
        Some(output)
    } else {
        None
    };

    eprintln!("Pushing {tagged_reference}");
    registry.push(&repository, &tag, &layout).await?;

    let digest = layout.descriptor.digest.clone();
    let digest_reference = format!("{}/{repository}@{digest}", registry.authority());

    Ok(json!({
        "image": tagged_reference,
        "digest": digest,
        "reference": digest_reference,
        "platform": platform,
        "builder": method.as_str(),
        "railpackVersion": (method == BuildMethod::Railpack).then_some(RAILPACK_VERSION),
        "user": layout.runtime_user,
        "output": retained_output,
    }))
}

fn resolve_method(method: BuildMethod, context: &Path) -> Result<BuildMethod> {
    let dockerfile = context.join("Dockerfile");
    match method {
        BuildMethod::Auto if dockerfile.is_file() => Ok(BuildMethod::Dockerfile),
        BuildMethod::Auto => Ok(BuildMethod::Railpack),
        BuildMethod::Dockerfile if dockerfile.is_file() => Ok(BuildMethod::Dockerfile),
        BuildMethod::Dockerfile => Err(anyhow!(
            "Dockerfile builder requires {}",
            dockerfile.display()
        )),
        BuildMethod::Railpack => Ok(BuildMethod::Railpack),
    }
}

async fn build_dockerfile(context: &Path, temporary: &TempDir, platform: &str) -> Result<PathBuf> {
    let destination = temporary.path().join("image");
    eprintln!("Building image from Dockerfile");
    run_command(
        Command::new("docker")
            .arg("buildx")
            .arg("build")
            .arg("--builder")
            .arg("brainpod")
            .arg("--platform")
            .arg(platform)
            .arg("--provenance=false")
            .arg("--output")
            .arg(format!("type=oci,dest={},tar=false", destination.display()))
            .arg("--file")
            .arg(context.join("Dockerfile"))
            .arg(context),
        "Dockerfile image build",
    )
    .await?;
    Ok(destination)
}

async fn build_railpack(context: &Path, temporary: &TempDir, platform: &str) -> Result<PathBuf> {
    let railpack = railpack_binary().await?;
    let plan = temporary.path().join("railpack-plan.json");
    eprintln!("Preparing Railpack build plan");
    run_command(
        Command::new(&railpack)
            .arg("prepare")
            .arg(context)
            .arg("--plan-out")
            .arg(&plan),
        "Railpack plan generation",
    )
    .await?;

    let base_layout = temporary.path().join("base");
    eprintln!("Building image with Railpack");
    run_command(
        Command::new("docker")
            .arg("buildx")
            .arg("build")
            .arg("--builder")
            .arg("brainpod")
            .arg("--platform")
            .arg(platform)
            .arg("--provenance=false")
            .arg("--build-arg")
            .arg(format!("BUILDKIT_SYNTAX={RAILPACK_FRONTEND}"))
            .arg("--output")
            .arg(format!("type=oci,dest={},tar=false", base_layout.display()))
            .arg("--file")
            .arg(&plan)
            .arg(context),
        "Railpack image build",
    )
    .await?;

    let secure_context = temporary.path().join("non-root");
    tokio::fs::create_dir(&secure_context)
        .await
        .context("failed to create non-root build context")?;
    let dockerfile = secure_context.join("Dockerfile");
    tokio::fs::write(&dockerfile, NON_ROOT_DOCKERFILE)
        .await
        .context("failed to write non-root Dockerfile")?;

    let destination = temporary.path().join("image");
    eprintln!("Applying non-root runtime user");
    run_command(
        Command::new("docker")
            .arg("buildx")
            .arg("build")
            .arg("--builder")
            .arg("brainpod")
            .arg("--platform")
            .arg(platform)
            .arg("--provenance=false")
            .arg("--build-context")
            .arg(format!("base=oci-layout://{}", base_layout.display()))
            .arg("--output")
            .arg(format!("type=oci,dest={},tar=false", destination.display()))
            .arg("--file")
            .arg(&dockerfile)
            .arg(&secure_context),
        "non-root image build",
    )
    .await?;
    Ok(destination)
}

fn temporary_directory(output: Option<&Path>) -> Result<(TempDir, Option<PathBuf>)> {
    let output = output
        .map(|output| {
            if output.exists() {
                return Err(anyhow!(
                    "OCI output path already exists: {}",
                    output.display()
                ));
            }
            let output = std::path::absolute(output).with_context(|| {
                format!("failed to resolve OCI output path {}", output.display())
            })?;
            output
                .parent()
                .ok_or_else(|| anyhow!("OCI output path has no parent: {}", output.display()))?;
            Ok(output)
        })
        .transpose()?;
    Ok((Builder::new().prefix("brainpod-image-").tempdir()?, output))
}

async fn retain_layout(source: PathBuf, destination: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let parent = destination
            .parent()
            .ok_or_else(|| anyhow!("OCI output path has no parent: {}", destination.display()))?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory {}", parent.display()))?;
        let staging = Builder::new()
            .prefix(".brainpod-image-")
            .tempdir_in(parent)
            .with_context(|| {
                format!(
                    "failed to create temporary directory in {}",
                    parent.display()
                )
            })?;
        copy_directory(&source, staging.path())?;
        std::fs::rename(staging.path(), &destination)
            .with_context(|| format!("failed to retain OCI layout at {}", destination.display()))?;
        Ok(())
    })
    .await
    .context("OCI layout copy task failed")?
}

fn copy_directory(source: &Path, destination: &Path) -> Result<()> {
    for entry in std::fs::read_dir(source)
        .with_context(|| format!("failed to read OCI directory {}", source.display()))?
    {
        let entry = entry.context("failed to read OCI directory entry")?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", source_path.display()))?;
        if file_type.is_dir() {
            std::fs::create_dir(&destination_path)
                .with_context(|| format!("failed to create {}", destination_path.display()))?;
            copy_directory(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&source_path, &destination_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    source_path.display(),
                    destination_path.display()
                )
            })?;
        } else {
            return Err(anyhow!(
                "OCI layout contains unsupported entry {}",
                source_path.display()
            ));
        }
    }
    Ok(())
}

async fn ensure_builder() -> Result<()> {
    let inspection = command_output(
        Command::new("docker")
            .arg("buildx")
            .arg("inspect")
            .arg("brainpod"),
        "Docker Buildx inspection",
    )
    .await
    .context("failed to execute Docker Buildx; install Docker with Buildx support")?;

    if !inspection.status.success() {
        eprintln!("Creating Brainpod BuildKit builder");
        let creation = command_output(
            Command::new("docker")
                .arg("buildx")
                .arg("create")
                .arg("--driver=docker-container")
                .arg("--name=brainpod"),
            "BuildKit builder creation",
        )
        .await?;
        if !creation.status.success() {
            let reinspection = command_output(
                Command::new("docker")
                    .arg("buildx")
                    .arg("inspect")
                    .arg("brainpod"),
                "BuildKit builder inspection",
            )
            .await?;
            if !reinspection.status.success() {
                let message = String::from_utf8_lossy(&creation.stderr);
                return Err(anyhow!(
                    "BuildKit builder creation failed: {}",
                    message.trim()
                ));
            }
        }
    }

    run_command(
        Command::new("docker")
            .arg("buildx")
            .arg("inspect")
            .arg("--bootstrap")
            .arg("brainpod"),
        "BuildKit builder startup",
    )
    .await
}

async fn command_output(command: &mut Command, description: &str) -> Result<Output> {
    let mut child = command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start {description}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture {description} stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture {description} stderr"))?;
    let mut stdout = tokio::spawn(read_output(stdout));
    let mut stderr = tokio::spawn(read_output(stderr));

    let status = match wait_for_child(&mut child, description, PROCESS_TIMEOUT).await {
        Ok(status) => status,
        Err(error) => {
            stdout.abort();
            stderr.abort();
            return Err(error);
        }
    };
    let stdout = finish_output(&mut stdout, description, "stdout").await?;
    let stderr = finish_output(&mut stderr, description, "stderr").await?;

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

async fn read_output<R>(mut source: R) -> std::io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut output = Vec::new();
    source.read_to_end(&mut output).await?;
    Ok(output)
}

async fn finish_output(
    task: &mut JoinHandle<std::io::Result<Vec<u8>>>,
    description: &str,
    stream: &str,
) -> Result<Vec<u8>> {
    match tokio::time::timeout(PROCESS_OUTPUT_DRAIN_TIMEOUT, &mut *task).await {
        Ok(result) => result
            .with_context(|| format!("{description} {stream} task failed"))?
            .with_context(|| format!("failed to read {description} {stream}")),
        Err(_) => {
            task.abort();
            Ok(Vec::new())
        }
    }
}

async fn run_command(command: &mut Command, description: &str) -> Result<()> {
    run_command_with_timeout(command, description, PROCESS_TIMEOUT).await
}

/// Forwards a child stream to stderr line by line, teeing it into the console.
///
/// Batched rather than written per line: the session file is replaced whole on
/// every write, and a build emits thousands of lines.
async fn mirror<R>(stream: R) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    const BATCH: usize = 24;

    let mut lines = tokio::io::BufReader::new(stream).lines();
    let mut destination = tokio::io::stderr();
    let mut batch = Vec::with_capacity(BATCH);

    while let Some(line) = lines.next_line().await? {
        destination.write_all(line.as_bytes()).await?;
        destination.write_all(b"\n").await?;
        batch.push(line);
        if batch.len() >= BATCH {
            crate::agent::append_log(std::mem::take(&mut batch));
        }
    }

    destination.flush().await?;
    crate::agent::append_log(batch);
    Ok(())
}

async fn run_command_with_timeout(
    command: &mut Command,
    description: &str,
    timeout: Duration,
) -> Result<()> {
    // Build output is the whole reason the session console has an output panel,
    // so mirror it there when one exists. Without a console nothing changes:
    // stdout is copied through byte for byte and the child keeps this process's
    // stderr, which is what preserves a terminal's redrawing progress display.
    let capture = crate::agent::is_active();

    let mut child = command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(if capture {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .spawn()
        .with_context(|| format!("failed to start {description}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture {description} stdout"))?;
    let errors = child
        .stderr
        .take()
        .map(|stderr| tokio::spawn(mirror(stderr)));
    let mut output = tokio::spawn(async move {
        if capture {
            return mirror(stdout).await;
        }
        let mut destination = tokio::io::stderr();
        tokio::io::copy(&mut stdout, &mut destination).await?;
        destination.flush().await
    });

    let status = match wait_for_child(&mut child, description, timeout).await {
        Ok(status) => status,
        Err(error) => {
            output.abort();
            if let Some(errors) = errors {
                errors.abort();
            }
            return Err(error);
        }
    };
    match tokio::time::timeout(PROCESS_OUTPUT_DRAIN_TIMEOUT, &mut output).await {
        Ok(result) => result
            .with_context(|| format!("{description} output task failed"))?
            .with_context(|| format!("failed while reading {description} output"))?,
        Err(_) => output.abort(),
    }
    if let Some(mut errors) = errors {
        match tokio::time::timeout(PROCESS_OUTPUT_DRAIN_TIMEOUT, &mut errors).await {
            Ok(result) => result
                .with_context(|| format!("{description} error output task failed"))?
                .with_context(|| format!("failed while reading {description} error output"))?,
            Err(_) => errors.abort(),
        }
    }

    if status.success() {
        Ok(())
    } else if let Some(code) = status.code() {
        Err(anyhow!("{description} failed with exit code {code}"))
    } else {
        Err(anyhow!("{description} terminated by a signal"))
    }
}

async fn wait_for_child(
    child: &mut Child,
    description: &str,
    timeout: Duration,
) -> Result<std::process::ExitStatus> {
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(result) => result.with_context(|| format!("failed while running {description}")),
        Err(_) => {
            let _ = child.start_kill();
            Err(anyhow!("{description} timed out after {timeout:?}"))
        }
    }
}

async fn railpack_binary() -> Result<PathBuf> {
    let asset = railpack_asset()?;
    let cache = dirs::cache_dir()
        .ok_or_else(|| anyhow!("cannot locate the operating system cache directory"))?
        .join("brainpod/tools/railpack")
        .join(RAILPACK_VERSION)
        .join(asset.target);
    let binary = cache.join("railpack");
    if binary.is_file() {
        return Ok(binary);
    }

    tokio::fs::create_dir_all(&cache)
        .await
        .with_context(|| format!("failed to create Railpack cache {}", cache.display()))?;
    let archive_name = format!("railpack-{RAILPACK_VERSION}-{}.tar.gz", asset.target);
    let url = format!(
        "https://github.com/railwayapp/railpack/releases/download/{RAILPACK_VERSION}/{archive_name}"
    );
    eprintln!("Downloading Railpack {RAILPACK_VERSION}");
    let http = reqwest::Client::builder()
        .timeout(NETWORK_TIMEOUT)
        .build()
        .context("failed to create Railpack download client")?;
    let archive = http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("failed to download Railpack from {url}"))?
        .error_for_status()
        .with_context(|| format!("failed to download Railpack from {url}"))?
        .bytes()
        .await
        .context("failed to read Railpack download")?;

    let actual = format!("{:x}", Sha256::digest(&archive));
    if actual != asset.sha256 {
        return Err(anyhow!(
            "Railpack download checksum mismatch: expected {}, got {actual}",
            asset.sha256
        ));
    }

    let destination = binary.clone();
    let cache_for_extract = cache.clone();
    tokio::task::spawn_blocking(move || {
        let mut tar = tar::Archive::new(GzDecoder::new(archive.as_ref()));
        let mut temporary = NamedTempFile::new_in(&cache_for_extract)
            .context("failed to create temporary Railpack binary")?;
        let mut found = false;

        for entry in tar.entries().context("failed to read Railpack archive")? {
            let mut entry = entry.context("failed to read Railpack archive entry")?;
            let path = entry.path().context("invalid path in Railpack archive")?;
            if path == Path::new("railpack") || path == Path::new("./railpack") {
                if !entry.header().entry_type().is_file() {
                    return Err(anyhow!("Railpack archive binary is not a regular file"));
                }
                std::io::copy(&mut entry, temporary.as_file_mut())
                    .context("failed to extract Railpack binary")?;
                found = true;
                break;
            }
        }

        if !found {
            return Err(anyhow!(
                "Railpack archive does not contain the railpack binary"
            ));
        }
        temporary
            .as_file_mut()
            .flush()
            .context("failed to flush Railpack binary")?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            temporary
                .as_file()
                .set_permissions(Permissions::from_mode(0o755))
                .context("failed to make Railpack executable")?;
        }

        match temporary.persist(&destination) {
            Ok(_) => Ok(destination),
            Err(_) if destination.is_file() => Ok(destination),
            Err(error) => Err(error.error).context("failed to cache Railpack binary"),
        }
    })
    .await
    .context("Railpack extraction task failed")?
}

fn railpack_asset() -> Result<RailpackAsset> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok(RailpackAsset {
            target: "x86_64-unknown-linux-musl",
            sha256: "d039785dd926ba059031c9c463c51f1462f344c844f828ac872c1f6d46fed7f1",
        }),
        ("linux", "aarch64") => Ok(RailpackAsset {
            target: "arm64-unknown-linux-musl",
            sha256: "ad147486812f2d17c7fd2a6965c580e5e1fa4f7e51f5c9e458ad2c3a5f6a3f79",
        }),
        ("macos", "x86_64") => Ok(RailpackAsset {
            target: "x86_64-apple-darwin",
            sha256: "cd69c23a2e412e62c092a043501b05ccd5560418025be61338e3d545e221b301",
        }),
        ("macos", "aarch64") => Ok(RailpackAsset {
            target: "arm64-apple-darwin",
            sha256: "fb4c16d57458eb7868d48ed8a454014ef40716a6c939929d7b7d5986563a0c65",
        }),
        (os, architecture) => Err(anyhow!(
            "Railpack {RAILPACK_VERSION} is not available for {os}/{architecture}"
        )),
    }
}

async fn load_layout(root: &Path, expected_platform: &str) -> Result<ImageLayout> {
    let index: Index = serde_json::from_slice(
        &tokio::fs::read(root.join("index.json"))
            .await
            .context("OCI layout is missing index.json")?,
    )
    .context("OCI layout contains an invalid index.json")?;
    if index.manifests.len() != 1 {
        return Err(anyhow!(
            "OCI layout must contain exactly one image manifest, found {}",
            index.manifests.len()
        ));
    }

    let descriptor = index
        .manifests
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("OCI layout contains no image manifest"))?;
    let platform = descriptor
        .platform
        .as_ref()
        .ok_or_else(|| anyhow!("OCI layout manifest does not declare a platform"))?;
    let actual = format!("{}/{}", platform.os, platform.architecture);
    if actual != expected_platform {
        return Err(anyhow!(
            "OCI layout platform mismatch: expected {expected_platform}, got {actual}"
        ));
    }

    let manifest_bytes = read_blob(root, &descriptor.digest).await?;
    verify_size(&manifest_bytes, &descriptor)?;
    verify_digest(&manifest_bytes, &descriptor.digest)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .context("OCI layout contains an invalid image manifest")?;
    let config_bytes = read_blob(root, &manifest.config.digest).await?;
    verify_size(&config_bytes, &manifest.config)?;
    verify_digest(&config_bytes, &manifest.config.digest)?;
    let configuration: ImageConfiguration = serde_json::from_slice(&config_bytes)
        .context("OCI layout contains an invalid image configuration")?;
    let runtime_user = configuration.config.user.filter(|user| !user.is_empty());

    Ok(ImageLayout {
        root: root.to_path_buf(),
        descriptor,
        manifest,
        manifest_bytes,
        runtime_user,
    })
}

pub(super) fn blob_path(root: &Path, digest: &str) -> Result<PathBuf> {
    let (algorithm, encoded) = digest
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid OCI digest {digest}"))?;
    if algorithm != "sha256"
        || encoded.len() != 64
        || !encoded
            .chars()
            .all(|character| character.is_ascii_digit() || matches!(character, 'a'..='f'))
    {
        return Err(anyhow!("invalid OCI digest {digest}"));
    }
    Ok(root.join("blobs").join(algorithm).join(encoded))
}

async fn read_blob(root: &Path, digest: &str) -> Result<Vec<u8>> {
    let path = blob_path(root, digest)?;
    tokio::fs::read(&path)
        .await
        .with_context(|| format!("failed to read OCI blob {}", path.display()))
}

fn verify_size(contents: &[u8], descriptor: &Descriptor) -> Result<()> {
    if contents.len() as u64 == descriptor.size {
        Ok(())
    } else {
        Err(anyhow!(
            "OCI blob {} has size {}, expected {}",
            descriptor.digest,
            contents.len(),
            descriptor.size
        ))
    }
}

fn verify_digest(contents: &[u8], expected: &str) -> Result<()> {
    let actual = format!("sha256:{:x}", Sha256::digest(contents));
    if actual == expected {
        Ok(())
    } else {
        Err(anyhow!(
            "OCI digest mismatch: expected {expected}, got {actual}"
        ))
    }
}

fn validate_repository(repository: &str) -> Result<()> {
    let valid = !repository.is_empty()
        && repository.len() <= 255
        && repository.split('/').all(|component| {
            !component.is_empty()
                && component.chars().all(|character| {
                    character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || matches!(character, '.' | '_' | '-')
                })
                && component.chars().next().is_some_and(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit()
                })
                && component.chars().last().is_some_and(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit()
                })
        });
    if valid {
        Ok(())
    } else {
        Err(anyhow!(
            "image must be a lowercase repository name within the selected pod"
        ))
    }
}

fn validate_namespace(namespace: &str) -> Result<()> {
    let valid =
        !namespace.is_empty()
            && namespace.len() <= 63
            && namespace.chars().all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
            })
            && namespace.chars().next().is_some_and(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit()
            })
            && namespace.chars().last().is_some_and(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit()
            });
    if valid {
        Ok(())
    } else {
        Err(anyhow!(
            "pod is not a valid registry namespace: {namespace}"
        ))
    }
}

fn validate_tag(tag: &str) -> Result<()> {
    let mut characters = tag.chars();
    let valid = tag.len() <= 128
        && characters
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-')
        });
    if valid {
        Ok(())
    } else {
        Err(anyhow!("invalid image tag {tag}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BuildMethod, resolve_method, validate_namespace, validate_repository, validate_tag,
        verify_digest,
    };

    #[cfg(unix)]
    use super::run_command_with_timeout;
    #[cfg(unix)]
    use std::time::{Duration, Instant};
    #[cfg(unix)]
    use tokio::process::Command;

    #[cfg(unix)]
    #[tokio::test]
    async fn command_exit_does_not_wait_for_inherited_stdout() {
        let started = Instant::now();
        let error = run_command_with_timeout(
            Command::new("sh").arg("-c").arg("sleep 5 & exit 42"),
            "test command",
            Duration::from_secs(10),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("exit code 42"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_timeout_terminates_the_process() {
        let error = run_command_with_timeout(
            Command::new("sh").arg("-c").arg("exec sleep 10"),
            "test command",
            Duration::from_millis(25),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("timed out"));
    }

    #[test]
    fn automatically_prefers_a_dockerfile() {
        let context = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_method(BuildMethod::Auto, context.path()).unwrap(),
            BuildMethod::Railpack
        );

        std::fs::write(context.path().join("Dockerfile"), "FROM scratch").unwrap();
        assert_eq!(
            resolve_method(BuildMethod::Auto, context.path()).unwrap(),
            BuildMethod::Dockerfile
        );
    }

    #[test]
    fn explicit_dockerfile_requires_one() {
        let context = tempfile::tempdir().unwrap();
        assert!(resolve_method(BuildMethod::Dockerfile, context.path()).is_err());
    }

    #[test]
    fn validates_repository_names_with_paths() {
        assert!(validate_repository("services/api-v2").is_ok());
        assert!(validate_repository("registry.example.com:5000/api").is_err());
        assert!(validate_repository("../api").is_err());
        assert!(validate_repository("API").is_err());
    }

    #[test]
    fn validates_registry_namespaces() {
        assert!(validate_namespace("my-pod").is_ok());
        assert!(validate_namespace("../pub").is_err());
        assert!(validate_namespace("MyPod").is_err());
    }

    #[test]
    fn validates_tags() {
        assert!(validate_tag("v1.2-rc_1").is_ok());
        assert!(validate_tag("bad/tag").is_err());
        assert!(validate_tag(".hidden").is_err());
    }

    #[test]
    fn verifies_sha256_digests() {
        assert!(
            verify_digest(
                b"brainpod",
                "sha256:34969396f74cd6135e922d8fd5d0a76a1bf33d0678b9ec12b71c9b3bf5949f68"
            )
            .is_ok()
        );
        assert!(verify_digest(b"brainpod", "sha256:deadbeef").is_err());
    }
}
