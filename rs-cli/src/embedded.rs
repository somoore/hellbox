//! Capsule build context and CloudFormation template baked into the binary,
//! so `hellbox deploy` works from a package-manager install with no repo clone.
//!
//! Files are listed explicitly (no directory-embedding macro) so a stray local
//! file — e.g. a retail WAD staged under capsule/app/ — can never ride along
//! into a released binary. scripts/check-embedded-capsule.sh keeps this list in
//! sync with `git ls-files capsule/` in CI.

/// The prerequisites stack (S3 artifact bucket + build/execution IAM roles).
pub const STACK_TEMPLATE: &str = include_str!("../../deploy/doom.yaml");

/// Every tracked file of the capsule build context, relative to capsule/.
pub const CAPSULE_FILES: &[(&str, &[u8])] = &[
    ("Dockerfile", include_bytes!("../../capsule/Dockerfile")),
    (
        "app/.gitignore",
        include_bytes!("../../capsule/app/.gitignore"),
    ),
    (
        "app/README.md",
        include_bytes!("../../capsule/app/README.md"),
    ),
    ("index.html", include_bytes!("../../capsule/index.html")),
    (
        "requirements.txt",
        include_bytes!("../../capsule/requirements.txt"),
    ),
    (
        "rootfs/opt/capsule/audio_ws.py",
        include_bytes!("../../capsule/rootfs/opt/capsule/audio_ws.py"),
    ),
    (
        "rootfs/opt/capsule/focus.py",
        include_bytes!("../../capsule/rootfs/opt/capsule/focus.py"),
    ),
    (
        "rootfs/opt/capsule/input_ws.py",
        include_bytes!("../../capsule/rootfs/opt/capsule/input_ws.py"),
    ),
    (
        "rootfs/opt/capsule/run_app.sh",
        include_bytes!("../../capsule/rootfs/opt/capsule/run_app.sh"),
    ),
    (
        "rootfs/opt/capsule/start.sh",
        include_bytes!("../../capsule/rootfs/opt/capsule/start.sh"),
    ),
    (
        "rootfs/opt/capsule/video_ws.py",
        include_bytes!("../../capsule/rootfs/opt/capsule/video_ws.py"),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_capsule_is_nonempty_and_wad_free() {
        assert!(
            CAPSULE_FILES.len() >= 10,
            "capsule file list looks truncated"
        );
        for (path, bytes) in CAPSULE_FILES {
            assert!(!bytes.is_empty(), "{path} embedded empty");
            assert!(
                !path.to_ascii_lowercase().ends_with(".wad"),
                "game data must never be embedded ({path})"
            );
        }
        assert!(STACK_TEMPLATE.contains("ArtifactBucket"));
    }
}
