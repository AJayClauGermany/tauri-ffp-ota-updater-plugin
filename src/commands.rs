use std::{str::FromStr, time::Duration};

use http::{HeaderMap, HeaderName, HeaderValue};
use semver::Version;
use serde::Serialize;
use tauri::{AppHandle, Runtime, Webview, command, ipc::Channel};
use url::Url;

use crate::{Error, OtaUpdaterExt, RequestOptions, Result, Status, Update};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub(crate) enum DownloadEvent {
    #[serde(rename_all = "camelCase")]
    Started {
        #[serde(skip_serializing_if = "Option::is_none")]
        content_length: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    Progress {
        chunk_length: usize,
    },
    Finished,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Metadata {
    current_version: String,
    version: String,
    downloaded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    raw_json: serde_json::Value,
}

fn parse(version: &str) -> Result<Version> {
    Ok(Version::parse(version.trim_start_matches('v'))?)
}

/// The update for `version` that a `check` returned to the webview.
fn pending<R: Runtime>(app: &AppHandle<R>, version: &str) -> Result<Update<R>> {
    app.ota_updater()
        .pending
        .lock()
        .unwrap()
        .get(&parse(version)?)
        .cloned()
        .ok_or_else(|| Error::NoPendingUpdate(version.to_string()))
}

async fn download_with_events<R: Runtime>(
    update: &Update<R>,
    on_event: &Channel<DownloadEvent>,
) -> Result<()> {
    let mut started = false;
    update
        .download(
            |chunk_length, content_length| {
                if !started {
                    started = true;
                    let _ = on_event.send(DownloadEvent::Started { content_length });
                }
                let _ = on_event.send(DownloadEvent::Progress { chunk_length });
            },
            || {
                let _ = on_event.send(DownloadEvent::Finished);
            },
        )
        .await
}

#[command]
pub(crate) async fn check<R: Runtime>(
    app: AppHandle<R>,
    headers: Option<Vec<(String, String)>>,
    download_headers: Option<Vec<(String, String)>>,
    timeout: Option<u64>,
    proxy: Option<Url>,
) -> Result<Option<Metadata>> {
    // no endpoints: where updates come from is not the webview's to decide
    let mut options = RequestOptions {
        headers: HeaderMap::new(),
        download_headers: HeaderMap::new(),
        timeout: timeout.map(Duration::from_millis),
        proxy,
        ..Default::default()
    };
    for (map, pairs) in [
        (&mut options.headers, headers),
        (&mut options.download_headers, download_headers),
    ] {
        for (name, value) in pairs.unwrap_or_default() {
            map.append(HeaderName::from_str(&name)?, HeaderValue::from_str(&value)?);
        }
    }

    let updater = app.ota_updater();
    let update = updater.check_with(options).await?;
    let metadata = update.as_ref().map(|update| Metadata {
        current_version: update.current_version.to_string(),
        version: update.version.to_string(),
        downloaded: update.downloaded,
        date: update.date.clone(),
        body: update.body.clone(),
        raw_json: update.raw_json.clone(),
    });
    if let Some(update) = update {
        // Kept until that version is being served, whatever later checks return: the page may
        // still hold the object it got for it.
        let current = updater.current_version();
        let mut pending = updater.pending.lock().unwrap();
        pending.retain(|version, _| updater.store.order.newer(version, &current));
        pending.insert(update.version.clone(), update);
    }
    Ok(metadata)
}

#[command]
pub(crate) async fn download<R: Runtime>(
    app: AppHandle<R>,
    version: String,
    on_event: Channel<DownloadEvent>,
) -> Result<()> {
    download_with_events(&pending(&app, &version)?, &on_event).await
}

#[command]
pub(crate) async fn install<R: Runtime>(app: AppHandle<R>, version: String) -> Result<()> {
    // by version alone: a bundle staged in an earlier run needs no `check` to be installed
    app.ota_updater().install(&parse(&version)?)
}

#[command]
pub(crate) async fn discard<R: Runtime>(app: AppHandle<R>, version: String) -> Result<()> {
    app.ota_updater().discard(&parse(&version)?)
}

#[command]
pub(crate) async fn download_and_install<R: Runtime>(
    app: AppHandle<R>,
    version: String,
    on_event: Channel<DownloadEvent>,
) -> Result<()> {
    let update = pending(&app, &version)?;
    download_with_events(&update, &on_event).await?;
    update.install()
}

#[command]
pub(crate) fn current_version<R: Runtime>(app: AppHandle<R>) -> String {
    app.ota_updater().current_version().to_string()
}

#[command]
pub(crate) async fn reset<R: Runtime>(app: AppHandle<R>) -> Result<()> {
    app.ota_updater().reset()
}

#[command]
pub(crate) async fn notify_ready<R: Runtime>(app: AppHandle<R>, webview: Webview<R>) -> Result<()> {
    app.ota_updater().notify_ready_from(webview.label())
}

// async: a sync command would run on the main thread
#[command]
pub(crate) async fn status<R: Runtime>(app: AppHandle<R>) -> Status {
    app.ota_updater().status()
}

#[command]
pub(crate) async fn rollback<R: Runtime>(app: AppHandle<R>) -> Result<()> {
    app.ota_updater().rollback()
}
