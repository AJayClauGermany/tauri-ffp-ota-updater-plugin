// `app.withGlobalTauri` is enabled in tauri.conf.json, so the Tauri API is
// available on `window.__TAURI__` and no bundler is required. With a bundler,
// use `check()` from `tauri-plugin-ffp-ota-updater-api` instead of `invoke`.
const { invoke, Channel } = window.__TAURI__.core;
const plugin = (command, args) => invoke(`plugin:ffp-ota-updater|${command}`, args);

const $ = (id) => document.getElementById(id);
const log = (message) => ($("output").textContent += `${message}\n`);

let update = null;

function progressChannel() {
  const channel = new Channel();
  let received = 0;
  channel.onmessage = ({ event, data }) => {
    if (event === "Progress") received += data.chunkLength;
    if (event === "Finished") log(`Downloaded ${received} bytes`);
  };
  return channel;
}

async function run(action) {
  try {
    await action();
  } catch (error) {
    // plugin errors are `{ kind, message }`
    log(`Error: ${error?.kind ? `[${error.kind}] ${error.message}` : error}`);
  }
}

$("check").addEventListener("click", () =>
  run(async () => {
    update = await plugin("check");
    $("download").disabled = $("install").disabled = !update;
    log(
      update
        ? `Frontend update ${update.version} available (current ${update.currentVersion}): ${update.body ?? ""}`
        : "No frontend update available",
    );
  }),
);

$("download").addEventListener("click", () =>
  run(async () => {
    await plugin("download", { version: update.version, onEvent: progressChannel() });
    log("Stored. It will be used the next time the app starts.");
  }),
);

$("install").addEventListener("click", () =>
  // reloads the page into the new frontend
  run(() =>
    plugin("download_and_install", { version: update.version, onEvent: progressChannel() }),
  ),
);

$("reset").addEventListener("click", () => run(() => plugin("reset")));

run(async () => {
  // the page got this far, so a downloaded frontend is good to keep
  await plugin("notify_ready");
  const status = await plugin("status");
  $("version").textContent = status.currentVersion;
  if (status.staged) {
    log(`${status.staged} is downloaded and will be used at the next launch.`);
    $("apply").disabled = $("discard").disabled = false;
    // by version alone: no check, so this works offline
    $("apply").onclick = () => run(() => plugin("install", { version: status.staged }));
    $("discard").onclick = () =>
      run(async () => {
        await plugin("discard", { version: status.staged });
        $("apply").disabled = $("discard").disabled = true;
        log(`Discarded ${status.staged}.`);
      });
  }
  if (status.rejected.length) log(`Rolled back earlier: ${status.rejected.join(", ")}`);
});
