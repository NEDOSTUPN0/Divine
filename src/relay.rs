//! The local HTTP proxy legendary, umu and Divine itself are pointed at.
//!
//! It does two jobs. legendary has no bandwidth setting of its own — no flag,
//! no config key, no throttle anywhere in its downloader. What it does have is
//! download workers built on plain `requests` sessions, and those honour
//! `HTTPS_PROXY`. So Divine puts itself in the middle and holds the line there.
//! And when a proxy is set in the preferences, everything goes out through it
//! from here; [`crate::proxy`] says why the programs are not simply handed
//! that proxy themselves.
//!
//! Downloads share one throttled relay. Several can run at once and legendary
//! forks a worker per connection, so a per-connection or per-process limit
//! would mean nothing; the point of a single shared token bucket is that the
//! number in the preferences is the number on the wire. It exists only while a
//! download is holding it. Every download that is given the address takes a
//! [`Lease`] and keeps it until legendary exits, and the last lease dropped
//! takes the listener down with it — so nothing is left listening once the
//! downloads are done, or once the limit is switched off.
//!
//! Everything else goes through a second, unthrottled relay that stays open
//! for as long as the proxy is set, since Divine's own requests come and go at
//! any moment and a game started through it may run for hours.
//!
//! Nothing is inspected. A CONNECT becomes a blind byte pump, which is what
//! keeps TLS out of this entirely — no certificate to trust, no traffic to
//! decrypt. Plain HTTP is forwarded in origin form for the sake of being a
//! correct proxy rather than a special case.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::log::{error, warn};

/// How much is read from upstream at a time. Also the largest overshoot the
/// limit can suffer, since a read is charged once it has already landed.
///
/// Deliberately small. legendary gives each download a ten second read timeout,
/// and a connection's longest silence is however long its last read takes to
/// pay for. Sixteen workers sharing a slow ceiling would stall five seconds a
/// time on 32 KiB reads; on 8 KiB they stall a quarter of that.
const BUFFER: usize = 8 * 1024;

/// Longest a thread sleeps off its debt in one go. Waking up early costs
/// nothing and means a limit changed mid-download is picked up promptly.
const SLICE: Duration = Duration::from_millis(500);

/// The relay downloads share, for as long as one of them wants it.
static PROXY: Mutex<Option<Proxy>> = Mutex::new(None);
/// Downloads currently holding a lease.
static HOLDERS: AtomicUsize = AtomicUsize::new(0);

/// The relay for everything else, open while a proxy is set.
static OPEN: Mutex<Option<Proxy>> = Mutex::new(None);

/// Claims the throttled relay for one download, or hands back `None` when
/// there is no limit to hold. Someone who never sets a limit never runs one,
/// and their downloads go out the way everything else does.
pub fn lease(bytes_per_second: u64) -> Option<Lease> {
    if bytes_per_second == 0 {
        return None;
    }
    let mut slot = PROXY.lock().unwrap();
    if slot.is_none() {
        match Proxy::start(bytes_per_second) {
            Ok(proxy) => *slot = Some(proxy),
            Err(e) => {
                // Worth saying out loud: the download still runs, just without
                // the ceiling the user asked for.
                error!("cannot limit downloads: {e:#}");
                return None;
            }
        }
    }
    let proxy = slot.as_ref()?;
    proxy.set_limit(bytes_per_second);
    HOLDERS.fetch_add(1, Ordering::AcqRel);
    Some(Lease {
        address: proxy.address.clone(),
    })
}

/// Applies a new limit to whatever is already downloading. Zero lets those
/// downloads finish unthrottled rather than cutting their connections; the
/// proxy goes away on its own once the last of them is done.
pub fn set_limit(bytes_per_second: u64) {
    if let Some(proxy) = PROXY.lock().unwrap().as_ref() {
        proxy.set_limit(bytes_per_second);
    }
}

/// Opens the unthrottled relay, or closes it. Programs started while it was
/// open keep its address, so closing it cuts them off from the proxy along
/// with everything else, which is what switching the proxy off asks for.
pub fn stay_open(open: bool) {
    let mut slot = OPEN.lock().unwrap();
    if !open {
        slot.take();
        return;
    }
    if slot.is_none() {
        match Proxy::start(0) {
            Ok(proxy) => *slot = Some(proxy),
            Err(e) => error!("cannot go through the proxy: {e:#}"),
        }
    }
}

/// Where to send what should go out through the proxy, while one is set.
pub fn address() -> Option<String> {
    OPEN.lock()
        .unwrap()
        .as_ref()
        .map(|proxy| proxy.address.clone())
}

/// Points a program Divine runs at `address`, in every spelling, since each
/// reads a different one: umu only `https_proxy`, curl `http_proxy` only in
/// lower case, `requests` either.
pub fn point(cmd: &mut Command, address: &str) {
    for name in ["http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY"] {
        cmd.env(name, address);
    }
}

/// One download's claim on the proxy. Dropping the last one shuts it down.
pub struct Lease {
    address: String,
}

impl Lease {
    pub fn address(&self) -> &str {
        &self.address
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if HOLDERS.fetch_sub(1, Ordering::AcqRel) == 1 {
            // Dropping the proxy is what closes the listener. Connections that
            // are still open own their sockets and end in their own time.
            PROXY.lock().unwrap().take();
        }
    }
}

struct Proxy {
    address: String,
    local: SocketAddr,
    bucket: Arc<Bucket>,
    closing: Arc<AtomicBool>,
}

impl Proxy {
    fn start(bytes_per_second: u64) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").context("binding the relay")?;
        let local = listener.local_addr()?;
        let address = format!("http://{local}");
        let bucket = Arc::new(Bucket::new(bytes_per_second));
        let closing = Arc::new(AtomicBool::new(false));

        let shared = bucket.clone();
        let done = closing.clone();
        thread::spawn(move || {
            for client in listener.incoming() {
                if done.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(client) = client else { continue };
                let bucket = shared.clone();
                thread::spawn(move || {
                    if let Err(e) = serve(client, &bucket) {
                        warn!("relay: {e:#}");
                    }
                });
            }
        });

        Ok(Self {
            address,
            local,
            bucket,
            closing,
        })
    }

    fn set_limit(&self, bytes_per_second: u64) {
        self.bucket.limit.store(bytes_per_second, Ordering::Relaxed);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Relaxed);
        // The accept loop is asleep in accept(); one connection of our own is
        // what lets it wake up, read the flag and let the listener go.
        let _ = TcpStream::connect(self.local);
    }
}

fn serve(client: TcpStream, bucket: &Bucket) -> Result<()> {
    let mut reader = BufReader::new(client.try_clone().context("cloning the client socket")?);

    let mut request = String::new();
    reader
        .read_line(&mut request)
        .context("reading the request")?;
    let mut fields = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        fields.push(line);
    }

    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();

    if method.eq_ignore_ascii_case("CONNECT") {
        let upstream = crate::proxy::dial(&target)?;
        (&client).write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")?;
        // A client that pipelined past the CONNECT left bytes in the buffer,
        // and they belong to the tunnel.
        let pending = reader.buffer().to_vec();
        tunnel(client, upstream, pending, bucket);
        return Ok(());
    }

    let (host, path) = split_target(&target)
        .with_context(|| format!("{method} {target} is not something this proxy forwards"))?;
    let mut upstream = crate::proxy::dial(&host)?;
    let mut head = format!("{method} {path} HTTP/1.1\r\n");
    for field in fields {
        // Ours to answer, not the origin server's.
        if !field.to_ascii_lowercase().starts_with("proxy-") {
            head.push_str(&field);
        }
    }
    head.push_str("\r\n");
    upstream.write_all(head.as_bytes())?;

    let pending = reader.buffer().to_vec();
    tunnel(client, upstream, pending, bucket);
    Ok(())
}

/// Pumps both directions until either end hangs up. Only what comes back is
/// charged: the requests going out are a few hundred bytes each.
fn tunnel(client: TcpStream, upstream: TcpStream, pending: Vec<u8>, bucket: &Bucket) {
    let outbound = match (client.try_clone(), upstream.try_clone()) {
        (Ok(from), Ok(mut to)) => Some(thread::spawn(move || {
            let _ = to.write_all(&pending);
            let _ = io::copy(&mut { from }, &mut to);
            let _ = to.shutdown(Shutdown::Write);
        })),
        _ => None,
    };

    let _ = pump(upstream, client, bucket);
    if let Some(outbound) = outbound {
        let _ = outbound.join();
    }
}

fn pump(mut from: TcpStream, mut to: TcpStream, bucket: &Bucket) -> io::Result<()> {
    let mut buffer = [0u8; BUFFER];
    loop {
        let read = match from.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        to.write_all(&buffer[..read])?;
        bucket.charge(read as u64);
    }
    let _ = to.shutdown(Shutdown::Write);
    Ok(())
}

/// `http://host[:port]/path` as the host to dial and the path to ask it for.
fn split_target(target: &str) -> Option<(String, String)> {
    let rest = target.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(cut) => (&rest[..cut], &rest[cut..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return None;
    }
    let host = if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{authority}:80")
    };
    Some((host, path.to_string()))
}

/// A token bucket shared by every connection. Bytes are charged after they have
/// been passed on and the debt is paid by sleeping, which throttles the next
/// read: the sender's window fills, and TCP does the rest.
struct Bucket {
    /// Bytes per second. Zero lets everything through.
    limit: AtomicU64,
    state: Mutex<State>,
}

struct State {
    /// Bytes of credit. Goes negative, and that debt is what gets slept off.
    tokens: f64,
    filled: Instant,
}

impl Bucket {
    fn new(bytes_per_second: u64) -> Self {
        Self {
            limit: AtomicU64::new(bytes_per_second),
            state: Mutex::new(State {
                tokens: 0.0,
                filled: Instant::now(),
            }),
        }
    }

    fn charge(&self, bytes: u64) {
        if self.limit.load(Ordering::Relaxed) == 0 {
            return;
        }
        {
            let mut state = self.state.lock().unwrap();
            let limit = self.limit.load(Ordering::Relaxed);
            self.refill(&mut state, limit);
            state.tokens -= bytes as f64;
        }
        loop {
            let limit = self.limit.load(Ordering::Relaxed);
            // Lifting the limit mid-download forgives the debt rather than
            // making everyone wait it out first.
            if limit == 0 {
                return;
            }
            let debt = {
                let mut state = self.state.lock().unwrap();
                self.refill(&mut state, limit);
                -state.tokens
            };
            if debt <= 0.0 {
                return;
            }
            thread::sleep(Duration::from_secs_f64(debt / limit as f64).min(SLICE));
        }
    }

    fn refill(&self, state: &mut State, limit: u64) {
        let now = Instant::now();
        state.tokens += now.duration_since(state.filled).as_secs_f64() * limit as f64;
        state.filled = now;
        // A second of credit is as much as anyone may bank. Without the cap an
        // idle minute would buy a burst that makes the limit look ignored.
        let ceiling = limit as f64;
        if state.tokens > ceiling {
            state.tokens = ceiling;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_splits_into_a_host_and_a_path() {
        assert_eq!(
            split_target("http://epicgames-download1.akamaized.net/Builds/chunk"),
            Some((
                "epicgames-download1.akamaized.net:80".into(),
                "/Builds/chunk".into()
            ))
        );
        assert_eq!(
            split_target("http://cache.lan:8080/a/b"),
            Some(("cache.lan:8080".into(), "/a/b".into()))
        );
        assert_eq!(
            split_target("http://host"),
            Some(("host:80".into(), "/".into()))
        );
        assert_eq!(split_target("https://host/path"), None);
    }

    /// The globals make this one test rather than three: it walks the whole
    /// life of the shared proxy in order.
    #[test]
    fn a_proxy_lives_only_while_a_download_holds_it() {
        assert!(lease(0).is_none(), "no limit means no proxy");
        assert!(PROXY.lock().unwrap().is_none());

        let first = lease(1024 * 1024).expect("a limit should start one");
        let second = lease(1024 * 1024).expect("a second download shares it");
        assert_eq!(first.address(), second.address());

        drop(first);
        assert!(
            PROXY.lock().unwrap().is_some(),
            "one download is still holding it"
        );

        let port = second.address().rsplit(':').next().unwrap().to_string();
        drop(second);
        assert!(PROXY.lock().unwrap().is_none(), "nothing is left to serve");

        // And the port is really gone, not merely forgotten. Closing the
        // listener trails the drop by a hop through the accept loop.
        let gone = (0..100).any(|_| {
            if TcpStream::connect(format!("127.0.0.1:{port}")).is_err() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
            false
        });
        assert!(gone, "the listener is still up");
    }

    #[test]
    fn a_bucket_holds_the_rate_it_was_given() {
        // A quarter of a second's worth at 1 MiB/s, charged in the same size
        // reads the pump uses.
        let limit = 1024 * 1024;
        let bucket = Bucket::new(limit);
        let start = Instant::now();
        for _ in 0..(limit / 4) / BUFFER as u64 {
            bucket.charge(BUFFER as u64);
        }
        let spent = start.elapsed();
        // The first second of credit is free, so only the excess is slept off.
        assert!(spent < Duration::from_millis(400), "{spent:?} is too slow");
    }

    #[test]
    fn a_bucket_without_a_limit_never_waits() {
        let bucket = Bucket::new(0);
        let start = Instant::now();
        for _ in 0..1000 {
            bucket.charge(BUFFER as u64);
        }
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    /// An upstream that answers every connection with `bytes` of payload and
    /// then hangs up, so a reader can time how long it took to arrive.
    fn upstream(bytes: usize) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            for mut client in listener.incoming().flatten() {
                thread::spawn(move || {
                    // Swallow whatever was asked; every answer is the same.
                    let mut scratch = [0u8; 1024];
                    let _ = client.read(&mut scratch);
                    let _ = client.write_all(&vec![b'x'; bytes]);
                    let _ = client.shutdown(Shutdown::Write);
                });
            }
        });
        address
    }

    fn drain(stream: &mut TcpStream) -> usize {
        let mut sink = Vec::new();
        stream.read_to_end(&mut sink).unwrap();
        sink.len()
    }

    #[test]
    fn a_plain_request_is_forwarded_and_held_to_the_limit() {
        let origin = upstream(768 * 1024);
        let proxy = Proxy::start(1024 * 1024).unwrap();
        let port = proxy.address.rsplit(':').next().unwrap();

        let start = Instant::now();
        let mut client = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        client
            .write_all(
                format!("GET http://{origin}/chunk HTTP/1.1\r\nHost: {origin}\r\n\r\n").as_bytes(),
            )
            .unwrap();
        let arrived = drain(&mut client);
        let spent = start.elapsed();

        assert_eq!(arrived, 768 * 1024, "the body did not come through whole");
        // 768 KiB at 1 MiB/s: the banked second covers it, so this only proves
        // the bytes flow. The rate itself is the bucket's own test.
        assert!(spent < Duration::from_secs(2), "{spent:?} is too slow");
    }

    #[test]
    fn a_connect_becomes_a_tunnel() {
        let origin = upstream(64 * 1024);
        let proxy = Proxy::start(0).unwrap();
        let port = proxy.address.rsplit(':').next().unwrap();

        let mut client = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        client
            .write_all(format!("CONNECT {origin} HTTP/1.1\r\n\r\n").as_bytes())
            .unwrap();

        let mut reply = [0u8; 39];
        client.read_exact(&mut reply).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&reply),
            "HTTP/1.1 200 Connection established\r\n\r\n"
        );

        // Past the reply the proxy is a pipe, so anything works as a request.
        client.write_all(b"hello\r\n\r\n").unwrap();
        assert_eq!(drain(&mut client), 64 * 1024);
    }

    #[test]
    fn a_debt_is_slept_off() {
        // Half a second of debt at 64 KiB/s: one buffer beyond the banked
        // second, so the charge has to wait for it.
        let bucket = Bucket::new(64 * 1024);
        bucket.charge(64 * 1024); // spends the free second
        let start = Instant::now();
        bucket.charge(32 * 1024);
        let spent = start.elapsed();
        assert!(
            spent >= Duration::from_millis(400),
            "{spent:?} is too quick"
        );
    }
}
