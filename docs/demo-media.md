# Demo media

Use real LambdaDoom sessions for launch media. Do not use a static mock, local-only HTML page,
or generated DOOM-looking image: the point of the demo is that the pixels are streamed from an
AWS Lambda MicroVM and survive suspend/resume.

## Capture

Start a live proxy:

```bash
ldoom open --name doom --no-open
```

Then capture:

```bash
make capture-demo-media
```

The script opens the local proxy with pinned Playwright, waits for the real stream surface,
sends a couple of simple inputs, and writes:

- `assets/demo/lambdadoom-live.png`
- `assets/demo/lambdadoom-live.webm`

Set `LAMBDADOOM_DEMO_URL` if the proxy picked a different port:

```bash
LAMBDADOOM_DEMO_URL=http://127.0.0.1:49152/?display=h264 make capture-demo-media
```

## README usage

Only embed files that were produced from a real session. If the script cannot reach the proxy,
it exits before writing media so the repo does not accidentally ship a fake screenshot.
