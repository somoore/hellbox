# LambdaDoom Security Review Findings

Review date: 2026-06-26  
Reviewed commit: `ed9d0545cfee83ddf631ffe24a35aae1b92e2edb`  
Scope: repository-wide review of Rust CLI/proxy code, CloudFormation, capsule Dockerfile/scripts/Python services, deployment scripts, GitHub workflows, and architecture/security docs.

Notes:

- This was a parent-agent review of all 30 source/config files selected by the scan inventory. The session policy did not allow delegated subagents, so this is not claimed as a delegated exhaustive Codex Security scan.
- Validation run: `cargo test` passed 18/18 tests in `rs-cli`.
- Validation run: `cargo audit` completed against 366 Rust dependencies and reported no RustSec vulnerabilities.
- Validation run: `shellcheck` passed for shell scripts reviewed.
- Validation run: Python syntax compilation passed for capsule Python services.
- `cfn-lint` was not installed in this environment, so CloudFormation linting was not run.

Remediation status: all findings below have been fixed in the working tree.

- Finding 1 and 4: fixed in `rs-cli/src/proxy.rs` with loopback/session guards for data-plane forwarding, expected forwarded-path checks, and upstream stripping of `ldoom_control`.
- Finding 2: fixed in `deploy.sh` by requiring GitHub artifact attestation for downloaded/cached release binaries unless an explicit pinned-version bypass is set.
- Finding 3 and 6: fixed in `deploy/doom.yaml` with `aws:SourceAccount` trust conditions, constrained service-principal input, and a TLS-only S3 bucket policy.
- Finding 5: fixed in `capsule/Dockerfile` and `capsule/requirements.txt` with hash-pinned Python package installs.

## Medium

### 1. Local proxy forwards data-plane WebSockets/HTTP without an Origin or session guard

**What was found**

The local loopback proxy hardens only `/__lambdadoom/*` control endpoints with Host, Origin, cookie, and method checks. Non-control traffic is routed directly to `handle_ws` or `handle_http`, and those paths inject the MicroVM JWE token before forwarding to the AWS data-plane endpoint.

**Where it was found**

- `rs-cli/src/proxy.rs:223` routes non-control WebSocket and HTTP requests without calling the control endpoint guard.
- `rs-cli/src/proxy.rs:401` builds an upstream WSS request and injects `x-aws-proxy-auth`.
- `rs-cli/src/proxy.rs:243` forwards plain HTTP and injects auth/port headers via `build_upstream_headers`.
- `rs-cli/src/proxy.rs:155` maps `/ldoom/audio`, `/ldoom/video`, and `/ldoom/input` to internal ports.
- `capsule/index.html:236`, `capsule/index.html:260`, and `capsule/index.html:348` show the expected browser WebSocket paths.

**Why this is an issue**

WebSockets are not protected by the normal browser same-origin policy in the way XHR/fetch responses are. A malicious website visited while `ldoom open` is running can attempt connections to `ws://127.0.0.1:6080/ldoom/video`, `/ldoom/audio`, `/ldoom/input`, or `/websockify`. Because the proxy does not validate Origin or require the local session secret for data-plane streams, it will inject the JWE token and bridge those connections to the MicroVM.

In this repo's current DOOM-only threat model, this does not expose AWS credentials and the AWS control endpoints are separately protected. The realistic impact is still security-relevant: cross-site access to the live game pixels/audio, cross-site input injection, keeping streams active to defeat idle suspend, and possible wake-on-traffic cost impact while the token remains valid.

**How to fix**

- Apply a data-plane guard before `handle_ws` and `handle_http`, not only before `handle_control`.
- Require `Host` to be loopback and reject any non-loopback `Origin` on all WebSocket upgrades and forwarded HTTP requests.
- Require the per-session secret for stream endpoints, for example by setting `ldoom_control` with `Path=/__lambdadoom` for control plus a separate HttpOnly stream cookie/path token for `/ldoom/*` and `/websockify`, or by using an unguessable per-session URL prefix.
- Restrict forwarded paths and methods to the expected display/audio/video/input assets instead of forwarding arbitrary loopback paths to the MicroVM.
- Add regression tests showing a foreign `Origin` cannot open `/ldoom/input`, `/ldoom/video`, or `/websockify`.

### 2. `deploy.sh` can fall back to same-release SHA256 without mandatory provenance verification

**What was found**

`deploy.sh` downloads a prebuilt `ldoom` binary and its `.sha256` sidecar from the selected GitHub release. It verifies the checksum, and verifies GitHub artifact attestation only when `gh` is installed and `LAMBDADOOM_SKIP_ATTESTATION` is not set.

**Where it was found**

- `deploy.sh:64` resolves `latest` dynamically.
- `deploy.sh:76` downloads release assets.
- `deploy.sh:108` downloads the binary and same-release checksum sidecar.
- `deploy.sh:110` verifies the binary against that sidecar.
- `deploy.sh:112` verifies attestation only when `gh` is available; `deploy.sh:121` only warns when `gh` is absent.
- `README.md:44` documents `deploy.sh` as the quickstart path that downloads the prebuilt CLI.

**Why this is an issue**

The downloaded `ldoom` binary runs locally with the user's AWS credential chain and drives CloudFormation, S3 uploads, MicroVM image creation, and MicroVM lifecycle calls. A SHA256 file fetched from the same release asset set does not protect against a compromised release, compromised GitHub account, or malicious asset substitution in that same trust domain. The existing attestation support is good, but it is optional in the common "gh not installed" path.

**How to fix**

- Fail closed when `gh attestation verify` is unavailable, unless the user explicitly chooses a documented insecure mode.
- Avoid `latest` for the default install path, or pair it with a signed manifest committed to the repository.
- Prefer one of these default-safe flows: build from source, verify Sigstore/GitHub attestation, or verify a pinned expected checksum shipped out-of-band from the release assets.
- Keep `LAMBDADOOM_SKIP_ATTESTATION=1`, but make the warning much stronger and require a pinned `LAMBDADOOM_VERSION` when it is used.

### 3. IAM role trust policies lack confused-deputy conditions

**What was found**

The CloudFormation template creates build and execution roles that trust the `lambda.amazonaws.com` service principal, but the trust policies do not add `aws:SourceAccount` or `aws:SourceArn` conditions. The service principal is also parameterized.

**Where it was found**

- `deploy/doom.yaml:15` defines `BuildServicePrincipal` as an overrideable parameter.
- `deploy/doom.yaml:49` defines the build role trust policy.
- `deploy/doom.yaml:63` grants the build role `s3:GetObject` on the artifact bucket and CloudWatch Logs writes.
- `deploy/doom.yaml:79` defines the execution role trust policy.

**Why this is an issue**

Service-assumed IAM roles should usually bind the service assumption to the expected account and resource context to reduce confused-deputy risk. The current build role has limited permissions, but it can read all objects in the build artifact bucket. The execution role has no permissions today, but the template comments explicitly invite users to add runtime permissions later; if they do, the broad trust policy becomes more important.

**How to fix**

- Add a trust-policy condition at minimum:

```yaml
Condition:
  StringEquals:
    aws:SourceAccount: !Ref AWS::AccountId
```

- If Lambda MicroVMs supports a stable SourceArn value for image builds and MicroVM runs, also add an `ArnLike`/`ArnEquals` `aws:SourceArn` condition for the expected MicroVM or MicroVM-image ARN pattern.
- Remove the service principal parameter or constrain it with `AllowedValues` so accidental deployment with an unsafe principal is not possible.
- Keep the build and execution role trust policies separate if AWS exposes different SourceArn shapes for image build versus runtime execution.

## Low

### 4. Local control cookie is forwarded to the MicroVM data plane

**What was found**

The proxy sets the `ldoom_control` HttpOnly cookie for local control endpoints. The generic upstream header builder copies inbound headers to the MicroVM except hop-by-hop and Host headers, so `Cookie` is forwarded to the capsule services.

**Where it was found**

- `rs-cli/src/proxy.rs:387` sets `ldoom_control` with `Path=/`.
- `rs-cli/src/proxy.rs:640` sets the same cookie on the control-only page.
- `rs-cli/src/proxy.rs:555` copies inbound headers to the upstream request.
- `rs-cli/src/proxy.rs:557` excludes hop-by-hop and Host headers, but not Cookie.

**Why this is an issue**

The cookie is intended to protect proxy-local AWS control endpoints, not to be shared with the MicroVM display server. Forwarding it weakens the boundary between local proxy control state and capsule web content/logs. In the current DOOM-only capsule this is limited, but it becomes more significant if the capsule content is changed or generalized to arbitrary apps.

**How to fix**

- Set the control cookie with `Path=/__lambdadoom` so it is only sent to control endpoints.
- Strip `Cookie` entirely, or at least remove `ldoom_control`, before forwarding HTTP or WebSocket upgrade requests upstream.
- Add a unit test for `build_upstream_headers` proving `ldoom_control` is not forwarded.

### 5. Python packages in the capsule build are version-pinned but not hash-pinned

**What was found**

The Dockerfile installs Python packages from PyPI by exact version, but without `--require-hashes` or a locked requirements file.

**Where it was found**

- `capsule/Dockerfile:40` runs `pip3 install --no-cache-dir websockify==0.13.0 websockets==16.0 python-xlib==0.33`.

**Why this is an issue**

Most external tarballs and the WAD are SHA256-verified in this Dockerfile, but the PyPI packages are not. A compromised package, account, index response, or dependency resolution path at build time could place malicious code into the capsule image. The build role appears narrow, but the resulting image runs in the user's AWS account with network egress and browser interaction.

**How to fix**

- Use a checked-in `requirements.txt` generated with hashes and install with `pip install --require-hashes -r requirements.txt`.
- Alternatively vendor the wheels or install distro packages from a controlled repository.
- Keep the existing SHA256 pattern for all non-package-manager downloads.

### 6. Artifact bucket does not explicitly deny non-TLS S3 access

**What was found**

The artifact bucket enables block-public-access and SSE-S3 encryption, but it does not include a bucket policy denying requests where `aws:SecureTransport` is false.

**Where it was found**

- `deploy/doom.yaml:25` defines the S3 artifact bucket.
- `deploy/doom.yaml:35` enables AES256 server-side encryption.
- `deploy/doom.yaml:39` enables S3 public access blocks.

**Why this is an issue**

The current Rust SDK and AWS CLI paths use HTTPS, and this bucket holds short-lived build contexts. This is therefore defense-in-depth rather than an active exploit path in the reviewed code. Still, an explicit TLS-only bucket policy is a common baseline control and prevents accidental plaintext S3 API use by future tooling or manual commands.

**How to fix**

Add a bucket policy similar to:

```yaml
ArtifactBucketPolicy:
  Type: AWS::S3::BucketPolicy
  Properties:
    Bucket: !Ref ArtifactBucket
    PolicyDocument:
      Version: '2012-10-17'
      Statement:
        - Sid: DenyInsecureTransport
          Effect: Deny
          Principal: '*'
          Action: 's3:*'
          Resource:
            - !GetAtt ArtifactBucket.Arn
            - !Sub '${ArtifactBucket.Arn}/*'
          Condition:
            Bool:
              aws:SecureTransport: false
```

## Notable Non-Findings

- The `/__lambdadoom/*` AWS control endpoints have meaningful CSRF and DNS-rebinding defenses: loopback Host, loopback Origin when present, HttpOnly per-session cookie, and POST for mutating suspend/resume actions.
- Rust dependency audit found no RustSec vulnerabilities in the current lockfile.
- Capsule downloads for ffmpeg, noVNC, SDL2, SDL2_mixer, SDL2_net, Chocolate Doom, and DOOM1.WAD are pinned and SHA256-verified.
- The CloudFormation execution role has no permissions by default.
- The local proxy binds to `127.0.0.1`, not a LAN interface.
