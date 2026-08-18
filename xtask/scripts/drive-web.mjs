// Replays a scenario against the WASM build in real Chromium.
//
// The scenario comes in on stdin as JSON — the same file the native harness
// runs — so a screenshot named `03-request` shows the same interaction on both
// targets and the two can be compared directly.
//
// Console panics are a failure, not noise: a Rust panic in wasm leaves the
// event loop wedged and every later screenshot pixel-identical, which reads as
// "nothing happened" rather than "it crashed".
import { chromium } from "/opt/node22/lib/node_modules/playwright/index.mjs";
import { mkdir, rm } from "node:fs/promises";
import path from "node:path";

const [url, outDir] = process.argv.slice(2);
if (!url || !outDir) {
  console.error("usage: drive-web.mjs <url> <out-dir>  (scenario JSON on stdin)");
  process.exit(2);
}

const scenario = JSON.parse(await new Promise((resolve, reject) => {
  let text = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (chunk) => (text += chunk));
  process.stdin.on("end", () => resolve(text));
  process.stdin.on("error", reject);
}));

// Cleared per run so a stale screenshot from a previous, different scenario is
// never mistaken for this one's output.
await rm(outDir, { recursive: true, force: true });
await mkdir(outDir, { recursive: true });

const browser = await chromium.launch({
  executablePath: "/opt/pw-browsers/chromium-1194/chrome-linux/chrome",
  args: [
    "--no-sandbox",
    "--disable-dev-shm-usage",
    // The container has no GPU. SwiftShader also claims WebGPU support, which
    // is why the app asks for WebGL2 explicitly rather than trusting `Auto`.
    "--enable-unsafe-swiftshader",
    "--use-angle=swiftshader",
  ],
});

// GPUI sizes its canvas from the window, so this is the equivalent of the
// native harness's window size. Scenario coordinates are written against it.
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });

const failures = [];
// Kept so a failed boot can show what the app was doing, not just that it
// stopped: the app logs each stage of start-up.
const log = [];
const PANIC = /panicked at|RuntimeError|unreachable executed|called `Option::unwrap|called `Result::unwrap/i;
page.on("pageerror", (error) => failures.push(`pageerror: ${error.message}`));
page.on("console", (message) => {
  const text = message.text();
  log.push(`[${message.type()}] ${text}`);
  if (log.length > 60) log.shift();
  if (PANIC.test(text)) failures.push(`console: ${text}`);
});

// What the page itself says when it fails to start. The host page writes the
// reason into #loading, and without this a failed boot is just "no canvas".
async function pageState() {
  try {
    const loading = await page.textContent("#loading", { timeout: 2000 });
    const canvases = await page.locator("canvas").count();
    return `page says ${JSON.stringify(loading)}, ${canvases} canvas element(s)`;
  } catch (error) {
    return `page state unavailable: ${error.message}`;
  }
}

let status = 0;
try {
  await page.goto(url, { waitUntil: "load", timeout: 120000 });
  await page.waitForSelector("canvas", { timeout: 120000 });
  // The first frame lags canvas creation: fonts, storage and layout.
  await page.waitForTimeout(6000);

  for (const step of scenario.steps) {
    switch (step.step) {
      case "move":
        await page.mouse.move(step.x, step.y);
        await page.waitForTimeout(150);
        break;
      case "click":
        await page.mouse.move(step.x, step.y);
        await page.waitForTimeout(150);
        await page.mouse.click(step.x, step.y);
        await page.waitForTimeout(250);
        break;
      case "type":
        await page.keyboard.type(step.text, { delay: 40 });
        await page.waitForTimeout(250);
        break;
      case "key":
        // xdotool spells chords `ctrl+s`; Playwright wants `Control+s`.
        await page.keyboard.press(
          step.key.replace(/\bctrl\b/i, "Control").replace(/\bcmd\b/i, "Meta"),
        );
        await page.waitForTimeout(250);
        break;
      case "scroll":
        await page.mouse.wheel(0, step.by * 100);
        await page.waitForTimeout(250);
        break;
      case "wait":
        await page.waitForTimeout(step.ms);
        break;
      case "shot": {
        const file = path.join(outDir, `${step.name}.png`);
        await page.screenshot({ path: file });
        console.log(`  ${file}`);
        break;
      }
      default:
        throw new Error(`unknown step ${JSON.stringify(step)}`);
    }
  }
} catch (error) {
  failures.push(`driver: ${error.message}`);
  failures.push(await pageState());
  if (log.length > 0) failures.push(`last console output:\n    ${log.join("\n    ")}`);
  await page.screenshot({ path: path.join(outDir, "failure.png") }).catch(() => {});
} finally {
  await browser.close();
}

if (failures.length > 0) {
  for (const failure of failures) console.error(`  ! ${failure}`);
  status = 1;
}
process.exit(status);
