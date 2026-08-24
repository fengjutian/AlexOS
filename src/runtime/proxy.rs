//! Internal reverse proxy from the WebView to a service-mode backend.
//!
//! Alex OS exposes a single `alex://app/...` custom protocol in the
//! WebView; this module handles the `/api/*` slice of that space by
//! forwarding the request to the host-allocated loopback port
//! described by a [`ServiceEndpoint`]. The page never sees the
//! upstream port — it always uses `fetch('alex://app/api/...')` —
//! which means:
//!
//! - the page cannot reach a backend belonging to a different app
//!   (each `WebView` is bound to one endpoint at launch);
//! - the host can inject the per-launch shared secret without the
//!   page having to know it;
//! - CSP can stay `connect-src 'self'` because `alex://app/api/...`
//!   is same-origin to the page that already loaded the manifest.
//!
//! WebSocket traffic uses a capability-scoped loopback tunnel. The injected
//! WebView bridge rewrites `new WebSocket("alex://app/api/...")` to that
//! unguessable endpoint; the tunnel injects app identity and the backend token
//! before relaying the handshake and frames byte-for-byte.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

use wry::http::{Request, Response, StatusCode};

use crate::runtime::ServiceEndpoint;

/// Cap on the upstream request body. Larger payloads are rejected
/// with `413 Payload Too Large` before any bytes hit the wire. The
/// cap mirrors the 1 MiB limit on WebView → host IPC
/// (`src/ipc.rs::MAX_IPC_MESSAGE_BYTES`) so a hostile page cannot
/// tunnel a multi-gigabyte upload through a service backend.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const READ_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Loopback-only capability URL used to tunnel WebSocket handshakes and
/// frames to a service backend without exposing the backend token to JS.
pub struct WebSocketTunnel {
    pub base_url: String,
}

impl WebSocketTunnel {
    pub fn start(endpoint: ServiceEndpoint, app_id: String) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let mut secret = [0u8; 24];
        getrandom::fill(&mut secret).map_err(|error| std::io::Error::other(error.to_string()))?;
        let secret = secret
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let route_prefix = format!("/{secret}");
        let base_url = format!("ws://127.0.0.1:{port}{route_prefix}");
        std::thread::Builder::new()
            .name("alex-websocket-tunnel".into())
            .spawn(move || {
                for incoming in listener.incoming() {
                    let Ok(client) = incoming else { break };
                    let endpoint = endpoint.clone();
                    let app_id = app_id.clone();
                    let route_prefix = route_prefix.clone();
                    std::thread::spawn(move || {
                        let _ = relay_websocket(client, &endpoint, &app_id, &route_prefix);
                    });
                }
            })?;
        Ok(Self { base_url })
    }
}

fn relay_websocket(
    mut client: TcpStream,
    endpoint: &ServiceEndpoint,
    app_id: &str,
    route_prefix: &str,
) -> std::io::Result<()> {
    let _ = client.set_read_timeout(Some(READ_WRITE_TIMEOUT));
    let mut request = Vec::new();
    let mut chunk = [0u8; 2048];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 64 * 1024 {
        let read = client.read(&mut chunk)?;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(&chunk[..read]);
    }
    let request_text = std::str::from_utf8(&request)
        .map_err(|_| std::io::Error::other("non-UTF8 WebSocket handshake"))?;
    let (head, trailing) = request_text
        .split_once("\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("incomplete WebSocket handshake"))?;
    let mut lines = head.lines();
    let first = lines
        .next()
        .ok_or_else(|| std::io::Error::other("missing request line"))?;
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    if method != "GET" || !target.starts_with(route_prefix) {
        client.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")?;
        return Ok(());
    }
    let backend_target = &target[route_prefix.len()..];
    let mut upstream = TcpStream::connect(("127.0.0.1", endpoint.port))?;
    write!(
        upstream,
        "GET {backend_target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Alx-App-Id: {app_id}\r\nX-Alx-Token: {}\r\n",
        endpoint.port, endpoint.token
    )?;
    for line in lines {
        if !line.to_ascii_lowercase().starts_with("host:") {
            writeln!(upstream, "{line}\r")?;
        }
    }
    upstream.write_all(b"\r\n")?;
    upstream.write_all(trailing.as_bytes())?;
    upstream.flush()?;
    let mut client_read = client.try_clone()?;
    let mut upstream_write = upstream.try_clone()?;
    let upload = std::thread::spawn(move || std::io::copy(&mut client_read, &mut upstream_write));
    let _ = std::io::copy(&mut upstream, &mut client);
    let _ = upload.join();
    Ok(())
}

/// Request headers forwarded from the page. Anything not on this
/// list (`Host`, `Origin`, `Referer`, `Cookie`, `Sec-Fetch-*`, …)
/// is dropped so the page cannot smuggle cookies into a service
/// that did not opt in or impersonate the host page's origin.
const FORWARDED_REQUEST_HEADERS: &[&str] = &[
    "accept",
    "accept-language",
    "accept-encoding",
    "content-type",
    "authorization",
    "user-agent",
    "cache-control",
];

/// Response headers kept when relaying the upstream reply. The rest
/// (notably `Connection` and `Transfer-Encoding`) are stripped so
/// the WebView sees a plain HTTP/1.0-style reply it can interpret
/// without ambiguity.
const FORWARDED_RESPONSE_HEADERS: &[&str] = &[
    "content-type",
    "content-length",
    "cache-control",
    "etag",
    "last-modified",
    "expires",
    "vary",
];

/// Forward `request` to the service backend bound to `endpoint` and
/// return the upstream reply. `request_path` is the WebView's path
/// component (e.g. `/api/notes` for `alex://app/api/notes`) and is
/// forwarded verbatim to the upstream backend — backends are
/// expected to mount their HTTP routes under `/api/...` so the
/// page's URL is meaningful to the backend as well.
///
/// `app_id` is stamped on the upstream request as `X-Alx-App-Id` so
/// the backend can verify the host actually launched it.
pub fn proxy_to_service(
    endpoint: &ServiceEndpoint,
    app_id: &str,
    request_path: &str,
    request: &Request<Vec<u8>>,
) -> Response<Cow<'static, [u8]>> {
    if !request_path.starts_with('/') || request_path == "/" {
        return text_response(StatusCode::NOT_FOUND, "missing path");
    }
    let body = request.body();
    if body.len() > MAX_REQUEST_BYTES {
        return text_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body exceeds 1 MiB cap",
        );
    }
    let Some(addr) = resolve_loopback(endpoint.port) else {
        return text_response(StatusCode::BAD_GATEWAY, "cannot resolve backend address");
    };
    // (kept to preserve clippy suggestion shape — single-use helper)
    let mut stream = match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
        Ok(s) => s,
        Err(error) => {
            return text_response(
                StatusCode::BAD_GATEWAY,
                &format!("backend not reachable: {error}"),
            );
        }
    };
    let _ = stream.set_read_timeout(Some(READ_WRITE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_WRITE_TIMEOUT));

    let head = build_upstream_request(endpoint, app_id, request_path, request);
    if let Err(error) = stream.write_all(&head) {
        return text_response(
            StatusCode::BAD_GATEWAY,
            &format!("write to backend failed: {error}"),
        );
    }
    if !body.is_empty()
        && let Err(error) = stream.write_all(body)
    {
        return text_response(
            StatusCode::BAD_GATEWAY,
            &format!("write body to backend failed: {error}"),
        );
    }
    let _ = stream.flush();
    // Half-close the write side so the backend's read_to_end
    // returns as soon as our request body is fully sent. Without
    // this, HTTP/1.0 backends that read until EOF would hang
    // because TCP has no other way to know the request is done.
    let _ = stream.shutdown(Shutdown::Write);

    let mut raw = Vec::new();
    if let Err(error) = stream.read_to_end(&mut raw) {
        return text_response(
            StatusCode::BAD_GATEWAY,
            &format!("read from backend failed: {error}"),
        );
    }
    parse_upstream_response(&raw)
}

fn resolve_loopback(port: u16) -> Option<std::net::SocketAddr> {
    ("127.0.0.1", port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut iter| iter.next())
}

fn build_upstream_request(
    endpoint: &ServiceEndpoint,
    app_id: &str,
    request_path: &str,
    request: &Request<Vec<u8>>,
) -> Vec<u8> {
    let body_len = request.body().len();
    let mut head = String::with_capacity(256 + body_len);
    let _ = write!(
        head,
        "{method} {path} HTTP/1.0\r\n\
         Host: 127.0.0.1\r\n\
         X-Alx-App-Id: {app_id}\r\n\
         Connection: close\r\n\
         Content-Length: {body_len}\r\n",
        method = request.method(),
        path = request_path,
        app_id = app_id,
        body_len = body_len,
    );
    for (name, value) in request.headers() {
        if let Ok(value_str) = value.to_str()
            && FORWARDED_REQUEST_HEADERS.contains(&name.as_str())
        {
            let _ = write!(head, "{}: {}\r\n", name.as_str(), value_str);
        }
    }
    // Token is appended last and on its own line so that an
    // accidental dump of the head buffer never lands the secret in
    // a human-readable log line above the actual method line.
    let _ = write!(head, "X-Alx-Token: {}\r\n\r\n", endpoint.token);
    head.into_bytes()
}

fn parse_upstream_response(raw: &[u8]) -> Response<Cow<'static, [u8]>> {
    let Some(head_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return text_response(StatusCode::BAD_GATEWAY, "malformed upstream response");
    };
    let head_bytes = &raw[..head_end];
    let body = raw[head_end + 4..].to_vec();
    let head_str = match std::str::from_utf8(head_bytes) {
        Ok(s) => s,
        Err(_) => return text_response(StatusCode::BAD_GATEWAY, "non-utf8 upstream headers"),
    };
    let mut lines = head_str.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status_code: u16 = status_line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(502);
    let mut builder = Response::builder().status(status_code);
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            let value = value.trim();
            if FORWARDED_RESPONSE_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
                builder = builder.header(name, value);
            }
        }
    }
    builder.body(Cow::Owned(body)).unwrap_or_else(|_| {
        text_response(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")
    })
}

fn text_response(status: StatusCode, body: &str) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .header("x-content-type-options", "nosniff")
        .body(Cow::Owned(body.as_bytes().to_vec()))
        .expect("static text response is valid")
}

/// Returned by the WebView protocol handler when the page requests
/// `/api/...` but the host didn't launch a service-mode backend for
/// this app (e.g. a legacy RPC-only app or a frontend-only package).
/// The page sees a 503 with a stable error body it can branch on.
pub fn service_unavailable_response() -> Response<Cow<'static, [u8]>> {
    text_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "no service-mode backend is bound to this app",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[test]
    fn websocket_tunnel_injects_identity_and_returns_upgrade() {
        let backend = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = backend.local_addr().unwrap().port();
        let captured = Arc::new(Mutex::new(String::new()));
        let captured_thread = Arc::clone(&captured);
        thread::spawn(move || {
            let (mut stream, _) = backend.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            *captured_thread.lock().unwrap() = String::from_utf8(request).unwrap();
            stream.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n").unwrap();
        });
        let tunnel = WebSocketTunnel::start(
            ServiceEndpoint {
                port,
                token: "secret-token".into(),
            },
            "com.alex.test".into(),
        )
        .unwrap();
        let url = url::Url::parse(&format!("{}/api/socket", tunnel.base_url)).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", url.port().unwrap())).unwrap();
        write!(client, "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n", url.path()).unwrap();
        let mut response = [0u8; 128];
        let read = client.read(&mut response).unwrap();
        assert!(String::from_utf8_lossy(&response[..read]).contains("101 Switching Protocols"));
        let request = captured.lock().unwrap().clone();
        assert!(request.contains("GET /api/socket HTTP/1.1"));
        assert!(request.contains("X-Alx-App-Id: com.alex.test"));
        assert!(request.contains("X-Alx-Token: secret-token"));
    }

    /// Spawn a one-shot TCP server: it accepts a single connection,
    /// reads until EOF, writes `reply`, then drops the stream.
    /// Returns the bound port and a shared handle to the captured
    /// request bytes so the test can assert what the proxy sent.
    /// The proxy half-closes its write side, so read_to_end
    /// unblocks as soon as the request body is fully sent.
    fn spawn_one_shot_backend(reply: &'static [u8]) -> (u16, Arc<Mutex<Option<Vec<u8>>>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind 127.0.0.1:0");
        let port = listener.local_addr().unwrap().port();
        let captured: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
        let captured_for_thread = Arc::clone(&captured);
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = Vec::new();
                let _ = stream.read_to_end(&mut buf);
                *captured_for_thread.lock().unwrap() = Some(buf);
                let _ = stream.write_all(reply);
                let _ = stream.flush();
            }
        });
        (port, captured)
    }

    fn make_get_request(path: &str) -> Request<Vec<u8>> {
        Request::get(path).body(Vec::new()).expect("get request")
    }

    fn make_post_request(path: &str, body: &[u8]) -> Request<Vec<u8>> {
        Request::post(path)
            .header("content-type", "application/json")
            .body(body.to_vec())
            .expect("post request")
    }

    fn endpoint_for(port: u16) -> ServiceEndpoint {
        ServiceEndpoint {
            port,
            token: "deadbeef".repeat(8),
        }
    }

    #[test]
    fn proxy_returns_404_when_path_is_empty() {
        let endpoint = endpoint_for(1);
        let request = make_get_request("alex://app/api/");
        let response = proxy_to_service(&endpoint, "com.example", "", &request);
        assert_eq!(response.status().as_u16(), 404);
    }

    #[test]
    fn proxy_returns_502_when_backend_unreachable() {
        // Pick a port that nothing should be listening on. 1 is
        // privileged on most systems; the connect will fail fast
        // and we expect 502 from the proxy.
        let endpoint = endpoint_for(1);
        let request = make_get_request("alex://app/api/health");
        let response = proxy_to_service(&endpoint, "com.example", "/api/health", &request);
        assert_eq!(response.status().as_u16(), 502);
    }

    #[test]
    fn proxy_rejects_body_above_one_mib() {
        let endpoint = endpoint_for(1);
        let huge = vec![b'x'; MAX_REQUEST_BYTES + 1];
        let request = make_post_request("alex://app/api/notes", &huge);
        let response = proxy_to_service(&endpoint, "com.example", "/api/notes", &request);
        assert_eq!(response.status().as_u16(), 413);
    }

    #[test]
    fn proxy_forwards_get_and_preserves_status() {
        let reply: &'static [u8] = b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: 17\r\n\r\n{\"status\":\"ready\"}";
        let (port, captured) = spawn_one_shot_backend(reply);
        let endpoint = endpoint_for(port);
        let request = make_get_request("alex://app/api/health");
        let response = proxy_to_service(&endpoint, "com.example.test", "/api/health", &request);
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.body().as_ref(), b"{\"status\":\"ready\"}");
        let sent = captured.lock().unwrap().clone().expect("captured");
        let sent_str = std::str::from_utf8(&sent).expect("utf8 request");
        assert!(sent_str.starts_with("GET /api/health HTTP/1.0\r\n"));
        assert!(sent_str.contains("X-Alx-App-Id: com.example.test\r\n"));
        assert!(sent_str.contains("X-Alx-Token: "));
        // Token must match what the endpoint declared.
        assert!(sent_str.contains(&endpoint.token));
    }

    #[test]
    fn proxy_forwards_post_body_and_content_type() {
        let reply: &'static [u8] = b"HTTP/1.0 201 Created\r\nContent-Length: 0\r\n\r\n";
        let (port, captured) = spawn_one_shot_backend(reply);
        let endpoint = endpoint_for(port);
        let body = b"{\"title\":\"hello\"}";
        let request = make_post_request("alex://app/api/notes", body);
        let response = proxy_to_service(&endpoint, "com.example", "/api/notes", &request);
        assert_eq!(response.status().as_u16(), 201);
        let sent = captured.lock().unwrap().clone().expect("captured");
        let sent_str = std::str::from_utf8(&sent).expect("utf8 request");
        assert!(sent_str.starts_with("POST /api/notes HTTP/1.0\r\n"));
        assert!(sent_str.contains("content-type: application/json\r\n"));
        assert!(sent_str.contains(&format!("Content-Length: {}\r\n", body.len())));
        assert!(sent_str.ends_with(std::str::from_utf8(body).unwrap()));
    }

    #[test]
    fn proxy_drops_origin_and_cookie_headers() {
        let reply: &'static [u8] = b"HTTP/1.0 204 No Content\r\nContent-Length: 0\r\n\r\n";
        let (port, captured) = spawn_one_shot_backend(reply);
        let endpoint = endpoint_for(port);
        let request = Request::get("alex://app/api/notes")
            .header("origin", "https://evil.example")
            .header("cookie", "session=secret")
            .header("referer", "https://evil.example/page")
            .header("accept", "application/json")
            .body(Vec::new())
            .unwrap();
        let _ = proxy_to_service(&endpoint, "com.example", "/api/notes", &request);
        let sent = captured.lock().unwrap().clone().expect("captured");
        let sent_str = std::str::from_utf8(&sent).expect("utf8 request");
        assert!(sent_str.contains("accept: application/json\r\n"));
        assert!(!sent_str.contains("origin:"));
        assert!(!sent_str.contains("cookie:"));
        assert!(!sent_str.contains("referer:"));
    }

    #[test]
    fn proxy_preserves_4xx_status_code() {
        let reply: &'static [u8] =
            b"HTTP/1.0 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 9\r\n\r\nnot here";
        let (port, _captured) = spawn_one_shot_backend(reply);
        let endpoint = endpoint_for(port);
        let request = make_get_request("alex://app/api/missing");
        let response = proxy_to_service(&endpoint, "com.example", "/api/missing", &request);
        assert_eq!(response.status().as_u16(), 404);
        assert_eq!(response.body().as_ref(), b"not here");
    }

    #[test]
    fn parse_upstream_response_handles_malformed() {
        let raw = b"HTTP/1.0 200 OK\r\nbut no body separator";
        let response = parse_upstream_response(raw);
        assert_eq!(response.status().as_u16(), 502);
    }
}
