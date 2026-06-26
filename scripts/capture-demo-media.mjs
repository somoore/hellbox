import fs from "node:fs";
import path from "node:path";
import { chromium } from "playwright";

const url = process.argv[2] ?? "http://127.0.0.1:6080/?display=h264";
const outDir = process.argv[3] ?? "assets/demo";

fs.mkdirSync(outDir, { recursive: true });

const browser = await chromium.launch({ headless: true });
const context = await browser.newContext({
  viewport: { width: 1280, height: 800 },
  deviceScaleFactor: 1,
  recordVideo: {
    dir: outDir,
    size: { width: 1280, height: 800 },
  },
});

const page = await context.newPage();
page.setDefaultTimeout(30_000);

await page.goto(url, { waitUntil: "domcontentloaded" });

await Promise.race([
  page.locator("canvas").first().waitFor({ state: "visible" }),
  page.locator("iframe").first().waitFor({ state: "visible" }),
]);

await page.waitForTimeout(4_000);
await page.keyboard.press("Enter").catch(() => {});
await page.keyboard.down("KeyW").catch(() => {});
await page.waitForTimeout(600);
await page.keyboard.up("KeyW").catch(() => {});
await page.keyboard.press("Control").catch(() => {});
await page.waitForTimeout(6_000);

const screenshot = path.join(outDir, "lambdadoom-live.png");
await page.screenshot({ path: screenshot, fullPage: true });

const video = page.video();
await context.close();
await browser.close();

if (video) {
  const tmpVideoPath = await video.path();
  const finalVideoPath = path.join(outDir, "lambdadoom-live.webm");
  fs.renameSync(tmpVideoPath, finalVideoPath);
}

console.log(`screenshot=${screenshot}`);
