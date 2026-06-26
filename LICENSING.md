# Licensing recommendation

This is practical repo hygiene, not legal advice.

## Recommendation

Use **Apache License 2.0** for LambdaDoom's own source code.

The root [LICENSE](LICENSE) file is the operative license for LambdaDoom's original code. Keep
`rs-cli/Cargo.toml` aligned with it:

```toml
license = "Apache-2.0"
```

Do not relicense the whole repository as GPL just because the generated MicroVM image builds or
contains GPL components. LambdaDoom's own Rust CLI, scripts, docs, and deployment code are not
derived from Chocolate Doom or ffmpeg; they build, configure, and run those programs as separate
third-party components.

## Dependency read

The Rust CLI dependency graph does not force a copyleft root license:

| Area | License impact |
|---|---|
| Rust CLI dependencies | Mostly MIT, Apache-2.0, BSD, ISC, Zlib, Unicode, or similarly permissive licenses. |
| `option-ext` via `directories` | MPL-2.0. File-level copyleft for that crate, not a requirement to license LambdaDoom as MPL. |
| `r-efi` | Lists `MIT OR Apache-2.0 OR LGPL-2.1-or-later`; use the permissive Apache-2.0 or MIT option. |

The capsule image has stronger third-party obligations, but those obligations apply to the
components in the image and to any distributed image artifacts, not to the root license for
LambdaDoom's original source code:

| Component | License posture |
|---|---|
| Chocolate Doom | GPLv2. |
| BtbN ffmpeg `*-gpl-*` build | GPLv3. |
| websockify | LGPLv3. |
| python-xlib | LGPLv2 or later. |
| noVNC | MPL-2.0 core, with BSD/OFL/CC-licensed supporting files. |
| SDL2, SDL2_mixer, SDL2_net | zlib-style permissive licenses. |
| Python packages in `capsule/requirements.txt` | Mixed permissive/MPL/LGPL package notices; preserve their license texts when distributing an image. |

## Practical compliance stance

For source releases of this repo:

- Keep [LICENSE](LICENSE) as Apache-2.0.
- Keep [LEGAL.md](LEGAL.md) for third-party notices, trademark disclaimers, WAD language, and AWS
  charge language.
- Keep generated game assets and WAD files out of git.

For prebuilt `ldoom` CLI releases:

- Apache-2.0 is still appropriate for LambdaDoom's own CLI binary.
- Include dependency notices for bundled Rust crates if you publish formal binary distributions.

For distributed MicroVM/container/image artifacts:

- Treat the image as a third-party software bundle.
- Include license texts and notices for Chocolate Doom, ffmpeg, websockify, python-xlib, noVNC,
  SDL2, SDL2_mixer, SDL2_net, Python wheels, and OS packages.
- Provide corresponding source or a clear written/source offer where required by GPL, LGPL, and
  MPL components.
- Keep the DOOM shareware WAD and any user-provided IWAD/PWAD assets covered by their own terms;
  do not imply they are Apache-2.0.

## Bottom line

Apache-2.0 is the right default for LambdaDoom's original code. The legal work this repo needs is
accurate third-party attribution and source-availability hygiene for generated images, not a
forced switch to GPL or MIT for the whole project.
