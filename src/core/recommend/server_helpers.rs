//! LAN-IP / local-server-URL probing shared by the hook renderers.
//!
//! Rendered hook output embeds a `curl` URL the main agent should hit. These
//! helpers pick a teammate-reachable URL (loopback when the local dashboard is
//! bound, LAN IPv4 otherwise) so the curl never points at an interface the
//! server never listened on.

/// Best-effort detect the machine's outbound IPv4. Trick: open a UDP
/// socket and `connect` to a public IP — no packets fly, but the OS
/// picks the network interface it would route to, and `local_addr()`
/// returns that interface's IP. Returns None when offline / IPv6-only /
/// only loopback available.
///
/// Shared by `server::guess_server_url` (replace Host=loopback with LAN
/// IP) and the CLI hook path (`recommend()` + `cli::handle_recommend`
/// default server URL) so rendered hook output URLs are always
/// teammate-reachable, not loopback-only.
pub fn local_ipv4() -> Option<String> {
    use std::net::UdpSocket;
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    let ip = addr.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        return None;
    }
    Some(ip.to_string())
}

/// Default server URL used by CLI/library hook rendering when no remote
/// server is configured. Probes `127.0.0.1:17888` first — if the local
/// dashboard is listening on loopback (the safe default `runai server`
/// bind), the rendered hook URL stays on loopback so curl from the same
/// machine always works regardless of LAN interface state. Only falls
/// back to LAN IPv4 when loopback is not bound but a LAN interface is up
/// (e.g. user explicitly ran `--host 0.0.0.0` and disabled loopback for
/// some reason). Final fallback is loopback string when offline.
///
/// Root cause this fixes: server defaults to `127.0.0.1` bind, but the
/// previous URL builder unconditionally picked LAN IPv4 (e.g.
/// `192.168.0.93`). Main Claude then curl'd a LAN URL the server never
/// listened on → connection refused.
pub fn default_local_server_url() -> String {
    use std::net::TcpStream;
    use std::time::Duration;
    let probe = |host: &str| -> bool {
        format!("{host}:17888")
            .parse::<std::net::SocketAddr>()
            .ok()
            .and_then(|s| TcpStream::connect_timeout(&s, Duration::from_millis(80)).ok())
            .is_some()
    };
    if probe("127.0.0.1") {
        return "http://127.0.0.1:17888".to_string();
    }
    if let Some(ip) = local_ipv4() {
        if probe(&ip) {
            return format!("http://{ip}:17888");
        }
    }
    "http://127.0.0.1:17888".to_string()
}
