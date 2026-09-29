// Regenerates the two media files the README embeds: docs/media/demo.gif, a ~30 s recording of the
// cockpit driven through its in-browser mock (`?mock=1`) — the dashboard of colonies, one colony's
// chat, the agent's question with choices, the pull request it opens — and the 1280x640
// social-preview.png rendered from social-preview.html here. Run `node docs/media/record-demo.mjs`,
// with `--only gif` or `--only social` for one file; README.md next to this file holds the flags,
// prerequisites and the GIF's size budget. The cockpit is served by `vite dev` on a free port and
// stopped on exit, error or not; raw video and palettes stay in the cache directory.
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, openSync, statSync } from "node:fs";
import { connect, createServer } from "node:net";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const HERE = join(ROOT, "docs", "media");
const WEB = join(ROOT, "web");
const PLAYWRIGHT_PIN = "1.55.0";
const CACHE = process.env.COLONIZER_DEMO_CACHE ?? join(tmpdir(), "colonizer-demo-recorder");
const VIEWPORT = { width: 1280, height: 800 };
// The demo is full of small perpetual motion (settlers, cursors, pulses), so GIF frames never
// dedupe: fps, the palette cap and the gifsicle lossy pass are what hold the 5 MB budget.
const GIF = { width: 960, fps: 8, colors: 128, lossy: 100, budget: 5 * 1024 * 1024 };
// The waiting colony (`mock.ts`, `acme/webshop#64`) whose question the demo answers.
const CHAMBER = 'button[aria-label^="acme/webshop #64"]';

const only = process.argv[2] === "--only" ? process.argv[3] : null;
if (only !== null && only !== "gif" && only !== "social") {
  console.error(`usage: node docs/media/${basename(import.meta.url)} [--only gif|social]`);
  process.exit(2);
}

const say = (line) => console.log(`record-demo: ${line}`);
/** A failure with a maintainer-facing message: reported without a stack, exits non-zero. */
class Failure extends Error {}
const fail = (message) => {
  throw new Failure(message);
};
const sh = (file, args, opts = {}) => {
  const r = spawnSync(file, args, { stdio: "inherit", ...opts });
  if (r.status !== 0) fail(`${file} ${args.join(" ")} failed${r.status === null ? `: ${r.error?.message}` : ""}`);
};

function ensureEncoders() {
  for (const [tool, arg, help] of [
    ["ffmpeg", "-version", "apt-get install ffmpeg, brew install ffmpeg, or https://ffmpeg.org"],
    ["gifsicle", "--version", "apt-get install gifsicle, brew install gifsicle, or https://eternallybored.org/misc/gifsicle"],
  ]) {
    if (spawnSync(tool, [arg]).error) fail(`${tool} is not on PATH. Install it (${help}) and run this again.`);
  }
}

/** The pinned Playwright, installed into the cache directory and imported from there by absolute path. */
async function loadPlaywright() {
  const pkg = join(CACHE, "node_modules", "playwright", "package.json");
  if (!existsSync(pkg)) {
    say(`fetching playwright@${PLAYWRIGHT_PIN} into ${CACHE} (first run only)`);
    mkdirSync(CACHE, { recursive: true });
    sh("npm", ["install", "--prefix", CACHE, "--no-audit", "--no-fund", "--no-save", `playwright@${PLAYWRIGHT_PIN}`]);
  }
  const playwright = await import(pathToFileURL(join(CACHE, "node_modules", "playwright", "index.mjs")).href);
  if (!existsSync(playwright.chromium.executablePath())) {
    say("fetching Chromium (first run only)");
    sh(process.execPath, [join(CACHE, "node_modules", "playwright", "cli.js"), "install", "chromium"]);
  }
  return playwright;
}

/** `vite dev` for the cockpit on a free port, polled until it serves, killed by the returned stop(). */
async function startCockpit() {
  if (!existsSync(join(WEB, "node_modules"))) {
    say("web/node_modules is missing; running npm ci in web/");
    sh("npm", ["ci", "--no-audit", "--no-fund"], { cwd: WEB });
  }
  const port = await new Promise((ok) => {
    const probe = createServer();
    // `address()` is null once close() begins, so read the port before closing the probe.
    probe.listen(0, () => {
      const { port } = probe.address();
      probe.close(() => ok(port));
    });
  });
  const log = openSync("/tmp/colonizer-demo-vite.log", "w");
  const vite = spawn("npm", ["run", "dev", "--", "--port", String(port), "--strictPort"], { cwd: WEB, detached: true, stdio: ["ignore", log, log] });
  // vite binds `localhost`, which is ::1 first here — never hardcode 127.0.0.1.
  const url = `http://localhost:${port}/?mock=1`;
  const serving = () =>
    new Promise((up) => {
      const probe = connect({ host: "localhost", port });
      probe.on("connect", () => { probe.destroy(); up(true); });
      probe.on("error", () => up(false));
    });
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    if (vite.exitCode !== null) fail(`vite exited (${vite.exitCode}) — see /tmp/colonizer-demo-vite.log`);
    if (await serving()) {
      say(`cockpit serving on ${url}`);
      return { url, stop: () => { try { process.kill(-vite.pid, "SIGTERM"); } catch { /* already gone */ } } };
    }
    await new Promise((r) => setTimeout(r, 250));
  }
  fail("vite did not start within 120 s — see /tmp/colonizer-demo-vite.log");
}

/**
 * The story. Every hold is a beat a viewer reads; waits on selectors keep the script correct even
 * when the mock's streaming runs slow, so the runtime drifts a little either side of ~30 s.
 */
async function story(page) {
  const hold = (ms) => page.waitForTimeout(ms);

  // The nest: every colony in its own chamber, the dashboard beside it. Answer the two first-run
  // cards out of the way (declining the live map is the privacy-preserving choice).
  say("scene 1: the nest");
  await hold(1100);
  const org = page.getByRole("region", { name: "New workspace initech" });
  await org.getByText("Not this one").click();
  await hold(300);
  await org.getByRole("button", { name: "Confirm" }).click();
  await page.getByRole("region", { name: "Live map" }).getByRole("button", { name: "No thanks" }).click();
  await hold(800);

  // Open the waiting colony: pick its chamber, hold on the zoom, then "open colony" from there.
  say("scene 2: a colony's chat");
  await page.locator(CHAMBER).click();
  await hold(1100);
  await page.getByRole("button", { name: "open colony →" }).click();
  await page.getByRole("button", { name: "Create PR" }).waitFor({ timeout: 30_000 });
  await hold(1100);

  // The colony's chat ends at the question the agent is stuck on: pick the guest flow, then what
  // else belongs in the pull request. The click lands on the option's title, not the card's
  // centre: the code preview swallows clicks, so a tall row toggles unreliably from its middle.
  const card = page.locator("form[data-open-question]").last();
  await card.waitFor({ timeout: 120_000 });
  say("scene 3: the agent asks with choices");
  await hold(600);
  for (const label of ["Guest cart by email", "Regression test", "Update docs"]) {
    await card.locator("label", { hasText: label }).getByText(label, { exact: true }).click();
    await hold(550);
  }
  await hold(300);
  await card.getByRole("button", { name: /Submit answers?/ }).click();

  // The fix turn runs (edit, tests, summary); then publish and hold on the opened pull request.
  await page.getByText("or tell me what to change").waitFor({ timeout: 90_000 });
  say("scene 4: the pull request");
  await hold(1000);
  await page.getByRole("button", { name: "Create PR" }).click();
  await page.getByRole("link", { name: /View PR/ }).waitFor({ timeout: 30_000 });
  await hold(2600);
}

/** The browser to record with: root needs --no-sandbox, a proxy may make even http look unsafe. */
async function launch(playwright) {
  return playwright.chromium.launch({ args: process.getuid?.() === 0 ? ["--no-sandbox"] : [], ignoreHTTPSErrors: true });
}

async function recordGif(playwright, url) {
  const work = mkdtempSync(join(CACHE, "run-"));
  const browser = await launch(playwright);
  try {
    // A first, unrecorded pass warms vite's module graph so the recorded page paints at once.
    const warm = await browser.newContext({ viewport: VIEWPORT });
    const warmPage = await warm.newPage();
    await warmPage.goto(url, { waitUntil: "domcontentloaded" });
    await warmPage.locator(CHAMBER).waitFor({ timeout: 120_000 });
    await warm.close();

    const context = await browser.newContext({
      viewport: VIEWPORT,
      deviceScaleFactor: 1,
      colorScheme: "dark",
      recordVideo: { dir: join(work, "raw"), size: VIEWPORT },
    });
    const at = Date.now();
    const page = await context.newPage();
    await page.goto(url, { waitUntil: "domcontentloaded" });
    // "reconnecting…" is a mock-only artifact — mock.ts' socket never opens, while a cockpit connected to its mothership doesn't show it — so hide it.
    await page.addStyleTag({ content: "header span[role='status'][title^='the live feed dropped'] { display: none }" });
    await page.locator(CHAMBER).waitFor({ timeout: 120_000 });
    // The recorder starts at page creation, not at first paint: cut the boot off the GIF.
    const lead = ((Date.now() - at) / 1000 + 0.2).toFixed(2);
    await story(page);
    const video = page.video();
    await context.close();
    const raw = join(work, "demo.webm");
    await video.saveAs(raw);

    // Two palette passes: colours picked where frames differ, then applied with no dither (a flat
    // UI bands less than it dithers) and only changed rectangles rewritten; gifsicle does the rest.
    const palette = join(work, "palette.png");
    const pass = (extra) => ["-loglevel", "error", "-ss", lead, "-i", raw, ...extra, "-y"];
    sh("ffmpeg", pass(["-vf", `fps=${GIF.fps},scale=${GIF.width}:-2:flags=lanczos,palettegen=max_colors=${GIF.colors}:stats_mode=diff`, palette]));
    const full = join(work, "demo-full.gif");
    sh("ffmpeg", pass(["-i", palette, "-lavfi", `[0:v]fps=${GIF.fps},scale=${GIF.width}:-2:flags=lanczos[x];[x][1:v]paletteuse=dither=none:diff_mode=rectangle`, full]));
    const out = join(HERE, "demo.gif");
    sh("gifsicle", ["-O3", `--lossy=${GIF.lossy}`, "-o", out, full]);
    const { size } = statSync(out);
    const mb = (size / 1048576).toFixed(2);
    if (size > GIF.budget) {
      fail(`demo.gif is ${mb} MB, over the ${GIF.budget / 1048576} MB budget — lower GIF.fps, GIF.width or GIF.colors and run again.`);
    }
    const probe = spawnSync("ffprobe", ["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height,nb_frames", "-of", "csv=p=0", out]);
    say(`demo.gif ${probe.stdout.toString().trim()} — ${mb} MB, within budget`);
  } finally {
    await browser.close();
  }
}

async function renderSocial(playwright) {
  const browser = await launch(playwright);
  try {
    const page = await (await browser.newContext({ viewport: { width: 1280, height: 640 }, deviceScaleFactor: 1 })).newPage();
    await page.goto(`file://${join(HERE, "social-preview.html")}`);
    await page.locator(".card").waitFor();
    await page.waitForTimeout(250);
    await page.screenshot({ path: join(HERE, "social-preview.png") });
    say("social-preview.png 1280x640 written from social-preview.html");
  } finally {
    await browser.close();
  }
}

if (only !== "social") ensureEncoders();
const playwright = await loadPlaywright();
const cockpit = only === "social" ? null : await startCockpit();
try {
  if (cockpit) await recordGif(playwright, cockpit.url);
  if (only !== "gif") await renderSocial(playwright);
  say("done");
} catch (error) {
  if (error instanceof Failure) console.error(`record-demo: ${error.message}`);
  else throw error;
  process.exitCode = 1;
} finally {
  cockpit?.stop();
}
