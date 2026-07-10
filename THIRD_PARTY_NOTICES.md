# Third-party notices

This file summarizes third-party software and assets used by Hellbox. It is a practical
notice index, not legal advice and not a substitute for reviewing the upstream license texts.

Hellbox's original source code is licensed under Apache-2.0. Third-party components remain
under their own licenses.

## Summary

| Component | Where used | Version / source | License posture | Notes |
|---|---|---|---|---|
| Chocolate Doom | Compiled in CI, shipped in the prebuilt tarball, installed into the MicroVM image | `3.0.1`, from `chocolate-doom/chocolate-doom` | GPLv2 | Provide license text and corresponding source when distributing the prebuilt tarball or any image artifact that includes it. |
| SDL2 | Compiled in CI, shipped in the prebuilt tarball, installed into the MicroVM image | `2.30.9` | zlib-style permissive | Preserve upstream license notice. |
| SDL2_mixer | Compiled in CI, shipped in the prebuilt tarball, installed into the MicroVM image | `2.8.0` | zlib-style permissive | Preserve upstream license notice. |
| SDL2_net | Compiled in CI, shipped in the prebuilt tarball, installed into the MicroVM image | `2.2.0` | zlib-style permissive | Preserve upstream license notice. |
| BtbN ffmpeg GPL build | Downloaded into the MicroVM image | `ffmpeg-n8.1.2-linuxarm64-gpl-8.1.tar.xz` | GPLv3 | This pinned artifact is the GPL build. Provide license text and corresponding source/offers when distributing image artifacts that include it. |
| websockify | Python wheel in the generated MicroVM image | `0.13.0` | LGPLv3 | Preserve license text and source-availability path for distributed image artifacts. |
| python-xlib | Python wheel in the generated MicroVM image | `0.33` | LGPLv2 or later | Preserve license text and source-availability path for distributed image artifacts. |
| noVNC | Downloaded into the generated MicroVM image | `1.5.0` | MPL-2.0 core; BSD/OFL/CC-licensed supporting files | Preserve MPL source/license notices and supporting asset notices. |
| Shareware `DOOM1.WAD` | Downloaded at image build time unless the user supplies a WAD | Pinned file from `nneonneo/universal-doom` | Third-party game data | Not Apache-2.0. Do not distribute retail DOOM WADs or imply user-supplied WADs are covered by this repo's license. |
| Rust crates | Linked into the prebuilt `hellbox` CLI | Locked in `rs-cli/Cargo.lock` | Mostly permissive; `option-ext` is MPL-2.0 | Checked by `cargo-deny`; see `deny.toml`. |
| Python wheels | Installed into the generated MicroVM image | Pinned in `capsule/requirements.txt` | Mixed permissive/MPL/LGPL package licenses | Preserve package license texts when distributing image artifacts. |
| Amazon Linux / MicroVM base image and DNF packages | Base and OS packages in the generated MicroVM image | `public.ecr.aws/lambda/microvms:al2023-minimal` plus packages installed in `capsule/Dockerfile` | Mixed OS package licenses | For production image distribution, generate a full package bill of materials from the built image. |

## Source and license pointers

- Chocolate Doom: <https://github.com/chocolate-doom/chocolate-doom>
- BtbN FFmpeg Builds: <https://github.com/BtbN/FFmpeg-Builds>
- FFmpeg legal/license information: <https://ffmpeg.org/legal.html>
- websockify: <https://github.com/novnc/websockify>
- python-xlib: <https://github.com/python-xlib/python-xlib>
- noVNC: <https://github.com/novnc/noVNC>
- SDL: <https://github.com/libsdl-org/SDL>
- SDL_mixer: <https://github.com/libsdl-org/SDL_mixer>
- SDL_net: <https://github.com/libsdl-org/SDL_net>
- Shareware WAD source used by the build: <https://github.com/nneonneo/universal-doom>
- Prebuilt SDL + Chocolate Doom tarball (compiled by `.github/workflows/capsule-prebuilt.yml`): <https://github.com/somoore/hellbox/releases/tag/capsule-prebuilt-1>

## Distribution notes

For source-only releases of this repo, the root [LICENSE](LICENSE) covers Hellbox's original
code. The third-party components above are fetched or built later and remain under their own
licenses.

For prebuilt `hellbox` CLI releases, include Rust dependency notices if you publish formal binary
distributions. The CLI itself links only permissive/MPL Rust crates; it contains no GPL code.

The **prebuilt SDL + Chocolate Doom tarball** (`capsule-prebuilt-1` release) is a distributed
binary artifact that includes GPLv2 Chocolate Doom and zlib-licensed SDL. Distributing it
carries the GPLv2 corresponding-source obligation; the source is the pinned upstream tags
listed above, and the exact build recipe is `.github/workflows/capsule-prebuilt.yml`.

For distributed MicroVM/container/image artifacts, treat the image as a third-party software
bundle. Include applicable license texts and notices, and provide corresponding source or a
written/source offer where required by GPL, LGPL, and MPL components.
