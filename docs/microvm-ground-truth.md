# Lambda MicroVMs: verified ground truth

The facts below were verified against the live AWS Lambda MicroVMs service while
building Hellbox. They are the load-bearing details: get one wrong and a build
fails or a request 403s. This is a working reference, not the official docs; check
the AWS docs for anything not listed here.

> **New-service warning:** these details were verified live while building Hellbox. Re-check
> them against AWS docs before using this as a general Lambda MicroVM reference.

## Endpoint and signing

- **Control plane:** SigV4-signed REST. Signing name `lambda`, host
  `https://lambda.<region>.amazonaws.com`, API path prefix `/2025-09-09/`.
- **Data plane (the stream):** a per-MicroVM hostname, `<id>.lambda-microvm.<region>.on.aws`.
- **Ingress is HTTPS / WSS only** (HTTP/1.1, HTTP/2, WebSockets, gRPC, SSE). No raw
  inbound TCP or UDP.

## Authentication

- A data-plane request needs a JWE auth token in the **`X-aws-proxy-auth` header**.
  The same token in a query string returns **403**. Browsers cannot set that header,
  which is why `hellbox open` runs a local header-injecting proxy (see architecture.md).
- Select the internal port with the **`X-aws-proxy-port` header** (or the WebSocket
  subprotocol `lambda-microvms.port.<N>`).
- Mint a token with `CreateMicrovmAuthToken` (`POST .../microvms/{id}/auth-token`):
  body `allowedPorts:[{port}]` + `expirationInMinutes` (<= 60). The response is an
  `authToken` map; use the value at key `X-aws-proxy-auth`.

## Operations

- `CreateMicrovmImage` = `POST /2025-09-09/microvm-images`; poll `GetMicrovmImage`
  until state `CREATED` or `CREATE_FAILED`.
- `RunMicrovm` = `POST /2025-09-09/microvms`; states `PENDING -> RUNNING ->
  SUSPENDING -> SUSPENDED -> TERMINATING -> TERMINATED`.
- `GetMicrovm`, `ListMicrovms`, `ListMicrovmImages` (`GET /2025-09-09/microvm-images`,
  returns only live images), `ListManagedMicrovmImages`.
- `SuspendMicrovm` / `ResumeMicrovm` = `POST .../{id}/suspend|resume`.
- `TerminateMicrovm` = **DELETE** `.../microvms/{id}`.
- `DeleteMicrovmImage` = **DELETE** `.../microvm-images/{FULL-ARN}`. The path segment
  must be the full image ARN, not the bare name (a name returns 400 "Invalid ARN
  format"); colons in the path are fine unencoded.
- **`DeleteMicrovmImage` is asynchronous** and the image *name* stays reserved while the
  delete completes (a minute or more). `CreateMicrovmImage` with the same name inside that
  window either fails with "already exists" or, worse, appears to succeed and is then
  swallowed by the completing delete (observed live: a create that never booted, no
  CloudWatch streams, and "No active version" forever). Retry the create until the name
  frees; the hellbox CLI does this automatically.
- **Wake on traffic:** any data-plane request to a SUSPENDED MicroVM auto-resumes it. To
  keep a suspended MicroVM paused, poll state via the control plane (`GetMicrovm` does not
  resume), not by hitting the endpoint.
- **Duration is a total budget, not fresh-from-suspend:** `maximumDurationInSeconds`
  (max 28800 = 8h) caps the MicroVM's *combined* time in the running AND suspended states
  before Lambda terminates it. That 8h is the real ceiling. `IdlePolicy.suspendedDurationSeconds`
  (min 0, no documented max) separately caps time spent suspended before termination, but
  it can never exceed the total-lifetime budget: play 3h, and at most ~5h of suspend remains.
  Hellbox sets both to 8h, so `maximumDurationInSeconds` is the effective limit. Do not tell
  users "come back next week"; frame-perfect resume only survives inside the 8h total window.

## Lifecycle hooks (the #1 build-failure source)

- Hooks are **ENABLED / DISABLED enums, not path strings**: `microvmImageHooks.ready`,
  `microvmHooks.run` / `resume` / ..., plus `*TimeoutInSeconds` and `hooks.port`.
- **The hook listener must be on port 9000.** AWS POSTs the readiness probe to
  `http://127.0.0.1:9000/aws/lambda-microvms/runtime/v1/ready`. A wrong port fails the
  build with "Ready hook invocation timed out".
- The image snapshot is captured the instant `/ready` returns 200, so **hold 503 until
  the app has actually drawn**, or every run and resume restores a blank screen.

## Image, base, and constraints

- Dockerfile base: `public.ecr.aws/lambda/microvms:al2023-minimal` (carries the guest
  agent). Managed base ARN per region: `arn:aws:lambda:<region>:aws:microvm-image:al2023-1`.
- **ARM64 only**, up to 16 vCPU / 32 GB RAM / 32 GB disk per MicroVM.
- **Account memory quota is 8 GB total** (each MicroVM is >= 2 GB, and suspended MicroVMs
  still hold their memory).
- **Amazon Linux 2023 has no SDL2 in its repos** (no EPEL), so anything SDL-based is
  compiled from source. AL2023 is glibc 2.34, roughly RHEL/Rocky 9.

## Networking

- **Omit** ingress / egress network connectors to get the Lambda-managed defaults
  (JWE-auth ingress + internet egress). Passing `[]` or `[""]` fails validation.

## ARM64 and the Graviton fleet

The MicroVM fleet runs on AWS Graviton (a mix of generations). Hellbox runs DOOM as a
**native aarch64** build, so it executes directly on the ARM CPU with no translation layer
and renders reliably on every host. Run native ARM code; if you need something that is not
native ARM, validate it across Graviton generations, not just across regions.
