// End-to-end run of this example in a real webview: `npm run e2e`.
//
// It builds the app, publishes frontends to a server on localhost and drives
// the app's own buttons to show, on whatever platform it runs on, that
//
//   1. a downloaded frontend changes nothing, also across a restart, until the
//      user agrees (`requireApproval`),
//   2. agreeing and switching need no connection,
//   3. the main page confirms a frontend, and the bootstrap window cannot,
//   4. a frontend whose main page is broken is taken back, offline,
//   5. a frontend without the page of one of the windows is refused
//      (`requiredFiles`),
//   6. a downloaded frontend passes the app's Content Security Policy.
//
// Needs what the tutorial needs: the Tauri prerequisites, Node.js and `tar`.
// On Linux without a display: `xvfb-run -a npm run e2e`.

import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.dirname(fileURLToPath(import.meta.url));
const work = fs.mkdtempSync(path.join(os.tmpdir(), "ota-e2e-"));
const ota = path.join(work, "ota");
fs.mkdirSync(ota);
// of its own, so that a run neither finds nor leaves anything in the example's data
const identifier = "com.example.ffp-ota-consent.e2e";
const KEY_PASSWORD = "e2e";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** What `promise` resolves to, or `undefined` if it takes longer than `ms`. */
async function within(ms, promise) {
  let timer;
  const timeout = new Promise((resolve) => (timer = setTimeout(resolve, ms)));
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    // a timer that is left running keeps Node alive after the last step
    clearTimeout(timer);
  }
}

/** Deletes `dir` if it can. On Windows a webview process that is still closing may hold a file. */
function remove(dir) {
  try {
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 300 });
  } catch (error) {
    console.warn(`could not delete ${dir}: ${error.message}`);
  }
}

function run(command, args, options = {}) {
  // stdout is where the signer prints keys and signatures, at length
  const result = spawnSync(command, args, { stdio: ["ignore", "ignore", "inherit"], ...options });
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed: ${result.error ?? result.status}`);
  }
}

// the CLI's entry point, run with this Node: no shell in between on any platform
const tauri = (args, options) =>
  run(
    process.execPath,
    [path.join(root, "node_modules", "@tauri-apps", "cli", "tauri.js"), ...args],
    { cwd: root, ...options },
  );

// --- the release server: a folder on localhost that can be taken offline

let server;
let port = 0;

async function online() {
  server = http.createServer((request, response) => {
    const file = path.join(ota, path.basename(new URL(request.url, "http://localhost").pathname));
    fs.readFile(file, (error, body) => {
      response.writeHead(error ? 404 : 200).end(error ? undefined : body);
    });
  });
  await new Promise((resolve, reject) =>
    server.once("error", reject).listen(port, "127.0.0.1", resolve),
  );
  port = server.address().port;
}

async function offline() {
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
}

// --- publishing, with the commands of docs/how-to/publish.md

/** Packs `src` as `version`, after `edit` changed the copy, signs it and makes it the release. */
function publish(version, heading, edit = () => {}) {
  const dist = path.join(work, `dist-${version}`);
  fs.cpSync(path.join(root, "src"), dist, { recursive: true });
  for (const page of ["index.html", "bootstrap.html"]) {
    const file = path.join(dist, page);
    const html = fs.readFileSync(file, "utf8");
    assert(html.includes("built-in"), `${page} has lost its build label`);
    fs.writeFileSync(
      file,
      html.replaceAll("built-in", version).replace("<h1>Consent example</h1>", `<h1>${heading}</h1>`),
    );
  }
  edit(dist);

  const name = `frontend-${version}.tar.gz`;
  // relative paths: the `tar` of Git for Windows takes `C:` for a remote host
  run("tar", ["-czf", `ota/${name}`, "-C", `dist-${version}`, "."], { cwd: work });
  tauri([
    "signer", "sign",
    "--private-key-path", path.join(work, "e2e.key"),
    "--password", KEY_PASSWORD,
    "--app-version", version,
    path.join(ota, name),
  ]);
  const manifest = {
    version,
    url: `http://127.0.0.1:${port}/${name}`,
    signature: fs.readFileSync(path.join(ota, `${name}.sig`), "utf8"),
  };
  fs.writeFileSync(path.join(ota, "ota.json"), JSON.stringify(manifest));
}

// --- the app, and the remote control it is built with for this run (src-tauri/src/e2e.rs)

class App {
  static async start() {
    const app = new App();
    const control = net.createServer();
    await new Promise((resolve) => control.listen(0, "127.0.0.1", resolve));
    const connected = new Promise((resolve) => control.once("connection", resolve));

    app.exited = false;
    app.process = spawn(binary, [], {
      stdio: "inherit",
      env: {
        ...process.env,
        OTA_E2E_CONTROL: `127.0.0.1:${control.address().port}`,
        // WebKitGTK without a GPU, as on a CI runner
        WEBKIT_DISABLE_DMABUF_RENDERER: "1",
        WEBKIT_DISABLE_COMPOSITING_MODE: "1",
      },
    });
    app.process.once("exit", () => (app.exited = true));
    running = app;

    app.socket = await within(120_000, connected);
    control.close();
    if (!app.socket) throw new Error("the app did not start");

    app.pending = new Map();
    app.next = 0;
    let buffer = "";
    app.socket.on("error", () => {});
    app.socket.on("data", (chunk) => {
      buffer += chunk;
      for (let end; (end = buffer.indexOf("\n")) >= 0; buffer = buffer.slice(end + 1)) {
        const reply = JSON.parse(buffer.slice(0, end));
        app.pending.get(reply.id)?.(reply);
        app.pending.delete(reply.id);
      }
    });
    return app;
  }

  /** What `expression` evaluates to in `window`. Rejects with what it threw. */
  async js(window, expression, timeout = 5_000) {
    const id = ++this.next;
    const reply = new Promise((resolve) => this.pending.set(id, resolve));
    this.socket.write(`${JSON.stringify({ id, window, js: expression })}\n`);
    const result = await within(timeout, reply);
    this.pending.delete(id);
    // a page that is loading or reloading does not answer
    if (!result) throw new Error(`no answer from the ${window} window`);
    if ("error" in result) throw result.error;
    return result.ok;
  }

  /** Asks until `accept` is content with the answer, and returns it. */
  async until(window, expression, accept, what, timeout = 60_000) {
    const deadline = Date.now() + timeout;
    let last;
    for (;;) {
      try {
        last = await this.js(window, expression, 2_000);
        if (accept(last)) return last;
      } catch (error) {
        last = error;
      }
      if (this.exited) throw new Error(`the app exited while waiting for ${what}`);
      if (Date.now() > deadline) {
        const seen = last instanceof Error ? last.message : JSON.stringify(last);
        throw new Error(`timed out waiting for ${what}; last seen: ${seen}`);
      }
      await sleep(250);
    }
  }

  page = (window = "main") => this.js(window, PAGE);
  status = (window = "main") => this.js(window, STATUS);

  /**
   * Clicks a button of the main page as a user could: once the page has enabled it. The
   * updater's status changes before the page has drawn it, so a click that follows a change
   * of status at once would land on a button that is still disabled, and do nothing.
   */
  async click(id) {
    const button = `document.getElementById(${JSON.stringify(id)})`;
    await this.until("main", `!${button}.disabled`, (enabled) => enabled, `the ${id} button`);
    try {
      await this.js("main", `${button}.click()`);
    } catch (error) {
      // "Reload into it now" replaces the page, which may not get to answer first. What the
      // click was for is checked by the step that follows.
      if (!(error instanceof Error && error.message.startsWith("no answer"))) throw error;
    }
  }

  /** Waits for the main page to show what `accept` looks for. */
  shows = (accept, what) => this.until("main", PAGE, accept, what);
  /** Waits for the updater's status to be what `accept` looks for. */
  reaches = (accept, what) => this.until("main", STATUS, accept, what);

  async exit() {
    // anything that is not a request ends the app
    this.socket.write(`${JSON.stringify({ exit: true })}\n`);
    for (let waited = 0; !this.exited; waited += 100) {
      if (waited > 20_000) throw new Error("the app did not exit");
      await sleep(100);
    }
    running = undefined;
  }
}

const PAGE = `({
  path: location.pathname,
  heading: document.querySelector("h1")?.textContent ?? null,
  build: document.documentElement.dataset.build,
  inline: document.documentElement.dataset.inline ?? null,
  version: document.getElementById("version").textContent,
  state: document.getElementById("state")?.textContent ?? null,
  output: document.getElementById("output")?.textContent ?? null,
})`;
const STATUS = `window.__TAURI__.core.invoke("plugin:ffp-ota-updater|status")`;
const NOTIFY_READY = `window.__TAURI__.core.invoke("plugin:ffp-ota-updater|notify_ready")`;

let running;
let binary;

const step = (title) => console.log(`\n=== ${title}`);

async function main() {
  step("setup: a signing key, a release server and a build of the app");
  tauri(["signer", "generate", "--ci", "--password", KEY_PASSWORD, "--write-keys", path.join(work, "e2e.key")]);
  await online();

  const config = path.join(work, "tauri.e2e.conf.json");
  fs.writeFileSync(
    config,
    JSON.stringify({
      identifier,
      plugins: {
        "ffp-ota-updater": {
          endpoints: [`http://127.0.0.1:${port}/ota.json`],
          pubkey: fs.readFileSync(path.join(work, "e2e.key.pub"), "utf8").trim(),
          // long enough to look at a broken frontend before it is taken back
          readyTimeout: 15_000,
        },
      },
    }),
  );
  // `--debug` compiles faster; the frontend is embedded and served as in a release build
  tauri(["build", "--debug", "--no-bundle", "--features", "e2e", "--config", config], { stdio: "inherit" });
  const target = process.env.CARGO_TARGET_DIR ?? path.join(root, "src-tauri", "target");
  binary = path.join(target, "debug", process.platform === "win32" ? "consent-app.exe" : "consent-app");

  const dataDir = {
    linux: path.join(process.env.XDG_DATA_HOME ?? path.join(os.homedir(), ".local", "share"), identifier),
    darwin: path.join(os.homedir(), "Library", "Application Support", identifier),
    win32: path.join(process.env.LOCALAPPDATA ?? "", identifier),
  }[process.platform];
  remove(dataDir);

  step("1. online: an update is downloaded, and nothing changes");
  publish("0.1.1", "Updated over the air");
  let app = await App.start();
  let status = await app.reaches((status) => status.staged === "0.1.1", "0.1.1 to be downloaded");
  assert.equal(status.currentVersion, "0.1.0");
  assert.equal(status.approved, undefined);
  let page = await app.shows((page) => page.state.includes("Nothing changes"), "the offer");
  assert.equal(page.heading, "Consent example");
  // the inline script ran: the built-in frontend passes the app's CSP
  assert.equal(page.inline, "built-in");
  assert.equal((await app.page("bootstrap")).build, "built-in");
  await app.exit();

  step("2. offline: it still waits after a restart, and is approved without a connection");
  await offline();
  app = await App.start();
  page = await app.shows((page) => page.output.includes("[network]"), "the failed check");
  assert.equal(page.heading, "Consent example");
  assert.equal(page.version, "0.1.0");
  status = await app.status();
  assert.equal(status.currentVersion, "0.1.0");
  assert.equal(status.staged, "0.1.1");
  assert.equal(status.approved, undefined);
  // the user agrees, changes their mind, and agrees again
  await app.click("approve");
  await app.reaches((status) => status.approved === "0.1.1", "the approval");
  await app.click("unapprove");
  await app.reaches((status) => status.approved === undefined, "the approval to be taken back");
  await app.click("approve");
  await app.reaches((status) => status.approved === "0.1.1", "the approval");
  page = await app.shows((page) => page.state.includes("next time"), "the approval to show");
  assert.equal(page.heading, "Consent example");
  assert.equal((await app.status()).currentVersion, "0.1.0");
  await app.exit();

  step("3. offline: the next start shows the approved frontend, and its main page confirms it");
  app = await App.start();
  page = await app.shows((page) => page.heading === "Updated over the air", "the new main page");
  assert.equal(page.path, "/");
  assert.equal(page.build, "0.1.1");
  // the downloaded frontend passes the CSP as well, with an inline script of its own
  assert.equal(page.inline, "0.1.1");
  status = await app.reaches((status) => status.confirmed, "the confirmation");
  assert.equal(status.currentVersion, "0.1.1");
  assert.equal(status.staged, undefined);
  assert.deepEqual(status.rejected, []);
  // every window is served from the bundle
  page = await app.until("bootstrap", PAGE, (page) => page.version === "0.1.1", "the bootstrap page");
  assert.equal(page.path, "/bootstrap.html");
  assert.equal(page.build, "0.1.1");
  await app.exit();

  step("4. a frontend whose main page is broken: the bootstrap window cannot confirm it, and it is taken back offline");
  await online();
  publish("0.1.2", "Broken build", (dist) => {
    const file = path.join(dist, "main.js");
    const script = fs.readFileSync(file, "utf8");
    // the checkout has CRLF line endings on Windows
    const confirmation = /^await plugin\("notify_ready"\);\r?\n/m;
    assert(confirmation.test(script), "main.js no longer confirms the frontend");
    fs.writeFileSync(file, script.replace(confirmation, ""));
  });
  app = await App.start();
  await app.reaches((status) => status.staged === "0.1.2", "0.1.2 to be downloaded");
  assert.equal((await app.page()).heading, "Updated over the air");
  await offline();
  await app.click("install");
  await app.shows((page) => page.heading === "Broken build", "the reload into 0.1.2");
  page = await app.until("bootstrap", PAGE, (page) => page.build === "0.1.2", "the bootstrap page of 0.1.2");
  assert.equal(page.path, "/bootstrap.html");
  // its page is from the bundle on trial, but its window lacks the permission
  await assert.rejects(app.js("bootstrap", NOTIFY_READY), (error) => {
    assert.match(String(error), /not allowed/);
    return true;
  });
  status = await app.status();
  assert.equal(status.currentVersion, "0.1.2");
  assert.equal(status.confirmed, false);
  // nobody confirms it: `readyTimeout` passes and every window reloads into the last good one
  await app.shows((page) => page.heading === "Updated over the air", "the rollback");
  await app.until("bootstrap", PAGE, (page) => page.build === "0.1.1", "the bootstrap page to go back");
  status = await app.status();
  assert.equal(status.currentVersion, "0.1.1");
  assert.equal(status.confirmed, true);
  assert.deepEqual(status.rejected, ["0.1.2"]);
  await app.exit();

  step("5. offline: it stays on the last good frontend");
  app = await App.start();
  page = await app.shows((page) => page.output.includes("Rolled back earlier: 0.1.2"), "the report");
  assert.equal(page.heading, "Updated over the air");
  assert.equal((await app.status()).currentVersion, "0.1.1");
  await app.exit();

  step("6. online: a frontend without the bootstrap page is refused");
  await online();
  publish("0.1.3", "No bootstrap page", (dist) => fs.rmSync(path.join(dist, "bootstrap.html")));
  app = await App.start();
  page = await app.shows((page) => page.output.includes("[invalidBundle]"), "the refusal");
  assert.match(page.output, /bootstrap\.html/);
  status = await app.status();
  assert.equal(status.staged, undefined);
  assert.equal(status.currentVersion, "0.1.1");
  await app.exit();
  await offline();

  remove(dataDir);
  console.log("\nAll steps passed.");
}

main()
  .catch((error) => {
    console.error(`\nFAILED: ${error?.stack ?? JSON.stringify(error)}`);
    process.exitCode = 1;
  })
  .finally(() => {
    running?.process.kill();
    server?.close();
    remove(work);
  });
