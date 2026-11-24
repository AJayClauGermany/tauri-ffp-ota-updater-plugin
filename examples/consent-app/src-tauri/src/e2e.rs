//! Remote control for `e2e.mjs`, compiled in with the `e2e` feature only.
//!
//! The script has no other way into a webview that works on Linux, macOS and Windows alike, so
//! the app lends it one: it runs the JavaScript the script sends in a window and sends back
//! what that evaluated to. With it the script clicks the buttons a user would and reads what
//! the pages show.

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    sync::Mutex,
};

use tauri::{plugin::TauriPlugin, Listener, Manager, Runtime};

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    tauri::plugin::Builder::new("e2e")
        .setup(|app, _| {
            let Ok(address) = std::env::var("OTA_E2E_CONTROL") else {
                return Ok(());
            };
            let requests = TcpStream::connect(address)?;
            let replies = Mutex::new(requests.try_clone()?);
            // a page reports what a script evaluated to with this event
            app.listen("e2e-reply", move |event| {
                let _ = writeln!(replies.lock().unwrap(), "{}", event.payload());
            });

            let app = app.clone();
            std::thread::spawn(move || {
                // one request per line: `{ "id": 1, "window": "main", "js": "<expression>" }`
                for line in BufReader::new(requests).lines() {
                    let Ok(request) =
                        serde_json::from_str::<serde_json::Value>(&line.unwrap_or_default())
                    else {
                        break;
                    };
                    let (Some(id), Some(window), Some(js)) = (
                        request["id"].as_u64(),
                        request["window"].as_str(),
                        request["js"].as_str(),
                    ) else {
                        break;
                    };
                    // A page that is not there, or not loaded yet, does not answer: the script
                    // asks again.
                    if let Some(window) = app.get_webview_window(window) {
                        let _ = window.eval(format!(
                            "(async () => {{
                                const reply = {{ id: {id} }};
                                try {{
                                    reply.ok = (await (async () => ({js}))()) ?? null;
                                }} catch (error) {{
                                    reply.error = error?.kind ? error : String(error);
                                }}
                                window.__TAURI__.event.emit('e2e-reply', reply);
                            }})()"
                        ));
                    }
                }
                // the script is done, or gone
                app.exit(0);
            });
            Ok(())
        })
        .build()
}
