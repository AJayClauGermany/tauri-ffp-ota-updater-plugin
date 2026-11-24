// The main window: the page that shows whether the app works, and the one the
// user answers in.
//
// `app.withGlobalTauri` is enabled in tauri.conf.json, so no bundler is
// needed. With one, use `status()`, `approve()` and the others from
// `tauri-plugin-ffp-ota-updater-api` instead of `invoke`.
const { invoke } = window.__TAURI__.core;
const plugin = (command, args) => invoke(`plugin:ffp-ota-updater|${command}`, args);

const $ = (id) => document.getElementById(id);
const log = (message) => ($("output").textContent += `${message}\n`);

async function run(action) {
  try {
    await action();
  } catch (error) {
    // plugin errors are `{ kind, message }`
    log(`Error: ${error?.kind ? `[${error.kind}] ${error.message}` : error}`);
  }
  await show();
}

// What the updater has on record. It is read from disk, so it also knows of a
// download from an earlier run, and none of the buttons needs a connection.
let shown = 0;
async function show() {
  const turn = ++shown;
  const status = await plugin("status");
  // a later call is under way: it has the newer answer, and draws it
  if (turn !== shown) return status;
  const { staged, approved } = status;
  $("version").textContent = status.currentVersion;
  $("state").textContent = !staged
    ? "No update is waiting."
    : approved === staged
      ? `Version ${staged} will be used the next time the app starts.`
      : `Version ${staged} is downloaded. Nothing changes until you choose.`;

  $("install").disabled = $("discard").disabled = !staged;
  $("approve").disabled = !staged || approved === staged;
  $("unapprove").disabled = !approved;
  // switches every window at once
  $("install").onclick = () => run(() => plugin("install", { version: staged }));
  // changes nothing in this run
  $("approve").onclick = () => run(() => plugin("approve", { version: staged }));
  $("unapprove").onclick = () => run(() => plugin("unapprove", { version: approved }));
  $("discard").onclick = () => run(() => plugin("discard", { version: staged }));
  return status;
}

// The Rust side checks and downloads; see `fetch_update` in src-tauri/src/lib.rs.
const fetchUpdate = () =>
  run(async () => {
    const version = await invoke("fetch_update");
    if (version) log(`Downloaded ${version}.`);
  });
$("fetch").addEventListener("click", fetchUpdate);

// This page got this far, so the frontend it belongs to is good to keep.
await plugin("notify_ready");
const status = await show();
if (status.rejected.length) log(`Rolled back earlier: ${status.rejected.join(", ")}`);
await fetchUpdate();
