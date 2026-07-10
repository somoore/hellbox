# The `hellbox` CLI

One Rust binary with two jobs. It drives the lifecycle (create, launch, suspend, resume,
destroy, all SigV4 calls to AWS) and it runs the stream proxy, a loopback reverse-proxy on
`127.0.0.1:6080` that injects the `X-aws-proxy-auth` token browsers cannot set. The
CloudFormation prerequisites template and the capsule build context are baked into the
binary, so nothing here needs a repo clone.

All AWS calls use the SDK's adaptive retry mode: jittered exponential backoff plus a
client-side rate limiter that backs off when AWS signals throttling. The polling loops stay
polite.

## Install

| Channel | Install | Update | Remove |
|---|---|---|---|
| Homebrew (macOS/Linux) | `brew install somoore/hellbox/hellbox` | `brew upgrade hellbox` | `brew uninstall hellbox` |
| GitHub Releases | [download](https://github.com/somoore/hellbox/releases), verify with the `.sha256` sidecar and `gh attestation verify` | re-download | delete the binary |
| Source | `cd rs-cli && make release` | `git pull` and rebuild | |

Every prebuilt binary carries a GitHub build-provenance attestation tied to the release
workflow and tag. The Homebrew tap re-pins its formula only after verifying those
attestations, and Homebrew enforces the pinned SHA256 at install time.

## Commands

`--name` defaults to `doom` everywhere, so the common case needs no flags.

### Play

```
hellbox                         # bare command, same as `hellbox play`
hellbox play                    # get DOOM on screen, whatever it takes
```

This is the "I just want to play" command. It checks what AWS actually has and recovers
from wherever things stand. A RUNNING capsule opens immediately. A SUSPENDED one opens the
paused page, and clicking **Resume** is your deliberate restart of billing. A terminated or
expired one (suspended MicroVMs only persist about 8 hours) gets relaunched from the image.
No image at all points you to `hellbox deploy`. Every path ends with the proxy verified and
a tab open.

### Deployment

```
hellbox deploy                  # stack -> config -> build image -> launch -> verify -> open
hellbox deploy -r us-west-2     # deploy to any region with Lambda MicroVMs
hellbox deploy -p KEY=VALUE     # override CloudFormation stack parameters (repeatable)
hellbox deploy edit             # customize the stack template in $EDITOR
hellbox destroy                 # remove everything, with a typed confirmation first
```

- `deploy` is idempotent. An existing stack gets updated or left alone, an existing image
  is reused (run `hellbox rm` first when you want a rebuild), an already-running machine is
  reconnected instead of duplicated, and a rerun right after `destroy` rides through the
  service's asynchronous image deletion.
- `deploy` only succeeds after end-to-end verification: the page answers on loopback, and
  the video, audio, and input WebSockets each complete a real handshake through the proxy
  into the VM.
- `deploy edit` writes the built-in template to `~/.hellbox/stack.yaml` and opens your
  editor. Later deploys use that copy. Delete the file to go back to the built-in template.
- `destroy` first prints the exact resources it will remove (microvm, image, bucket, stack,
  local files, each with what it is and why) and requires you to type `destroy`. Scripts
  can pass `--yes` to skip the prompt. The ownership guardrails hold either way: the stack
  is only deleted if it carries the Hellbox template markers, and the bucket is only
  emptied if it is the one that verified stack reports as its own output. Any mismatch
  aborts everything. Nothing Hellbox didn't create is ever touched. Locally it removes
  `config.toml` and `state.json` and leaves `~/.hellbox/` itself in place.

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
  `--capsule-dir <path>` for anything else (for example staging your own WAD, see
  [capsule/app/](../capsule/app/README.md)).
- `open` runs in the foreground. `Ctrl-C` stops the proxy. The MicroVM keeps running and
  auto-suspends after about 5 idle minutes.
- Auth tokens live about 30 minutes. The proxy notices an expired one (upstream 401/403),
  mints a fresh token, and retries transparently, so long sessions and late page reloads
  just keep working.
- The in-page **Suspend/Resume** panel keeps working while the machine is frozen. Those
  calls go through the proxy's local control endpoints, not the (dead) stream.

### Settings

```
hellbox config show
hellbox config set display h264|vnc         # WebCodecs stream (default) or noVNC fallback
hellbox config set idle_suspend_minutes N   # proxy-side auto-suspend when no viewer
hellbox config unset <key>
```

## Credentials

hellbox reads AWS credentials exactly the way the AWS CLI v2 does, through the SDK's
default provider chain: environment variables, `~/.aws/credentials` and `~/.aws/config`
profiles (`AWS_PROFILE` respected), IAM Identity Center / SSO login sessions, and
`credential_process` helpers like Granted's `assume`. If `aws sts get-caller-identity`
works in your shell, hellbox works.

Commands that touch AWS start with an identity check. If credentials are missing or
expired you get a plain explanation and the usual fixes (`aws sso login`, `assume`,
`AWS_PROFILE`) instead of an SDK stack trace. `hellbox deploy` prints the identity it is
about to use and records the account id (and `AWS_PROFILE`, when set) in `config.toml`.
`hellbox play` and `hellbox destroy` compare the current account against that record and
refuse to act on a mismatch, so switching profiles can never point a destroy at the wrong
account.

Region resolution for `deploy`: the `-r` flag, then `AWS_REGION`, then
`AWS_DEFAULT_REGION`, then an existing `config.toml`, then your AWS profile's `region`
setting, then `us-east-1`.

## Configuration files

Everything lives under `~/.hellbox/` (override with `HELLBOX_HOME`). On Windows that is
`C:\Users\<you>\.hellbox`; the path comes from the OS profile API, not a guessed `$HOME`.
Uninstalling the binary (brew, winget, or deleting the exe) leaves this directory alone,
so a reinstall picks up your deployment exactly where you left it. Only `hellbox destroy`
removes `config.toml` and `state.json`, and only after tearing down the AWS resources they
describe.

| File | What it is |
|---|---|
| `config.toml` | Region, artifact bucket, role ARNs, ports, display mode. Written by `deploy`. |
| `state.json` | Known capsules: image ARN, microvm id, endpoint, last state |
| `stack.yaml` | Optional customized CloudFormation template (`hellbox deploy edit`) |
| `bin/` | Binary cache used by `deploy.sh` |

Environment variables (mostly for `deploy.sh`; the CLI takes flags instead):

| Variable | Default | What it does |
|---|---|---|
| `AWS_REGION` | `us-east-1` | Region (`hellbox deploy -r` wins) |
| `HELLBOX_STACK` | `Hellbox` | CloudFormation stack name |
| `HELLBOX_NAME` | `doom` | Capsule name for `deploy.sh` |
| `HELLBOX_REPO` | `somoore/hellbox` | Release repo `deploy.sh` downloads from |
| `HELLBOX_VERSION` | latest | Pin `deploy.sh` to a release tag |
| `HELLBOX_SKIP_ATTESTATION` | `0` | `1` skips `gh attestation verify` (pinned versions only) |
| `HELLBOX_HOME` | `~/.hellbox` | Config, state, and cache directory |
| `HELLBOX_BIN` | none | Use a specific binary in `deploy.sh` |

## Troubleshooting

**`image 'doom' already exists`**. You have a built image. `hellbox up` launches it as-is.
To rebuild from a changed capsule, run `hellbox rm` first. If this shows up right after a
`destroy` or `rm`, the service is still finishing its asynchronous delete, and `build`
retries through that window automatically for up to about 3 minutes.

**Stream is frozen, buttons do nothing**. The MicroVM idle-suspended (by design, about 5
idle minutes). Click **Resume** in the page panel, or run `hellbox resume`. If the page
itself looks stale, reload the tab. Or just run `hellbox` and let it sort things out.

**`end-to-end verification failed (...)`**. The capsule is up but a stream service inside
it isn't answering. `hellbox suspend && hellbox resume` restarts the stream connections.
If one channel stays dead across a rebuild, check the build logs in CloudWatch
(`/aws/lambda-microvms/<name>`). The ready gate logs each service's status every 2 seconds
during the image build.

**Keyboard and mouse do nothing but video plays**. Click the game canvas once to focus it.
If that never helps, the image predates the input-service supervision fix. Rebuild:
`hellbox rm && hellbox deploy`.

**Proxy port busy**. Something else owns `127.0.0.1:6080`. The proxy falls back to an
ephemeral port and prints the URL it actually bound.

**`could not mint auth token (capsule may be suspended)`**. Informational. `open` starts a
control-only page, Resume works from there, and the token gets re-minted on resume.

**Verbose logs**. `RUST_LOG=hellbox=debug hellbox <cmd>`. The proxy logs upstream
connection failures at `warn` and per-channel verification attempts at `debug`.
