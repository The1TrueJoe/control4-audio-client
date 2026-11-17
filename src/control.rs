//! The TCP control channel Control4's streamer drives (port 6100). IMAP-style lines:
//! `<tag> <cmd> [args]` answered by `<tag> OK [data]`, `<tag> NO <msg>` or `<tag> BAD <cmd> <msg>`.

use crate::zone::{Session, Zones};
use std::net::IpAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

pub async fn serve(addr: String, zones: Arc<Zones>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(&addr).await?;
    eprintln!("control channel on {addr}");
    loop {
        let (sock, peer) = listener.accept().await?;
        let zones = zones.clone();
        tokio::spawn(async move {
            if let Err(e) = conn(sock, peer.ip(), &zones).await {
                eprintln!("control {peer}: {e}");
            }
        });
    }
}

async fn conn(sock: TcpStream, peer: IpAddr, zones: &Arc<Zones>) -> std::io::Result<()> {
    let (r, mut w) = sock.into_split();
    let mut lines = BufReader::new(r).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim_end_matches('\r');
        if !line.is_empty() {
            let reply = handle(line, peer, zones).await;
            eprintln!("{peer} > {line}\n{peer} < {reply}");
            w.write_all(format!("{reply}\r\n").as_bytes()).await?;
        }
    }
    Ok(())
}

fn no_channel() -> String {
    "no such zone".into()
}

fn on_off(s: Option<&&str>) -> Result<bool, String> {
    match s.map(|s| s.to_ascii_uppercase()).as_deref() {
        Some("ON") => Ok(true),
        Some("OFF") => Ok(false),
        _ => Err("state must be ON or OFF".into()),
    }
}

fn percent(s: Option<&&str>) -> Result<u8, String> {
    s.and_then(|v| v.parse::<u8>().ok()).filter(|v| *v <= 100).ok_or_else(|| "volume must be 0-100".into())
}

pub async fn handle(line: &str, peer: IpAddr, zones: &Arc<Zones>) -> String {
    let mut parts = line.splitn(3, ' ');
    let (Some(tag), Some(cmd)) = (parts.next(), parts.next().filter(|c| !c.is_empty())) else {
        return "* BAD expected: <tag> <command> [args]".into();
    };
    let rest = parts.next().unwrap_or("").trim();
    let args: Vec<&str> = rest.split_whitespace().collect();
    let zone = || args.first().and_then(|p| p.parse::<u16>().ok()).and_then(|p| zones.get(p)).ok_or_else(no_channel);

    let result: Result<String, String> = match cmd.to_ascii_lowercase().as_str() {
        "session" => match Session::parse(rest) {
            Ok(s) => match zones.for_session(s.port) {
                Some(z) => tokio::task::spawn_blocking(move || z.session(s, peer))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
                    .map(|_| "session_reply;".into()),
                None => Err("no zone for this port".into()),
            },
            Err(e) => Err(e),
        },
        "ping" | "initialize" | "reset" | "status" | "listparams" | "getparam" | "setparam" => Ok(String::new()),
        "vers" => Ok(env!("CARGO_PKG_VERSION").into()),
        "addch" | "remch" => zone().map(|_| String::new()),
        "setvol" => zone().and_then(|z| {
            let (l, r) = (percent(args.get(1))?, percent(args.get(2).or(args.get(1)))?);
            z.set_volume(((u16::from(l) + u16::from(r)) / 2) as u8); // one level per zone: average L and R
            Ok(String::new())
        }),
        "getvol" => zone().map(|z| z.view().volume.to_string()),
        "setmaxvol" => zone().and_then(|z| Ok(z.set_max_volume(percent(args.get(1))?))).map(|_| String::new()),
        "getmaxvol" => zone().map(|z| z.view().max_volume.to_string()),
        "setmut" => zone().and_then(|z| Ok(z.set_muted(on_off(args.get(1))?))).map(|_| String::new()),
        "togmut" => zone().map(|z| z.set_muted(!z.view().muted)).map(|_| String::new()),
        "getmut" => zone().map(|z| if z.view().muted { "ON" } else { "OFF" }.into()),
        "ramp" => zone().map(|z| z.ramp(args.get(1).copied().unwrap_or("OFF"))).map(|_| String::new()),
        _ => return format!("{tag} BAD {cmd} unknown command"),
    };
    match result {
        Ok(d) if d.is_empty() => format!("{tag} OK"),
        Ok(d) => format!("{tag} OK {d}"),
        Err(e) => format!("{tag} NO {e}"),
    }
}
