//! `styr net`: how the phone reaches this Mac, and a Tailscale restart.

use anyhow::{Result, bail};
use std::time::{Duration, Instant};
use styr_core::ipc::Response;
use styr_core::tailscale;

fn reachable(diag: Option<&Response>) -> (Vec<String>, Vec<String>) {
    match diag {
        Some(Response::Diagnostics {
            api_urls, lan_urls, ..
        }) => (
            api_urls
                .iter()
                .filter(|u| !u.contains("127.0.0.1"))
                .cloned()
                .collect(),
            lan_urls.clone(),
        ),
        _ => (vec![], vec![]),
    }
}

pub fn status(diag: Option<&Response>) {
    match tailscale::cli().and_then(|c| tailscale::state(&c)) {
        Some(st) => {
            println!("Tailscale      {}  {}", st.backend, st.ips.join(" "));
            for h in &st.health {
                println!("               ! {h}");
            }
        }
        None => println!("Tailscale      not found"),
    }
    if diag.is_none() {
        println!("styrd          not answering");
        return;
    }
    let (ts, lan) = reachable(diag);
    if ts.is_empty() {
        println!("Over Tailscale none: the phone can't reach this Mac over Tailscale");
        println!("               (`styr net restart` restarts Tailscale)");
    } else {
        println!("Over Tailscale {}", ts.join("\n               "));
    }
    if lan.is_empty() {
        println!("Same Wi-Fi     none");
    } else {
        println!("Same Wi-Fi     {}", lan.join("\n               "));
    }
}

pub fn restart(diag: impl Fn() -> Option<Response>) -> Result<()> {
    let Some(cli) = tailscale::cli() else {
        bail!("Tailscale is not installed")
    };
    println!("Restarting Tailscale…");
    tailscale::restart(&cli).map_err(anyhow::Error::msg)?;
    // styrd rescans addresses every 15 s.
    let until = Instant::now() + Duration::from_secs(40);
    while Instant::now() < until {
        let (ts, _) = reachable(diag().as_ref());
        if !ts.is_empty() {
            println!("Back: the phone can reach this Mac at {}", ts[0]);
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    status(diag().as_ref());
    bail!("Tailscale restarted, but styrd has no Tailscale address yet")
}
