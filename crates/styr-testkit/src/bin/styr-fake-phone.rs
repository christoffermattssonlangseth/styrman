//! A scripted phone: pairs with the running styrd over loopback, watches every
//! shared session, and answers the next permission request.
//!
//!     styr-fake-phone [--answer allow|always|deny] [--after-ms N] [--count N]
//!
//! It registers itself as a device named "fake-phone"; revoke it afterwards with
//! `styr devices revoke <ID>` (it prints the id).

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::collections::HashSet;
use std::time::Duration;
use styr_core::event::EventKind;
use styr_core::ipc::{Client, Request, Response};
use styr_core::paths::Paths;
use styr_core::phone::{PairResponse, ServerMsg};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut answer = "allow".to_owned();
    let mut after = 0u64;
    let mut count = 1u32;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--answer" => answer = args.next().unwrap_or(answer),
            "--after-ms" => after = args.next().and_then(|v| v.parse().ok()).unwrap_or(after),
            "--count" => count = args.next().and_then(|v| v.parse().ok()).unwrap_or(count),
            other => anyhow::bail!("unknown argument {other}"),
        }
    }

    let client = Client::new(Paths::resolve().socket);
    let Response::Pairing { code, urls, .. } = client.call(&Request::CreatePairing)? else {
        anyhow::bail!("unexpected pairing reply");
    };
    let ws_url = urls
        .iter()
        .find(|u| u.contains("127.0.0.1"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no loopback listener"))?;
    let base = ws_url.replace("ws://", "http://").replace("/v1/ws", "");
    let paired: PairResponse = reqwest::Client::new()
        .post(format!("{base}/v1/pair"))
        .json(&json!({"code": code, "device_name": "fake-phone"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    eprintln!("fake-phone: paired as {}", paired.device_id);

    let mut req = ws_url.as_str().into_client_request()?;
    req.headers_mut()
        .insert("authorization", format!("Bearer {}", paired.token).parse()?);
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await?;
    let mut subscribed: HashSet<String> = HashSet::new();
    let mut answered = 0;
    while let Some(msg) = ws.next().await {
        let Message::Text(text) = msg? else { continue };
        let msg: ServerMsg = serde_json::from_str(text.as_str())?;
        let to_subscribe: Vec<(String, u64)> = match &msg {
            ServerMsg::Sessions { sessions } => sessions
                .iter()
                .map(|s| (s.id.to_string(), s.last_seq))
                .collect(),
            ServerMsg::SessionUpdated { session } => {
                vec![(session.id.to_string(), session.last_seq.saturating_sub(1))]
            }
            _ => vec![],
        };
        for (id, seq) in to_subscribe {
            if subscribed.insert(id.clone()) {
                let m = json!({"type": "subscribe", "session": id, "since_seq": seq});
                ws.send(Message::Text(m.to_string().into())).await?;
            }
        }
        match msg {
            ServerMsg::Event { event } => {
                if let EventKind::PermissionRequested { request, tool, .. } = &event.kind {
                    eprintln!("fake-phone: permission requested: {}", tool.title);
                    tokio::time::sleep(Duration::from_millis(after)).await;
                    let m = json!({"type": "command", "session": event.session,
                                   "command": {"type": "resolve_permission", "request": request, "option": answer}});
                    ws.send(Message::Text(m.to_string().into())).await?;
                    eprintln!("fake-phone: answered '{answer}'");
                }
                if let EventKind::PermissionResolved { outcome, by, .. } = &event.kind {
                    eprintln!("fake-phone: resolved {outcome:?} by {by:?}");
                    answered += 1;
                    if answered >= count {
                        break;
                    }
                }
            }
            ServerMsg::Error { message } => eprintln!("fake-phone: error: {message}"),
            _ => {}
        }
    }
    println!("{}", paired.device_id);
    Ok(())
}
