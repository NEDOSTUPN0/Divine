//! The proxy set in the preferences, and how to reach the rest of the internet
//! through it.
//!
//! Only [`crate::relay`] dials through it. legendary's `requests` cannot speak
//! SOCKS without PySocks, which nothing guarantees is installed, and umu reads
//! nothing but an HTTP proxy in `https_proxy`. So legendary, umu and Divine's
//! own requests are all pointed at the relay instead, which speaks plain HTTP
//! proxy to every one of them and makes the one connection that matters here.
//! That also keeps the password out of every child's environment, where
//! `/proc/<pid>/environ` and the launch log would show it.
//!
//! SOCKS5 hands the proxy the name rather than an address. The point of the
//! proxy is getting past a network that blocks things, and a name resolved
//! on that network may already be the wrong answer.

use std::io::{Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::sync::RwLock;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::config::{ProxyKind, ProxySettings};

/// How long the proxy gets to answer each step of setting up a connection.
const HANDSHAKE: Duration = Duration::from_secs(15);

/// The proxy in use, or `None` for going out directly.
static IN_USE: RwLock<Option<ProxySettings>> = RwLock::new(None);

/// Takes up what the preferences now say. A connection already open stays on
/// the route it was opened on.
pub fn apply(settings: &ProxySettings) {
    let chosen = settings.is_set().then(|| settings.clone());
    let on = chosen.is_some();
    *IN_USE.write().unwrap() = chosen;
    crate::relay::stay_open(on);
}

/// Connects to `target`, a `host:port`, through the proxy when there is one.
pub fn dial(target: &str) -> Result<TcpStream> {
    let proxy = IN_USE.read().unwrap().clone();
    match proxy {
        Some(proxy) => through(&proxy, target),
        None => TcpStream::connect(target).with_context(|| format!("connecting to {target}")),
    }
}

fn through(proxy: &ProxySettings, target: &str) -> Result<TcpStream> {
    let mut stream = connect(&proxy.host, proxy.port)
        .with_context(|| format!("connecting to the proxy at {}:{}", proxy.host, proxy.port))?;
    stream.set_read_timeout(Some(HANDSHAKE))?;
    stream.set_write_timeout(Some(HANDSHAKE))?;
    match proxy.kind {
        ProxyKind::Http => http_connect(&mut stream, target, proxy),
        ProxyKind::Socks5 => socks5(&mut stream, target, proxy),
    }
    .with_context(|| format!("asking the proxy for {target}"))?;
    // From here on it is the tunnel, and whoever reads it keeps their own time.
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    Ok(stream)
}

fn connect(host: &str, port: u16) -> Result<TcpStream> {
    let mut last = None;
    for address in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, HANDSHAKE) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = Some(e),
        }
    }
    match last {
        Some(e) => Err(e.into()),
        None => bail!("{host} has no address"),
    }
}

fn http_connect(stream: &mut TcpStream, target: &str, proxy: &ProxySettings) -> Result<()> {
    let mut ask = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if !proxy.username.is_empty() {
        let credentials = base64(format!("{}:{}", proxy.username, proxy.password).as_bytes());
        ask.push_str(&format!("Proxy-Authorization: Basic {credentials}\r\n"));
    }
    ask.push_str("\r\n");
    stream.write_all(ask.as_bytes())?;

    let head = read_head(stream)?;
    let status = head.lines().next().unwrap_or_default();
    match status.split_whitespace().nth(1) {
        // Any 2xx opens the tunnel, not only 200.
        Some(code) if code.starts_with('2') => Ok(()),
        Some("407") if proxy.username.is_empty() => {
            bail!("the proxy wants a username and password")
        }
        Some("407") => bail!("the proxy did not accept the username and password"),
        _ => bail!("the proxy answered {status}"),
    }
}

/// The response head, and not a byte past it: what follows belongs to the
/// tunnel and must be left on the socket for whoever reads it next.
fn read_head(stream: &mut TcpStream) -> Result<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            bail!("the proxy answered with an endless header");
        }
        if stream.read(&mut byte)? == 0 {
            bail!("the proxy hung up");
        }
        head.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

/// RFC 1928, with the sign-in from RFC 1929.
fn socks5(stream: &mut TcpStream, target: &str, proxy: &ProxySettings) -> Result<()> {
    let (host, port) = host_and_port(target).with_context(|| format!("{target} has no port"))?;
    let signs_in = !proxy.username.is_empty();
    // No sign-in is offered next to the password, so a proxy that needs none
    // is not made to check one.
    stream.write_all(if signs_in { &[5, 2, 0, 2] } else { &[5, 1, 0] })?;
    let mut chosen = [0u8; 2];
    stream.read_exact(&mut chosen)?;
    if chosen[0] != 5 {
        bail!("the proxy does not speak SOCKS5");
    }
    match chosen[1] {
        0 => {}
        2 if signs_in => sign_in(stream, proxy)?,
        2 => bail!("the proxy wants a username and password"),
        _ => bail!("the proxy accepts none of the ways Divine can sign in"),
    }

    let mut ask = vec![5, 1, 0];
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            ask.push(1);
            ask.extend(ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            ask.push(4);
            ask.extend(ip.octets());
        }
        Err(_) => {
            let length = u8::try_from(host.len()).context("the name is too long for SOCKS5")?;
            ask.extend([3, length]);
            ask.extend(host.as_bytes());
        }
    }
    ask.extend(port.to_be_bytes());
    stream.write_all(&ask)?;

    let mut reply = [0u8; 4];
    stream.read_exact(&mut reply)?;
    if reply[1] != 0 {
        bail!("the proxy could not connect: {}", socks_failure(reply[1]));
    }
    // The address the proxy connected from. Nobody here needs it, but it sits
    // between this reply and the tunnel.
    let bound = match reply[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut length = [0u8; 1];
            stream.read_exact(&mut length)?;
            length[0] as usize
        }
        other => bail!("the proxy answered with an address of unknown type {other}"),
    };
    let mut skipped = vec![0u8; bound + 2];
    stream.read_exact(&mut skipped)?;
    Ok(())
}

fn sign_in(stream: &mut TcpStream, proxy: &ProxySettings) -> Result<()> {
    let username = proxy.username.as_bytes();
    let password = proxy.password.as_bytes();
    let (Ok(username_length), Ok(password_length)) =
        (u8::try_from(username.len()), u8::try_from(password.len()))
    else {
        bail!("SOCKS5 takes a username and password of 255 bytes at most");
    };
    let mut ask = vec![1, username_length];
    ask.extend(username);
    ask.push(password_length);
    ask.extend(password);
    stream.write_all(&ask)?;

    let mut answer = [0u8; 2];
    stream.read_exact(&mut answer)?;
    if answer[1] != 0 {
        bail!("the proxy did not accept the username and password");
    }
    Ok(())
}

fn socks_failure(code: u8) -> String {
    match code {
        1 => "it failed".into(),
        2 => "its rules forbid it".into(),
        3 => "the network is unreachable".into(),
        4 => "the host is unreachable".into(),
        5 => "the connection was refused".into(),
        6 => "it took too long".into(),
        7 => "it does not support connecting".into(),
        8 => "it does not support that kind of address".into(),
        other => format!("error {other}"),
    }
}

/// `host:port`, or `[v6 address]:port`.
fn host_and_port(target: &str) -> Option<(&str, u16)> {
    let (host, port) = match target.strip_prefix('[') {
        Some(rest) => rest.split_once("]:")?,
        None => target.rsplit_once(':')?,
    };
    Some((host, port.parse().ok()?))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let joined = chunk
            .iter()
            .enumerate()
            .fold(0u32, |all, (i, byte)| all | (*byte as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(joined >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::{SocketAddr, TcpListener};
    use std::sync::mpsc;
    use std::thread;

    fn settings(
        kind: ProxyKind,
        address: SocketAddr,
        username: &str,
        password: &str,
    ) -> ProxySettings {
        ProxySettings {
            enabled: true,
            kind,
            host: address.ip().to_string(),
            port: address.port(),
            username: username.into(),
            password: password.into(),
        }
    }

    /// A proxy that plays one conversation: it answers each thing it is told to
    /// expect with the reply paired to it, reports what it actually got, and
    /// then says "hello" down the tunnel.
    fn proxy(steps: Vec<(usize, Vec<u8>)>) -> (SocketAddr, mpsc::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (heard, said) = mpsc::channel();
        thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap();
            for (expected, reply) in steps {
                let mut got = vec![0u8; expected];
                if client.read_exact(&mut got).is_err() {
                    return;
                }
                heard.send(got).unwrap();
                client.write_all(&reply).unwrap();
            }
            let _ = client.write_all(b"hello");
        });
        (address, said)
    }

    fn hello(mut stream: TcpStream) -> String {
        let mut text = String::new();
        stream.read_to_string(&mut text).unwrap();
        text
    }

    #[test]
    fn socks5_leaves_the_name_for_the_proxy_to_resolve() {
        let target = "launcher-public-service-prod06.ol.epicgames.com";
        let mut request = vec![5, 1, 0, 3, target.len() as u8];
        request.extend(target.as_bytes());
        request.extend(443u16.to_be_bytes());
        let (address, said) = proxy(vec![
            (3, vec![5, 0]),
            (request.len(), vec![5, 0, 0, 1, 10, 0, 0, 1, 0x1f, 0x90]),
        ]);

        let stream = through(
            &settings(ProxyKind::Socks5, address, "", ""),
            &format!("{target}:443"),
        )
        .unwrap();

        assert_eq!(said.recv().unwrap(), [5, 1, 0], "offered no sign-in only");
        assert_eq!(said.recv().unwrap(), request);
        assert_eq!(hello(stream), "hello", "the tunnel lost its first bytes");
    }

    #[test]
    fn socks5_signs_in_with_the_username_and_password() {
        let mut sign_in = vec![1, 4];
        sign_in.extend(b"nedo");
        sign_in.push(6);
        sign_in.extend(b"secret");
        let (address, said) = proxy(vec![
            (4, vec![5, 2]),
            (sign_in.len(), vec![1, 0]),
            (10, vec![5, 0, 0, 1, 0, 0, 0, 0, 0, 0]),
        ]);

        let stream = through(
            &settings(ProxyKind::Socks5, address, "nedo", "secret"),
            "203.0.113.7:443",
        )
        .unwrap();

        assert_eq!(said.recv().unwrap(), [5, 2, 0, 2]);
        assert_eq!(said.recv().unwrap(), sign_in);
        assert_eq!(said.recv().unwrap(), [5, 1, 0, 1, 203, 0, 113, 7, 1, 187]);
        assert_eq!(hello(stream), "hello");
    }

    #[test]
    fn a_refused_sign_in_says_so_without_the_password() {
        let (address, _said) = proxy(vec![(4, vec![5, 2]), (13, vec![1, 1])]);
        let e = through(
            &settings(ProxyKind::Socks5, address, "nedo", "secret"),
            "example.com:443",
        )
        .unwrap_err();
        let said = format!("{e:#}");
        assert!(
            said.contains("did not accept the username and password"),
            "{said}"
        );
        assert!(!said.contains("secret"), "{said}");
    }

    #[test]
    fn an_http_proxy_is_asked_with_basic_credentials() {
        // RFC 7617's own example.
        let ask = "CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\
                   Proxy-Authorization: Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==\r\n\r\n";
        let (address, said) = proxy(vec![(
            ask.len(),
            b"HTTP/1.1 200 Connection established\r\n\r\n".to_vec(),
        )]);

        let stream = through(
            &settings(ProxyKind::Http, address, "Aladdin", "open sesame"),
            "example.com:443",
        )
        .unwrap();

        assert_eq!(String::from_utf8(said.recv().unwrap()).unwrap(), ask);
        assert_eq!(hello(stream), "hello");
    }

    #[test]
    fn an_http_proxy_that_wants_a_password_says_so() {
        let ask = "CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n";
        let (address, _said) = proxy(vec![(
            ask.len(),
            b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n".to_vec(),
        )]);
        let e = through(
            &settings(ProxyKind::Http, address, "", ""),
            "example.com:443",
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("wants a username and password"));
    }

    #[test]
    fn a_target_splits_into_host_and_port() {
        assert_eq!(
            host_and_port("epicgames.com:443"),
            Some(("epicgames.com", 443))
        );
        assert_eq!(
            host_and_port("[2001:db8::1]:8080"),
            Some(("2001:db8::1", 8080))
        );
        assert_eq!(host_and_port("epicgames.com"), None);
    }

    #[test]
    fn base64_pads_what_does_not_fill_three_bytes() {
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
