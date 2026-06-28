//! Build the capsule image and record its image ARN.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use aws_sdk_lambdamicrovms::types::{
    CodeArtifact, HookState, Hooks, MicrovmHooks, MicrovmImageHooks,
};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;

use crate::aws::Aws;
use crate::config::Config;
use crate::poll::{PollOpts, poll_until};
use crate::state::State;

/// AWS probes lifecycle hooks on this port.
const HOOK_PORT: i32 = 9000;

pub async fn run(name: &str, app: Option<&str>, capsule_dir_override: Option<&str>) -> Result<()> {
    let cfg = Config::load()?;
    let mut state = State::load()?;

    let capsule_dir = capsule_dir(capsule_dir_override)?;
    if let Some(app) = app {
        tracing::info!(target: "hellbox::build", "note: --app {app} — ensure it's staged under capsule/app/");
    }

    let context_zip = zip_context(&capsule_dir)
        .with_context(|| format!("zipping build context at {}", capsule_dir.display()))?;
    tracing::info!(target: "hellbox::build", "built context zip at {}", context_zip.path().display());

    let key = format!("contexts/{name}.zip");
    let bytes = std::fs::read(context_zip.path()).context("reading context zip")?;
    let aws = Aws::new(&cfg).await?;
    aws.s3
        .put_object()
        .bucket(&cfg.artifact_bucket)
        .key(&key)
        .body(bytes.into())
        .send()
        .await
        .with_context(|| format!("uploading s3://{}/{key}", cfg.artifact_bucket))?;
    drop(context_zip);
    let code_artifact_uri = format!("s3://{}/{}", cfg.artifact_bucket, key);
    tracing::info!(target: "hellbox::build", "uploaded {code_artifact_uri}");

    let hooks = Hooks::builder()
        .port(HOOK_PORT)
        .microvm_image_hooks(
            MicrovmImageHooks::builder()
                .ready(HookState::Enabled)
                .ready_timeout_in_seconds(600)
                .validate(HookState::Disabled)
                .validate_timeout_in_seconds(60)
                .build(),
        )
        .microvm_hooks(
            MicrovmHooks::builder()
                .run(HookState::Enabled)
                .run_timeout_in_seconds(60)
                .resume(HookState::Enabled)
                .resume_timeout_in_seconds(60)
                .suspend(HookState::Disabled)
                .suspend_timeout_in_seconds(60)
                .terminate(HookState::Disabled)
                .terminate_timeout_in_seconds(60)
                .build(),
        )
        .build();
    let created = aws
        .microvm
        .create_microvm_image()
        .name(name)
        .base_image_arn(&cfg.base_image_arn)
        .build_role_arn(&cfg.build_role_arn)
        .code_artifact(CodeArtifact::Uri(code_artifact_uri))
        .hooks(hooks)
        .client_token(client_token(name))
        .send()
        .await
        .context("create_microvm_image")?;
    let image_arn = created.image_arn().to_string();
    tracing::info!(target: "hellbox::build", "image creating: {image_arn} (state {})", created.state().as_str());

    state.upsert(name, |c| {
        c.image_arn = Some(image_arn.clone());
        c.image_version = created.latest_active_image_version().map(str::to_string);
        c.state = Some(created.state().as_str().to_string());
    })?;

    let image_id = image_arn.clone();
    let final_state = poll_until(
        &format!("image {name}"),
        &["CREATED", "CREATE_FAILED"],
        PollOpts::default(),
        || {
            let aws = &aws;
            let image_id = image_id.clone();
            async move {
                let out = aws
                    .microvm
                    .get_microvm_image()
                    .image_identifier(&image_id)
                    .send()
                    .await
                    .context("get_microvm_image")?;
                Ok(out.state().as_str().to_string())
            }
        },
    )
    .await?;

    let active_version = aws
        .microvm
        .get_microvm_image()
        .image_identifier(&image_arn)
        .send()
        .await
        .ok()
        .and_then(|o| o.latest_active_image_version().map(str::to_string));

    state.upsert(name, |c| {
        c.state = Some(final_state.clone());
        if active_version.is_some() {
            c.image_version = active_version.clone();
        }
    })?;

    if final_state == "CREATE_FAILED" {
        bail!("image build for '{name}' failed (state CREATE_FAILED)");
    }

    println!("built '{name}': image {image_arn} CREATED");
    Ok(())
}

fn capsule_dir(override_path: Option<&str>) -> Result<PathBuf> {
    let dir = match override_path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir()?.join("capsule"),
    };
    if !dir.is_dir() {
        bail!(
            "no capsule dir at {} — run `hellbox build` from the Hellbox repo root, \
             or pass --capsule-dir <PATH>",
            dir.display()
        );
    }
    Ok(dir)
}

fn client_token(name: &str) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("hellbox-build-{name}-{secs}")
}

#[derive(Debug)]
struct ContextZip {
    path: PathBuf,
    dir: PathBuf,
}

impl ContextZip {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ContextZip {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

fn zip_context(dir: &Path) -> Result<ContextZip> {
    let (context_zip, file) = create_context_zip_file()?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o755);

    // follow_links(false) (the default) yields symlinks as symlink entries rather
    // than recursing/following them, so path_is_symlink() can catch them below.
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        let rel = path.strip_prefix(dir).context("relativizing zip entry")?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        // SECURITY: fail closed on symlinks. std::fs::read() follows a file symlink,
        // so a symlink inside the capsule (e.g. -> ~/.aws/credentials) would package
        // the TARGET's bytes into the zip and upload them to S3 in the build context.
        // The capsule legitimately contains none, so reject rather than follow.
        if entry.path_is_symlink() {
            bail!(
                "refusing to build: capsule contains a symlink ({}). Symlinks are not \
                 packaged (their targets could exfiltrate local files into the cloud \
                 build context). Remove it or replace it with a real file.",
                rel.display()
            );
        }
        let name = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");

        if path.is_dir() {
            zip.add_directory(format!("{name}/"), opts)
                .with_context(|| format!("adding dir {name}"))?;
        } else {
            validate_zip_file_entry(path, rel)?;
            zip.start_file(&name, opts)
                .with_context(|| format!("adding file {name}"))?;
            let data =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            zip.write_all(&data)
                .with_context(|| format!("writing {name} into zip"))?;
        }
    }
    zip.finish().context("finalizing zip")?;
    Ok(context_zip)
}

fn create_context_zip_file() -> Result<(ContextZip, std::fs::File)> {
    let root = std::env::temp_dir();
    for _ in 0..100 {
        let dir = root.join(format!("hellbox-context-{}", random_context_suffix()?));
        match create_private_dir(&dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err).with_context(|| format!("creating {}", dir.display()));
            }
        }

        let path = dir.join("context.zip");
        match create_private_file(&path) {
            Ok(file) => return Ok((ContextZip { path, dir }, file)),
            Err(err) => {
                let _ = std::fs::remove_dir(&dir);
                return Err(err).with_context(|| format!("creating {}", path.display()));
            }
        }
    }

    bail!("could not create a unique temporary context zip path")
}

fn random_context_suffix() -> Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).context("generating random context zip path")?;
    Ok(format!("{:032x}", u128::from_le_bytes(bytes)))
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    std::fs::DirBuilder::new().mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir(dir)
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

fn validate_zip_file_entry(path: &Path, rel: &Path) -> Result<()> {
    let metadata =
        std::fs::symlink_metadata(path).with_context(|| format!("checking {}", path.display()))?;
    if !metadata.file_type().is_file() {
        bail!(
            "refusing to build: capsule contains a non-regular file ({}). Only regular \
             files and directories are packaged; remove sockets, FIFOs, devices, or other \
             special files.",
            rel.display()
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if metadata.nlink() > 1 {
            bail!(
                "refusing to build: capsule contains a hardlink ({}). Hardlinks are not \
                 packaged because they can alias files outside the capsule and exfiltrate \
                 local bytes into the cloud build context.",
                rel.display()
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unique scratch dir under the system temp, cleaned up on drop. Avoids a
    // tempfile dev-dependency for one test.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            #[cfg(unix)]
            let root = PathBuf::from("/tmp");
            #[cfg(not(unix))]
            let root = std::env::temp_dir();
            let p = root.join(format!(
                "hellbox-buildtest-{tag}-{}-{:p}",
                std::process::id(),
                &tag as *const _
            ));
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn zip_context_packages_normal_files() {
        let s = Scratch::new("normal");
        std::fs::write(s.0.join("a.txt"), b"hello").unwrap();
        std::fs::create_dir(s.0.join("sub")).unwrap();
        std::fs::write(s.0.join("sub/b.txt"), b"world").unwrap();
        let zip = zip_context(&s.0).expect("normal capsule should package");
        assert!(zip.path().exists(), "zip produced");
    }

    #[test]
    fn zip_context_uses_random_exclusive_path() {
        let s = Scratch::new("random");
        std::fs::write(s.0.join("a.txt"), b"hello").unwrap();

        let predictable =
            std::env::temp_dir().join(format!("hellbox-context-{}.zip", std::process::id()));
        let _ = std::fs::remove_file(&predictable);
        std::fs::write(&predictable, b"do-not-clobber").unwrap();

        let zip_one = zip_context(&s.0).expect("first zip should package");
        let zip_two = zip_context(&s.0).expect("second zip should package");

        assert_ne!(zip_one.path(), predictable.as_path());
        assert_ne!(zip_one.path(), zip_two.path());
        assert_eq!(std::fs::read(&predictable).unwrap(), b"do-not-clobber");

        let _ = std::fs::remove_file(predictable);
    }

    #[test]
    fn zip_context_removes_temp_artifact_on_drop() {
        let s = Scratch::new("cleanup");
        std::fs::write(s.0.join("a.txt"), b"hello").unwrap();

        let (zip_path, zip_dir) = {
            let zip = zip_context(&s.0).expect("zip should package");
            let zip_path = zip.path().to_path_buf();
            let zip_dir = zip_path.parent().unwrap().to_path_buf();
            assert!(zip_path.exists(), "zip exists before drop");
            assert!(zip_dir.is_dir(), "private context dir exists before drop");
            (zip_path, zip_dir)
        };

        assert!(!zip_path.exists(), "zip removed on drop");
        assert!(!zip_dir.exists(), "private context dir removed on drop");
    }

    #[cfg(unix)]
    #[test]
    fn zip_context_uses_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let s = Scratch::new("permissions");
        std::fs::write(s.0.join("a.txt"), b"hello").unwrap();

        let zip = zip_context(&s.0).expect("zip should package");
        let dir_mode = std::fs::metadata(zip.path().parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let file_mode = std::fs::metadata(zip.path()).unwrap().permissions().mode() & 0o777;

        assert_eq!(dir_mode, 0o700, "context dir is private");
        assert_eq!(file_mode, 0o600, "context zip is private");
    }

    #[cfg(unix)]
    #[test]
    fn zip_context_rejects_symlinks() {
        let s = Scratch::new("symlink");
        // A secret outside the capsule the symlink would exfiltrate.
        let secret = s.0.join("outside_secret");
        std::fs::write(&secret, b"AWS_SECRET").unwrap();
        let cap = s.0.join("capsule");
        std::fs::create_dir(&cap).unwrap();
        std::fs::write(cap.join("real.txt"), b"ok").unwrap();
        std::os::unix::fs::symlink(&secret, cap.join("link.txt")).unwrap();

        let err = zip_context(&cap).expect_err("symlink must be rejected");
        assert!(
            err.to_string().contains("symlink"),
            "error names the symlink: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn zip_context_rejects_hardlinks() {
        let s = Scratch::new("hardlink");
        let secret = s.0.join("outside_secret");
        std::fs::write(&secret, b"AWS_SECRET").unwrap();
        let cap = s.0.join("capsule");
        std::fs::create_dir(&cap).unwrap();
        std::fs::write(cap.join("real.txt"), b"ok").unwrap();
        std::fs::hard_link(&secret, cap.join("hardlink.txt")).unwrap();

        let err = zip_context(&cap).expect_err("hardlink must be rejected");
        assert!(
            err.to_string().contains("hardlink"),
            "error names the hardlink: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn zip_context_rejects_non_regular_entries() {
        let s = Scratch::new("socket");
        let cap = s.0.join("capsule");
        std::fs::create_dir(&cap).unwrap();
        std::fs::write(cap.join("real.txt"), b"ok").unwrap();
        let socket = cap.join("control.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();

        let err = zip_context(&cap).expect_err("non-regular entry must be rejected");
        assert!(
            err.to_string().contains("non-regular"),
            "error names the non-regular entry: {err}"
        );
    }
}
