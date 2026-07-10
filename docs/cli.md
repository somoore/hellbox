# The `hellbox` CLI

One Rust binary with two jobs: **lifecycle driver** (SigV4 calls that create, launch,
suspend, resume, and destroy the MicroVM) and **stream proxy** (a loopback reverse-proxy on
`127.0.0.1:6080` that injects the `X-aws-proxy-auth` token browsers cannot set). The
CloudFormation prerequisites template and the capsule build context are embedded in the
binary, so nothing here requires a repo clone.

All AWS calls use the SDK's adaptive retry mode — jittered exponential backoff plus a
client-side rate limiter that responds to throttling — so polling loops stay polite.

## Install

| Channel | Install | Update | Remove |
|---|---|---|---|
| Homebrew (macOS/Linux) | `brew install somoore/hellbox/hellbox` | `brew upgrade hellbox` | `brew uninstall hellbox` |
| GitHub Releases | [download](https://github.com/somoore/hellbox/releases) — verify with the `.sha256` sidecar and `gh attestation verify` | re-download | delete the binary |
| Source | `cd rs-cli && make release` | `git pull` + rebuild | — |

Every prebuilt binary carries a GitHub build-provenance attestation bound to the release
workflow and tag. The Homebrew tap re-pins its formula only after verifying those
attestations; Homebrew then enforces the pinned SHA256 at install time.

## Commands

`--name` defaults to `doom` everywhere, so the common case needs no flags.

### Play

```
hellbox                         # bare command = `hellbox play`
hellbox play                    # get DOOM on screen, whatever it takes
```

Reconciles local state with AWS and recovers from wherever things stand: a RUNNING
capsule opens immediately; a SUSPENDED one opens the paused page (clicking **Resume** is
your deliberate restart of billing); a terminated or expired one (suspended MicroVMs only
persist ~8 hours) is relaunched from the image; no image at all points you to
`hellbox deploy`. Every path ends with the proxy verified and a tab open.

### Deployment

```
hellbox deploy                  # stack -> config -> build image -> launch -> verify -> open
hellbox deploy -r us-west-2     # deploy to any region with Lambda MicroVMs
hellbox deploy -p KEY=VALUE     # override CloudFormation stack parameters (repeatable)
hellbox deploy edit             # customize the stack template in $EDITOR
hellbox destroy --yes           # remove microvm, image, bucket contents, stack, local state
```

- `deploy` is idempotent: an existing stack is updated (or left alone if unchanged), and a
  rerun after `destroy` rides through the service's asynchronous image deletion.
- `deploy` only succeeds after **end-to-end verification**: the page answers on loopback
  and the video, audio, and input WebSockets each complete a real handshake through the
  proxy into the VM.
- `deploy edit` writes the built-in template to `~/.hellbox/stack.yaml` and opens your
  editor; later deploys use that copy. Delete the file to return to the built-in template.
- `destroy` first prints the exact resource list (microvm, image, bucket, stack, local
  files — each with why) and requires typing `destroy` to proceed; `--yes` skips the
  prompt for scripts. Ownership guardrails hold either way: the stack is only deleted if
  it carries the Hellbox template markers, and the bucket is only emptied if it is the
  one that verified stack reports as its own output — any mismatch aborts everything.
  Nothing Hellbox didn't create is ever touched. Locally it removes `config.toml` and
  `state.json`, leaving `~/.hellbox/` itself in place.

### Lifecycle

```
hellbox build                   # zip capsule -> S3 -> build the MicroVM image in the cloud
hellbox up                      # launch a MicroVM from the image (PENDING -> RUNNING)
hellbox open                    # mint a token, start the proxy, open the browser tab
hellbox open --no-open          # start the proxy, print the URL, don't launch a browser
hellbox suspend                 # freeze the machine (compute billing stops)
hellbox resume                  # thaw on the exact frame
hellbox down                    # terminate the MicroVM (keeps the image for a fast `up`)
hellbox rm                      # terminate and delete the image
hellbox ps [--refresh]          # list capsules; --refresh reconciles against AWS
```

- `build` uses `./capsule` when run from a clone, the embedded capsule otherwise, or
  `--capsule-dir <path>` for anything else (e.g. staging your own WAD — see
  [capsule/app/](../capsule/app/README.md)).
- `open` runs in the foreground; `Ctrl-C` stops the proxy (the MicroVM keeps running and
  auto-suspends after ~5 idle minutes).
- The in-page **Suspend/Resume** panel keeps working while the machine is frozen — those
  calls go through the proxy's local control endpoints, not the (dead) stream.

### Settings

```
hellbox config show
hellbox config set display h264|vnc     # WebCodecs stream (default) or noVNC fallback
hellbox config set idle_suspend_minutes N   # proxy-side auto-suspend when no viewer
hellbox config unset <key>
```

## Configuration files

Everything lives under `~/.hellbox/` (override with `HELLBOX_HOME`):

| File | What it is |
|---|---|
| `config.toml` | Region, artifact bucket, role ARNs, ports, display mode — written by `deploy` |
| `state.json` | Known capsules: image ARN, microvm id, endpoint, last state |
| `stack.yaml` | Optional customized CloudFormation template (`hellbox deploy edit`) |
| `bin/` | Binary cache used by `deploy.sh` |

Environment variables (mostly for `deploy.sh`; the CLI takes flags instead):

| Variable | Default | What it does |
|---|---|---|
| `AWS_REGION` | `us-east-1` | Region (CLI: `hellbox deploy -r` wins) |
| `HELLBOX_STACK` | `Hellbox` | CloudFormation stack name |
| `HELLBOX_NAME` | `doom` | Capsule name for `deploy.sh` |
| `HELLBOX_REPO` | `somoore/hellbox` | Release repo `deploy.sh` downloads from |
| `HELLBOX_VERSION` | latest | Pin `deploy.sh` to a release tag |
| `HELLBOX_SKIP_ATTESTATION` | `0` | `1` skips `gh attestation verify` (pinned versions only) |
| `HELLBOX_HOME` | `~/.hellbox` | Config/state/cache directory |
| `HELLBOX_BIN` | none | Use a specific binary in `deploy.sh` |

## Troubleshooting

**`image 'doom' already exists`** — you have a built image. `hellbox up` launches it as-is;
to rebuild from a changed capsule run `hellbox rm` first. If this appears right after a
`destroy`/`rm`, the service is still completing the asynchronous delete — `build` retries
through that window automatically for up to ~3 minutes.

**Stream is frozen / buttons do nothing** — the MicroVM idle-suspended (by design, ~5 idle
minutes). Click **Resume** in the page panel, or `hellbox resume`. If the page itself is
stale, reload the tab.

**`end-to-end verification failed (…)`** — the capsule is up but a stream service inside
it isn't answering. `hellbox suspend && hellbox resume` restarts the streams' connections;
if one channel stays dead across a rebuild, check the build logs in CloudWatch
(`/aws/lambda-microvms/<name>`) — the ready gate logs each service's status every 2s
during the image build.

**Keyboard/mouse do nothing but video plays** — click the game canvas once to focus it.
If that never helps, the image predates the input-service supervision fix — rebuild:
`hellbox rm && hellbox deploy`.

**Proxy port busy** — something else owns `127.0.0.1:6080`; the proxy falls back to an
ephemeral port and prints the URL it actually bound.

**`could not mint auth token (capsule may be suspended)`** — informational: `open` starts
a control-only page from which Resume works; the token is re-minted on resume.

**Verbose logs** — `RUST_LOG=hellbox=debug hellbox <cmd>` (the proxy logs upstream
connection failures at `warn`, per-channel verification attempts at `debug`).
