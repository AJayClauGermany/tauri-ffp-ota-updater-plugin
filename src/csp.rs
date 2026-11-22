//! Content Security Policy support for bundles.
//!
//! When the app has a CSP, `tauri build` processes the embedded frontend: it puts nonce
//! placeholders on `<style>` elements and on scripts loaded from `http(s)` URLs, and records the
//! hashes of inline scripts and of every `.js` file. At runtime Tauri turns the placeholders into
//! fresh nonces and adds nonces and hashes to the policy it sends with each page. A downloaded
//! bundle never went through that build step, so the same processing is done here, when the
//! bundle is about to be served. It follows `tauri-codegen`; see `map_core_assets` and
//! `CspHashes::add_if_applicable` there.

use std::{
    collections::{BTreeSet, HashMap},
    sync::Mutex,
};

use base64::Engine;
use sha2::{Digest, Sha256};
use tauri::utils::{
    assets::{SCRIPT_NONCE_TOKEN, STYLE_NONCE_TOKEN},
    config::SecurityConfig,
};

use crate::bundle::Bundle;

/// The hashes a bundle adds to the policy, as `'sha256-…'` sources for `script-src`.
#[derive(Debug, Default)]
pub(crate) struct Hashes {
    /// Of every `.js` and `.mjs` file, for every page.
    pub scripts: Vec<&'static str>,
    /// Of the inline scripts of each HTML file, by asset key.
    pub inline_scripts: HashMap<String, Vec<&'static str>>,
}

/// Prepares `bundle` for the app's CSP, like `tauri build` prepares the embedded frontend. Does
/// nothing when the app has no CSP.
pub(crate) fn prepare(bundle: &mut Bundle, security: &SecurityConfig) {
    if security.csp.is_none() && security.dev_csp.is_none() {
        return;
    }
    let disabled = &security.dangerous_disable_asset_csp_modification;
    let scripts = disabled.can_modify("script-src");
    let styles = disabled.can_modify("style-src");

    let Bundle { files, csp, .. } = bundle;
    for (key, contents) in files.iter_mut() {
        if key.ends_with(".js") || key.ends_with(".mjs") {
            if scripts {
                csp.scripts.push(hash(contents));
            }
        } else if key.ends_with(".html") {
            let document = dom_query::Document::from(String::from_utf8_lossy(contents).as_ref());
            if scripts {
                inject_nonce(&document, "script[src^='http']", SCRIPT_NONCE_TOKEN);
            }
            if styles {
                inject_nonce(&document, "style", STYLE_NONCE_TOKEN);
            }
            if scripts {
                let hashes = document
                    .select("script:not(:empty)")
                    .iter()
                    .map(|script| hash(script.text().as_bytes()))
                    .collect();
                csp.inline_scripts.insert(key.clone(), hashes);
            }
            *contents = document.html().as_bytes().to_vec();
        }
    }
}

fn inject_nonce(document: &dom_query::Document, selector: &str, token: &str) {
    for element in document.select(selector).nodes() {
        if element.attr("nonce").is_none() {
            element.set_attr("nonce", token);
        }
    }
}

/// The CSP source for a script, hashed as the browser does: with line endings normalized.
fn hash(script: &[u8]) -> &'static str {
    let mut normalized = Vec::with_capacity(script.len());
    let mut bytes = script.iter().peekable();
    while let Some(&byte) = bytes.next() {
        if byte == b'\r' {
            bytes.next_if_eq(&&b'\n');
            normalized.push(b'\n');
        } else {
            normalized.push(byte);
        }
    }
    let digest = Sha256::digest(&normalized);
    intern(format!(
        "'sha256-{}'",
        base64::engine::general_purpose::STANDARD.encode(digest)
    ))
}

/// Gives `hash` the lifetime Tauri's [`CspHash`](tauri::utils::assets::CspHash) wants from an
/// asset provider. Each distinct hash is kept once for the life of the process, so memory only
/// grows with the number of different scripts across the bundles served.
fn intern(hash: String) -> &'static str {
    static HASHES: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());
    let mut hashes = HASHES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&known) = hashes.get(hash.as_str()) {
        return known;
    }
    let hash: &'static str = Box::leak(hash.into_boxed_str());
    hashes.insert(hash);
    hash
}

#[cfg(test)]
mod tests {
    use super::hash;

    #[test]
    fn line_endings_are_normalized_before_hashing() {
        assert_eq!(hash(b"a\r\nb\rc"), hash(b"a\nb\nc"));
        // `printf 'a\nb\nc' | openssl dgst -sha256 -binary | base64`
        assert_eq!(
            hash(b"a\nb\nc"),
            "'sha256-6n+wi3otxGGf+3x7s42VogR5NfoWXXGxLv04UqLm0Mw='"
        );
    }
}
