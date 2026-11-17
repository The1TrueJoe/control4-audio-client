//! SDDP, Control4's discovery protocol. Announcing this Type lets Director pair the
//! box with the driver whose `search_types` lists it and fill in its network address.

use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::time::{Duration, Instant};

const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const PORT: u16 = 1902;
/// Must match `<search_types>` in driver/driver.xml.
pub const TYPE: &str = "c4ac:audio_client";
const ANNOUNCE_EVERY: Duration = Duration::from_secs(60);

fn local_ip() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((GROUP, PORT)).ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(_) => None,
    }
}

fn message(first_line: &str, ip: Ipv4Addr, host: &str) -> String {
    format!(
        "{first_line}\r\nFrom: \"{ip}:{PORT}\"\r\nHost: \"{host}\"\r\nMax-Age: 1800\r\nType: \"{TYPE}\"\r\n\
         Primary-Proxy: \"avswitch\"\r\nProxies: \"avswitch\"\r\nManufacturer: \"Open Source\"\r\n\
         Model: \"c4-audio-client\"\r\nDriver: \"c4-audio-client.c4z\"\r\n\r\n"
    )
}

/// True for a SEARCH aimed at everything or at our Type.
fn wants_us(packet: &str) -> bool {
    packet.starts_with("SEARCH ")
        && packet
            .lines()
            .find_map(|l| l.strip_prefix("Type:"))
            .map_or(true, |t| matches!(t.trim().trim_matches('"'), "*" | TYPE))
}

pub fn spawn(host: String) {
    std::thread::spawn(move || {
        if let Err(e) = serve(&host) {
            eprintln!("sddp disabled: {e}");
        }
    });
}

fn serve(host: &str) -> std::io::Result<()> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, PORT))?;
    sock.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED)?;
    sock.set_read_timeout(Some(Duration::from_secs(5)))?;
    eprintln!("sddp announcing {TYPE} as {host}");
    let mut last: Option<Instant> = None;
    let mut buf = [0u8; 2048];
    loop {
        // Re-read each time: the address can change under DHCP.
        let ip = local_ip().unwrap_or(Ipv4Addr::UNSPECIFIED);
        if last.map_or(true, |t| t.elapsed() >= ANNOUNCE_EVERY) {
            sock.send_to(message("NOTIFY ALIVE SDDP/1.0", ip, host).as_bytes(), (GROUP, PORT))?;
            last = Some(Instant::now());
        }
        match sock.recv_from(&mut buf) {
            Ok((n, from)) if wants_us(&String::from_utf8_lossy(&buf[..n])) => {
                sock.send_to(message("SDDP/1.0 200 OK", ip, host).as_bytes(), from)?;
            }
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_matching_searches_only() {
        assert!(wants_us("SEARCH * SDDP/1.0\r\nHost: \"10.0.0.1:1902\"\r\n\r\n"));
        assert!(wants_us("SEARCH * SDDP/1.0\r\nType: \"c4ac:audio_client\"\r\n\r\n"));
        assert!(wants_us("SEARCH * SDDP/1.0\r\nType: \"*\"\r\n\r\n"));
        assert!(!wants_us("SEARCH * SDDP/1.0\r\nType: \"c4:director\"\r\n\r\n"));
        assert!(!wants_us("NOTIFY ALIVE SDDP/1.0\r\nType: \"c4ac:audio_client\"\r\n\r\n"));
        let m = message("NOTIFY ALIVE SDDP/1.0", Ipv4Addr::new(10, 0, 0, 5), "box");
        assert!(m.contains("From: \"10.0.0.5:1902\"") && m.ends_with("\r\n\r\n"));
    }
}
