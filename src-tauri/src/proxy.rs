use base64::Engine as _;
use base64::engine::general_purpose;
use http::{HeaderName, HeaderValue};
use http_body::Frame;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Incoming as IncomingBody;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::ServerConfig as RustlsServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::net::SocketAddr;
// use std::collections::HashSet; // no longer needed
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::Arc;
use tauri::Emitter;
use tokio::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
// use rustls_native_certs as native_certs;
use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::http_shared::{Header, HttpRequestEvent, HttpResponseEvent, gen_id, now_rfc3339};
use crate::llm_rules::load_llm_rules;
use crate::process_lookup::try_lookup_process;

static PROXY_RUNNING: AtomicBool = AtomicBool::new(false);

// Debug logging switch for proxy module
use once_cell::sync::Lazy;
static PROXY_DEBUG: Lazy<bool> = Lazy::new(|| {
    match std::env::var("PROXY_DEBUG") {
        Ok(v) => v == "1" || v.eq_ignore_ascii_case("true"),
        Err(_) => true, // default on to aid troubleshooting; set PROXY_DEBUG=0 to mute
    }
});
macro_rules! proxy_log {
    ($($arg:tt)*) => {{
        if *PROXY_DEBUG { eprintln!($($arg)*); }
    }};
}

fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

async fn wait_idle(last_activity_ms: Arc<AtomicU64>, inflight: Arc<AtomicUsize>, idle: Duration) {
    let idle_ms = idle.as_millis() as u64;
    loop {
        let last = last_activity_ms.load(Ordering::Relaxed);
        let now = now_millis();
        if now.saturating_sub(last) >= idle_ms && inflight.load(Ordering::Relaxed) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// small helper so we can clone and call the inner service_fn
// removed: not needed after integrating activity update inside main service

#[derive(Debug, serde::Deserialize)]
pub struct StartProxyArgs {
    pub addr: String,
}

static UPSTREAM_PROXY: once_cell::sync::Lazy<std::sync::Mutex<Option<String>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(None));

static CONN_SEQ: once_cell::sync::Lazy<AtomicU64> =
    once_cell::sync::Lazy::new(|| AtomicU64::new(1));

// 注意：我们强制保持 MITM 模式用于抓包，不再做自动绕过

pub async fn start_proxy<R, E>(
    app: E,
    addr: String,
    upstream: Option<String>,
) -> Result<(), String>
where
    R: tauri::Runtime,
    E: tauri::Emitter<R> + Clone + Send + Sync + 'static,
{
    if PROXY_RUNNING.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    {
        let mut g = UPSTREAM_PROXY.lock().unwrap();
        *g = upstream;
    }
    proxy_log!("[proxy] start_proxy listening on {}", addr);
    {
        let up = UPSTREAM_PROXY.lock().unwrap().clone();
        match up {
            Some(ref u) => proxy_log!("[proxy] upstream proxy configured: {}", u),
            None => proxy_log!("[proxy] no upstream proxy configured"),
        }
    }
    let listener = TcpListener::bind(&addr).await.map_err(|e| e.to_string())?;
    let llm_rules = load_llm_rules();
    tokio::spawn(async move {
        loop {
            if !PROXY_RUNNING.load(Ordering::SeqCst) {
                break;
            }
            match listener.accept().await {
                Ok((mut inbound, peer)) => {
                    proxy_log!("[proxy] accepted connection from {}", peer);
                    let app_handle = app.clone();
                    let llm_rules_cloned = llm_rules.clone();
                    tokio::spawn(async move {
                        if let Err(_e) =
                            handle_client(&app_handle, &llm_rules_cloned, &mut inbound, peer).await
                        {
                            // swallow errors
                        }
                    });
                }
                Err(e) => {
                    proxy_log!("[proxy] accept error: {}", e);
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            }
        }
    });
    Ok(())
}

pub fn stop_proxy() {
    PROXY_RUNNING.store(false, Ordering::SeqCst);
}

/// Read from `reader` until a full HTTP response head (terminated by CRLFCRLF) is received.
/// Returns (status_code, version, reason_phrase, headers, initial_body_bytes).
async fn read_http_response_head<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<(u16, String, String, Vec<Header>, Bytes), String> {
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut tmp = vec![0u8; 8192];
    let max = 1024 * 256; // 256 KiB cap for headers
    let head_end;
    loop {
        if buf.len() > max {
            return Err("response header too large".into());
        }
        let n = reader.read(&mut tmp).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("upstream closed before sending headers".into());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = memchr::memmem::find(&buf, b"\r\n\r\n") {
            head_end = pos;
            break;
        }
    }
    let first_line_end = memchr::memchr(b'\n', &buf).unwrap_or(buf.len());
    let first = String::from_utf8_lossy(&buf[..first_line_end]).to_string();
    let mut headers_acc = Vec::<Header>::new();
    for line in String::from_utf8_lossy(&buf[..head_end])
        .split("\r\n")
        .skip(1)
    {
        if line.is_empty() {
            break;
        }
        if let Some((name, val)) = line.split_once(':') {
            headers_acc.push(Header {
                name: name.trim().to_string(),
                value: val.trim().to_string(),
            });
        }
    }
    let mut scode: u16 = 200;
    let mut version = "1.1".to_string();
    let mut reason = String::new();
    if first.starts_with("HTTP/") {
        // HTTP/<ver> <code> <reason...>
        let parts: Vec<&str> = first.trim().splitn(3, ' ').collect();
        if parts.len() >= 2 {
            version = parts[0].trim_start_matches("HTTP/").to_string();
            scode = parts[1].parse::<u16>().unwrap_or(200);
            if parts.len() == 3 {
                reason = parts[2].trim().to_string();
            }
        }
    }
    let body_slice = if head_end + 4 < buf.len() {
        Bytes::copy_from_slice(&buf[head_end + 4..])
    } else {
        Bytes::new()
    };
    Ok((scode, version, reason, headers_acc, body_slice))
}

async fn connect_via_upstream(
    proxy_url: &str,
    dst_host: &str,
    dst_port: u16,
) -> Result<TcpStream, String> {
    // Only support http://[user:pass@]host:port
    let url = proxy_url.trim();
    let without_scheme = url
        .strip_prefix("http://")
        .ok_or("only http upstream supported")?;
    let (creds_part, host_part) = if let Some(idx) = without_scheme.find('@') {
        (&without_scheme[..idx], &without_scheme[idx + 1..])
    } else {
        ("", without_scheme)
    };
    let (user, pass) = if !creds_part.is_empty() {
        let mut cp = creds_part.split(':');
        (cp.next().unwrap_or(""), cp.next().unwrap_or(""))
    } else {
        ("", "")
    };
    let mut hp = host_part.split(':');
    let phost = hp.next().unwrap_or("");
    let pport: u16 = hp.next().unwrap_or("8080").parse().unwrap_or(8080);

    let mut s = TcpStream::connect(format!("{}:{}", phost, pport))
        .await
        .map_err(|e| e.to_string())?;
    let auth_header = if !user.is_empty() {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", user, pass));
        format!("Proxy-Authorization: Basic {}\r\n", token)
    } else {
        String::new()
    };
    let connect_req = format!(
        "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\n{}Proxy-Connection: Keep-Alive\r\n\r\n",
        dst_host, dst_port, dst_host, dst_port, auth_header
    );
    s.write_all(connect_req.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 4096];
    let n = s.read(&mut buf).await.map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("upstream proxy closed".into());
    }
    let head = String::from_utf8_lossy(&buf[..n]);
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return Err(format!(
            "upstream proxy CONNECT failed: {}",
            head.lines().next().unwrap_or("")
        ));
    }
    Ok(s)
}

/// Relay bytes between `inbound` (client) and `upstream` (server).
/// If the client closes first, we eagerly shutdown the upstream side immediately
/// instead of waiting for the server to finish, to avoid dangling connections.
async fn tunnel_with_eager_close(
    inbound: &mut TcpStream,
    mut upstream: TcpStream,
) -> Result<(), std::io::Error> {

    let (mut in_r, mut in_w) = inbound.split();
    let (mut up_r, mut up_w) = upstream.split();

    let client_to_upstream = async {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = in_r.read(&mut buf).await?;
            if n == 0 {
                // Client closed: half-close upstream write and stop relaying
                let _ = AsyncWriteExt::shutdown(&mut up_w).await;
                break;
            }
            up_w.write_all(&buf[..n]).await?;
        }
        Ok::<(), std::io::Error>(())
    };

    let upstream_to_client = async {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = up_r.read(&mut buf).await?;
            if n == 0 {
                // Upstream closed: half-close client write and stop relaying
                let _ = AsyncWriteExt::shutdown(&mut in_w).await;
                break;
            }
            in_w.write_all(&buf[..n]).await?;
        }
        Ok::<(), std::io::Error>(())
    };

    tokio::select! {
        _ = client_to_upstream => {},
        _ = upstream_to_client => {},
    }

    Ok(())
}
async fn handle_client<R: tauri::Runtime, E: tauri::Emitter<R> + Clone + Send + Sync + 'static>(
    app: &E,
    llm_rules: &crate::llm_rules::LlmRules,
    inbound: &mut TcpStream,
    peer: SocketAddr,
) -> Result<(), String> {
    // Minimal HTTP/1 CONNECT parser + plain HTTP forwarder placeholder (MITM to be filled)
    proxy_log!("[proxy] handle_client begin, peer={}", peer);
    let mut buf = vec![0u8; 65536];
    let n = inbound.read(&mut buf).await.map_err(|e| e.to_string())?;
    proxy_log!("[proxy] handle_client read {} bytes from client {}", n, peer);
    if n == 0 {
        proxy_log!("[proxy] client {} closed before sending data", peer);
        return Ok(());
    }
    let data = &buf[..n];
    let head_end = memchr::memmem::find(data, b"\r\n\r\n").unwrap_or(n);
    let first_line_end = memchr::memchr(b'\n', data).unwrap_or(n);
    let first = String::from_utf8_lossy(&data[..first_line_end]).to_string();
    proxy_log!("[proxy] request first line: {}", first.trim());

    // 如果第一个包不像 HTTP（既不是 CONNECT 也不是 形如 GET/POST/... 开头），直接断开，避免非 HTTP 噪声长时间占用连接
    let looks_http = first.starts_with("CONNECT ")
        || first.starts_with("GET ")
        || first.starts_with("POST ")
        || first.starts_with("HEAD ")
        || first.starts_with("PUT ")
        || first.starts_with("DELETE ")
        || first.starts_with("OPTIONS ")
        || first.starts_with("TRACE ")
        || first.starts_with("PATCH ");
    if !looks_http {
        proxy_log!("[proxy] non-http initial packet from {} -> close", peer);
        return Ok(());
    }
    if first.starts_with("CONNECT ") {
        let host_port = first.split_whitespace().nth(1).unwrap_or("");
        let mut parts = host_port.split(':');
        let host = parts.next().unwrap_or("");
        let port = parts.next().unwrap_or("443").parse::<u16>().unwrap_or(443);
        let conn_id = CONN_SEQ.fetch_add(1, Ordering::SeqCst);
        eprintln!(
            "[proxy][conn={}] CONNECT from {} => {}:{}",
            conn_id,
            peer,
            host,
            port
        );
        // Respond OK to switch to TLS
        inbound
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .map_err(|e| e.to_string())?;
        proxy_log!("[proxy][conn={}] CONNECT 200 sent to {}, waiting for TLS/next step", conn_id, peer);

        // If系统未安装我们的根证书，则不要做 MITM，直接按标准 CONNECT 建立隧道，避免客户端因证书校验/钉扎而主动断开
        // 测试场景可通过环境变量 FORCE_MITM=1 强制开启，以便在 CI 中覆盖 MITM 逻辑
        let force_mitm = std::env::var("FORCE_MITM").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false);
        let sys_mitm = match crate::ca::is_ca_installed_in_system_trust() {
            Ok(v) => v,
            Err(_) => false,
        };
        let can_mitm = force_mitm || sys_mitm;
        if !can_mitm {
            eprintln!(
                "[proxy][conn={}] CA not installed or check failed, fallback to pure tunnel for {}:{}",
                conn_id, host, port
            );
            // Upstream may be another HTTP proxy; otherwise direct connect
            let use_upstream = { UPSTREAM_PROXY.lock().unwrap().clone() };
            let upstream = if let Some(proxy_url) = use_upstream {
                eprintln!("[proxy][conn={}] tunneling via upstream proxy {}", conn_id, proxy_url);
                connect_via_upstream(&proxy_url, host, port).await.map_err(|e| e.to_string())?
            } else {
                eprintln!("[proxy][conn={}] tunneling direct to {}:{}", conn_id, host, port);
                TcpStream::connect(format!("{}:{}", host, port)).await.map_err(|e| e.to_string())?
            };
            // Pump bytes both ways; if client closes, eagerly close upstream too
            if let Err(e) = tunnel_with_eager_close(inbound, upstream).await {
                eprintln!("[proxy] tunnel error: {}", e);
            }
            return Ok(());
        }

        // Build rustls server config with leaf cert signed by our CA
        let (ca_pem, ca_key_pem) = match crate::ca::ensure_ca_exists() {
            Ok(v) => v,
            Err(e) => return Err(e),
        };
        proxy_log!("[proxy][conn={}] MITM enabled; generating leaf cert for {}", conn_id, host);
        let (leaf_der, key_der, ca_der) =
            crate::ca::generate_leaf_cert_for_host(host, &ca_pem, &ca_key_pem)?;
        // 提供完整链（叶子 + CA），兼容部分 Electron/Node 客户端的验证逻辑
        let certs = vec![CertificateDer::from(leaf_der), CertificateDer::from(ca_der)];
        let pkcs8_owned: PrivatePkcs8KeyDer<'static> = PrivatePkcs8KeyDer::from(key_der.clone());
        let priv_key = PrivateKeyDer::Pkcs8(pkcs8_owned);
        let mut server_cfg = RustlsServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, priv_key)
            .map_err(|e| e.to_string())?;
        // Offer ALPN; allow disabling h2 via env to avoid client-side reuse stalls
        let disable_h2 = std::env::var("DISABLE_H2")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        server_cfg.alpn_protocols = if disable_h2 {
            proxy_log!("[proxy] DISABLE_H2=1 -> ALPN http/1.1 only for {host}");
            vec![b"http/1.1".to_vec()]
        } else {
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        };
        // 允许 TLS1.2/1.3，移除不常用椭圆曲线以更贴近常见客户端期望
        server_cfg.max_fragment_size = None;
        let acceptor = TlsAcceptor::from(std::sync::Arc::new(server_cfg));

        // Hyper client for upstream direct (supports http/1.1 and http/2 over TLS)
        let https = HttpsConnectorBuilder::new()
            .with_native_roots()
            .expect("native roots")
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();
        type ReqBody = Full<Bytes>;
        let client_base: Client<_, ReqBody> = Client::builder(TokioExecutor::new()).build(https);

        // 单次 TLS 会话，Hyper 会在该会话内处理 keep-alive 多请求
        proxy_log!("[proxy][conn={}] accepting TLS from client for {}:{}", conn_id, host, port);
        let tls_stream = match acceptor.accept(inbound).await {
            Ok(s) => s,
            Err(e) => {
                proxy_log!("[proxy][conn={}] client TLS accept failed: {}", conn_id, e);
                return Err(e.to_string());
            }
        };
        proxy_log!("[proxy][conn={}] client TLS established for {}:{}", conn_id, host, port);

        let app_handle2 = app.clone();
        let llm_rules2 = llm_rules.clone();
        let host_owned = host.to_string();
        let client = client_base.clone();
        // activity tracker + inflight counter for h2 idle watchdog; harmless for h1
        let last_activity = Arc::new(AtomicU64::new(now_millis()));
        let last_activity_svc = last_activity.clone();
        let inflight = Arc::new(AtomicUsize::new(0));
        let inflight_svc_outer = inflight.clone();

        let last_activity_update = last_activity_svc.clone();
        let service = service_fn(move |req: Request<IncomingBody>| {
            let app3 = app_handle2.clone();
            let client3 = client.clone();
            let llm3 = llm_rules2.clone();
            let peer_ip = peer.ip().to_string();
            let peer_port = peer.port();
            let host3 = host_owned.clone();
            let last_update = last_activity_update.clone();
            let inflight_svc = inflight_svc_outer.clone();
            async move {
                last_update.store(now_millis(), Ordering::Relaxed);
                inflight_svc.fetch_add(1, Ordering::Relaxed);
                let (parts, body_in) = req.into_parts();
                let mut headers_vec = Vec::<Header>::new();
                for (name, value) in parts.headers.iter() {
                    headers_vec.push(Header {
                        name: name.as_str().to_string(),
                        value: value.to_str().unwrap_or("").to_string(),
                    });
                }
                // Ensure Host header exists for rule matching (HTTP/2 usually uses :authority)
                if !headers_vec
                    .iter()
                    .any(|h| h.name.eq_ignore_ascii_case("host"))
                {
                    headers_vec.push(Header {
                        name: "host".into(),
                        value: host3.clone(),
                    });
                }
                let body_bytes = body_in.collect().await?.to_bytes();
                let method_str = parts.method.as_str().to_string();
                let path_q = parts
                    .uri
                    .path_and_query()
                    .map(|x| x.as_str().to_string())
                    .unwrap_or("/".to_string());
                let id = gen_id();
                let mut req_evt = HttpRequestEvent {
                    id: id.clone(),
                    timestamp: now_rfc3339(),
                    src_ip: peer_ip.clone(),
                    src_port: peer_port,
                    dst_ip: host3.clone(),
                    dst_port: port,
                    method: method_str.clone(),
                    path: path_q.clone(),
                    version: "1.1".into(),
                    headers: headers_vec.clone(),
                    body_base64: if body_bytes.is_empty() {
                        None
                    } else {
                        Some(general_purpose::STANDARD.encode(&body_bytes))
                    },
                    body_len: body_bytes.len(),
                    process_name: None,
                    pid: None,
                    is_llm: false,
                    llm_provider: None,
                };
                // Method & path are not accessible after consuming body; reconstruct using request parts first
                // Workaround: parse from pseudo header list we copied not ideal; ignore for now.
                if let Some(provider) = llm3.match_request(&req_evt) {
                    req_evt.is_llm = true;
                    req_evt.llm_provider = Some(provider);
                }
                // 尝试按客户端源端口查询进程
                let (pname, pid) = try_lookup_process(peer_port, false);
                if pname.is_some() || pid.is_some() {
                    req_evt.process_name = pname;
                    req_evt.pid = pid;
                }
                let _ = app3.emit("onHttpRequest", req_evt.clone());
                last_update.store(now_millis(), Ordering::Relaxed);

                // Build upstream absolute URI
                let host_header = headers_vec
                    .iter()
                    .find(|h| h.name.eq_ignore_ascii_case("host"))
                    .map(|h| h.value.clone())
                    .unwrap_or(host3.clone());
                let uri = format!("https://{}{}", host_header, path_q);
                proxy_log!("[proxy] outbound (direct) {} {}", method_str, uri);

                let mut out_builder = Request::builder();
                out_builder = out_builder.method(method_str.as_str()).uri(uri);
                let mut out_req: Request<ReqBody> =
                    match out_builder.body(Full::new(Bytes::from(body_bytes.clone()))) {
                        Ok(r) => r,
                        Err(_) => {
                            let (etx, erx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(1);
                            let _ = etx.send(Ok(Frame::data(Bytes::new()))).await;
                            let ebody = StreamBody::new(ReceiverStream::new(erx));
                            let resp = Response::builder().status(400).body(ebody).unwrap();
                            return Ok(resp);
                        }
                    };
                // copy headers with filtering to avoid invalid/duplicated hop-by-hop headers
                // Hyper will set Host and Content-Length/Transfer-Encoding as needed.
                for h in headers_vec.iter() {
                    let lname = h.name.to_ascii_lowercase();
                    if matches!(
                        lname.as_str(),
                        // hop-by-hop and proxy-specific headers should not be forwarded
                        "connection"
                            | "proxy-connection"
                            | "proxy-authorization"
                            | "keep-alive"
                            | "upgrade"
                            | "te"
                            | "trailers"
                            // avoid duplicating Host and body length semantics
                            | "host"
                            | "content-length"
                            | "transfer-encoding"
                    ) {
                        continue;
                    }
                    if let (Ok(name), Ok(val)) =
                        (h.name.parse::<HeaderName>(), h.value.parse::<HeaderValue>())
                    {
                        out_req.headers_mut().append(name, val);
                    }
                }
                // 保持与上游的 keep-alive（由 Hyper/HTTP2 连接池管理），兼容 Cherry 客户端的长连接
                // 若目标端要求关闭会通过响应头告知，Hyper 会正确处理

                // 上游代理生效：若配置了上游，则通过上游 CONNECT + TLS，手工写入请求并流式回传
                let use_upstream = { UPSTREAM_PROXY.lock().unwrap().clone() };
                if let Some(proxy_url) = use_upstream {
                    proxy_log!("[proxy] using upstream {} for {}:{}", proxy_url, host3, port);
                    // 1) 建立到上游的 CONNECT 隧道
                    let upstream_tcp = match connect_via_upstream(&proxy_url, &host3, port).await {
                        Ok(s) => s,
                        Err(_) => {
                            proxy_log!("[proxy] upstream CONNECT failed");
                            let (etx, erx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(1);
                            let _ = etx.send(Ok(Frame::data(Bytes::new()))).await;
                            let ebody = StreamBody::new(ReceiverStream::new(erx));
                            let resp = Response::builder().status(502).body(ebody).unwrap();
                            return Ok(resp);
                        }
                    };
                    // 2) 在隧道上做 TLS 到目标
                    let mut roots = rustls::RootCertStore::empty();
                    if let Ok(certs) = rustls_native_certs::load_native_certs() {
                        for c in certs {
                            let _ = roots.add(c);
                        }
                    }
                    let client_cfg = std::sync::Arc::new(
                        rustls::ClientConfig::builder()
                            .with_root_certificates(roots)
                            .with_no_client_auth(),
                    );
                    let tls_conn = tokio_rustls::TlsConnector::from(client_cfg);
                    // Avoid borrowing host3; create a 'static str for SNI
                    let sni_leaked: &'static str = Box::leak(host3.clone().into_boxed_str());
                    let server_name = rustls::pki_types::ServerName::try_from(sni_leaked)
                        .unwrap_or_else(|_| {
                            rustls::pki_types::ServerName::try_from("localhost").unwrap()
                        });
                    let mut upstream_tls = match tls_conn.connect(server_name, upstream_tcp).await {
                        Ok(v) => v,
                        Err(_) => {
                            proxy_log!("[proxy] upstream TLS connect failed");
                            let (etx, erx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(1);
                            let _ = etx.send(Ok(Frame::data(Bytes::new()))).await;
                            let ebody = StreamBody::new(ReceiverStream::new(erx));
                            let resp = Response::builder().status(502).body(ebody).unwrap();
                            return Ok(resp);
                        }
                    };

                    // 3) 组装 origin-form 请求并写入上游
                    let mut forward = Vec::<u8>::new();
                    forward.extend_from_slice(
                        format!("{} {} HTTP/1.1\r\n", method_str, path_q).as_bytes(),
                    );
                    let mut has_host = false;
                    for h in headers_vec.iter() {
                        let lname = h.name.to_ascii_lowercase();
                        if lname == "host" {
                            has_host = true;
                        }
                        if matches!(
                            lname.as_str(),
                            "proxy-connection" | "proxy-authorization" | "connection" | "te"
                        ) {
                            continue;
                        }
                        forward
                            .extend_from_slice(format!("{}: {}\r\n", h.name, h.value).as_bytes());
                    }
                    if !has_host {
                        forward.extend_from_slice(format!("Host: {}\r\n", host_header).as_bytes());
                    }
                    forward.extend_from_slice(b"\r\n");
                    if !body_bytes.is_empty() {
                        forward.extend_from_slice(&body_bytes);
                    }
                    if let Err(_) = upstream_tls.write_all(&forward).await {
                        let (etx, erx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(1);
                        let _ = etx.send(Ok(Frame::data(Bytes::new()))).await;
                        let ebody = StreamBody::new(ReceiverStream::new(erx));
                        let resp = Response::builder().status(502).body(ebody).unwrap();
                        return Ok(resp);
                    }

                    // 4) 读上游首包，解析头，emit 首包事件，并把余下 body 通过 StreamBody 回给客户端
                    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(16);
                    let app4 = app3.clone();
                    let id4 = id.clone();
                    let peer_ip4 = peer_ip.clone();
                    let host4 = host3.clone();
                    // 先读取并解析上游响应首包，以便把真实状态码与响应头转发给客户端
                    let (scode, version_str, reason_phrase, resp_headers, first_body_slice) =
                        match read_http_response_head(&mut upstream_tls).await {
                            Ok(v) => v,
                            Err(_) => (
                                200u16,
                                "1.1".to_string(),
                                String::new(),
                                Vec::<Header>::new(),
                                Bytes::new(),
                            ),
                        };
                    proxy_log!("[proxy] upstream head parsed: {} {} {} bytes-first", scode, version_str, first_body_slice.len());

                    // emit 首包事件（包含头和首段 body）
                    let mut head_evt = HttpResponseEvent {
                        id: id4.clone(),
                        timestamp: now_rfc3339(),
                        src_ip: host4.clone(),
                        src_port: port,
                        dst_ip: peer_ip4.clone(),
                        dst_port: peer_port,
                        status_code: scode,
                        reason: if reason_phrase.is_empty() {
                            None
                        } else {
                            Some(reason_phrase.clone())
                        },
                        version: version_str.clone(),
                        headers: resp_headers.clone(),
                        body_base64: if first_body_slice.is_empty() {
                            None
                        } else {
                            Some(general_purpose::STANDARD.encode(&first_body_slice))
                        },
                        body_len: first_body_slice.len(),
                        process_name: None,
                        pid: None,
                        is_llm: false,
                        llm_provider: None,
                    };
                    // 回填进程名（服务端口在响应方向上更可靠）
                    let (pname2, pid2) = try_lookup_process(peer_port, true);
                    if pname2.is_some() || pid2.is_some() {
                        head_evt.process_name = pname2;
                        head_evt.pid = pid2;
                    }
                    if req_evt.is_llm {
                        head_evt.is_llm = true;
                        head_evt.llm_provider = req_evt.llm_provider.clone();
                    }
                    let _ = app4.emit("onHttpResponse", head_evt);
                    last_update.store(now_millis(), Ordering::Relaxed);

                    // 把首段 body 送入下游
                    if !first_body_slice.is_empty() {
                        let _ = tx.send(Ok(Frame::data(first_body_slice.clone()))).await;
                    }

                    // 后续 body 流式转发
                    let resp_headers_spawn = resp_headers.clone();
                    let req_is_llm_spawn = req_evt.is_llm;
                    let req_provider_spawn = req_evt.llm_provider.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 65536];
                        loop {
                            match tokio::time::timeout(Duration::from_secs(30), upstream_tls.read(&mut buf)).await {
                                Ok(Ok(n)) if n > 0 => {
                                    let chunk = Bytes::copy_from_slice(&buf[..n]);
                                    if tx.send(Ok(Frame::data(chunk.clone()))).await.is_err() {
                                        // downstream gone; stop reading and close upstream
                                        break;
                                    }
                                    let mut chunk_evt = HttpResponseEvent {
                                        id: id4.clone(),
                                        timestamp: now_rfc3339(),
                                        src_ip: host4.clone(),
                                        src_port: port,
                                        dst_ip: peer_ip4.clone(),
                                        dst_port: peer_port,
                                        status_code: scode,
                                        reason: None,
                                        version: version_str.clone(),
                                        headers: resp_headers_spawn.clone(),
                                        body_base64: Some(general_purpose::STANDARD.encode(&chunk)),
                                        body_len: chunk.len(),
                                        process_name: None,
                                        pid: None,
                                        is_llm: false,
                                        llm_provider: None,
                                    };
                                    let (pname3, pid3) = try_lookup_process(peer_port, true);
                                    if pname3.is_some() || pid3.is_some() {
                                        chunk_evt.process_name = pname3;
                                        chunk_evt.pid = pid3;
                                    }
                                    if req_is_llm_spawn {
                                        chunk_evt.is_llm = true;
                                        chunk_evt.llm_provider = req_provider_spawn.clone();
                                    }
                                    let _ = app4.emit("onHttpResponse", chunk_evt);
                                    last_update.store(now_millis(), Ordering::Relaxed);
                                }
                                Ok(Ok(_)) => { // n == 0
                                    break;
                                }
                                _ => {
                                    // timeout or error: 结束读取，避免卡住
                                    break;
                                }
                            }
                        }
                    });

                    // 构造返回并附带上游响应头（过滤 hop-by-hop 和长度编码类）
                    let mut rb = Response::builder().status(scode);
                    for h in resp_headers.iter() {
                        let lname = h.name.to_ascii_lowercase();
                        if matches!(
                            lname.as_str(),
                            "connection"
                                | "proxy-connection"
                                | "keep-alive"
                                | "transfer-encoding"
                                | "content-length"
                                | "upgrade"
                                | "proxy-authenticate"
                                | "proxy-authorization"
                                | "te"
                                | "trailers"
                        ) {
                            continue;
                        }
                        if let (Ok(name), Ok(val)) =
                            (h.name.parse::<HeaderName>(), h.value.parse::<HeaderValue>())
                        {
                            rb = rb.header(name, val);
                        }
                    }
                    let body = StreamBody::new(ReceiverStream::new(rx));
                        let resp_built = rb.body(body).unwrap();
                        inflight_svc.fetch_sub(1, Ordering::Relaxed);
                        Ok::<_, hyper::Error>(resp_built)
                } else {
                    // 仍然直连（hyper client）
                    let resp = match client3.request(out_req).await {
                        Ok(r) => r,
                        Err(_) => {
                            let (etx, erx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(1);
                            let _ = etx.send(Ok(Frame::data(Bytes::new()))).await;
                            let ebody = StreamBody::new(ReceiverStream::new(erx));
                            let resp = Response::builder().status(502).body(ebody).unwrap();
                            return Ok(resp);
                        }
                    };
                    let status = resp.status();
                    let mut resp_headers = Vec::<Header>::new();
                    for (name, value) in resp.headers().iter() {
                        resp_headers.push(Header {
                            name: name.as_str().to_string(),
                            value: value.to_str().unwrap_or("").to_string(),
                        });
                    }
                    let mut head_evt = HttpResponseEvent {
                        id: id.clone(),
                        timestamp: now_rfc3339(),
                        src_ip: host3.clone(),
                        src_port: port,
                        dst_ip: peer_ip.clone(),
                        dst_port: peer_port,
                        status_code: status.as_u16(),
                        reason: None,
                        version: "1.1".into(),
                        headers: resp_headers.clone(),
                        body_base64: None,
                        body_len: 0,
                        process_name: None,
                        pid: None,
                        is_llm: false,
                        llm_provider: None,
                    };
                    let (pname2, pid2) = try_lookup_process(peer_port, true);
                    if pname2.is_some() || pid2.is_some() {
                        head_evt.process_name = pname2;
                        head_evt.pid = pid2;
                    }
                    if let Some(provider) = llm3.match_response(&head_evt) {
                        head_evt.is_llm = true;
                        head_evt.llm_provider = Some(provider);
                    }
                    if req_evt.is_llm {
                        head_evt.is_llm = true;
                        head_evt.llm_provider = req_evt.llm_provider.clone();
                    }
                    let _ = app3.emit("onHttpResponse", head_evt);
                    last_update.store(now_millis(), Ordering::Relaxed);

                    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(16);
                    let mut upstream_body = resp.into_body();
                    let app4 = app3.clone();
                    let resp_headers4 = resp_headers.clone();
                    let id4 = id.clone();
                    let peer_ip4 = peer_ip.clone();
                    let status_code_value = status.as_u16();
                    let req_is_llm_spawn = req_evt.is_llm;
                    let req_provider_spawn = req_evt.llm_provider.clone();
                    tokio::spawn(async move {
                        while let Some(frame_res) = upstream_body.frame().await {
                            match frame_res {
                                Ok(frame) => {
                                    if let Some(data) = frame.data_ref() {
                                        let bytes = data.clone();
                                        if tx.send(Ok(Frame::data(bytes.clone()))).await.is_err() {
                                            break;
                                        }
                                        let mut chunk_evt = HttpResponseEvent {
                                            id: id4.clone(),
                                            timestamp: now_rfc3339(),
                                            src_ip: host3.clone(),
                                            src_port: port,
                                            dst_ip: peer_ip4.clone(),
                                            dst_port: peer_port,
                                            status_code: status_code_value,
                                            reason: None,
                                            version: "1.1".into(),
                                            headers: resp_headers4.clone(),
                                            body_base64: Some(
                                                general_purpose::STANDARD.encode(&bytes),
                                            ),
                                            body_len: bytes.len(),
                                            process_name: None,
                                            pid: None,
                                            is_llm: false,
                                            llm_provider: None,
                                        };
                                        let (pname3, pid3) = try_lookup_process(peer_port, true);
                                        if pname3.is_some() || pid3.is_some() {
                                            chunk_evt.process_name = pname3;
                                            chunk_evt.pid = pid3;
                                        }
                                        if req_is_llm_spawn {
                                            chunk_evt.is_llm = true;
                                            chunk_evt.llm_provider = req_provider_spawn.clone();
                                        }
                                        let _ = app4.emit("onHttpResponse", chunk_evt);
                                        last_update.store(now_millis(), Ordering::Relaxed);
                                    } else if frame.is_trailers() {
                                        if tx.send(Ok(frame)).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(Err(e)).await;
                                    break;
                                }
                            }
                        }
                    });
                    let mut rb = Response::builder().status(status);
                    for h in resp_headers.iter() {
                        if let (Ok(name), Ok(val)) =
                            (h.name.parse::<HeaderName>(), h.value.parse::<HeaderValue>())
                        {
                            rb = rb.header(name, val);
                        }
                    }
                    let body = StreamBody::new(ReceiverStream::new(rx));
                        let resp_built = rb.body(body).unwrap();
                        inflight_svc.fetch_sub(1, Ordering::Relaxed);
                        Ok::<_, hyper::Error>(resp_built)
                }
            }
        });

        let negotiated_h2 = {
            let (_s, conn) = tls_stream.get_ref();
            match conn.alpn_protocol() {
                Some(proto) => {
                    let bytes: &[u8] = proto.as_ref();
                    bytes == b"h2"
                }
                None => false,
            }
        };
        proxy_log!(
            "[proxy][conn={}] ALPN negotiated: {}",
            conn_id,
            if negotiated_h2 { "h2" } else { "http/1.1" }
        );
        if negotiated_h2 {
            let io = TokioIo::new(tls_stream);
            use hyper::server::conn::http2;
            proxy_log!("[proxy][conn={}] serving HTTP/2 for {}:{}", conn_id, host, port);
            // h2 会话 idle watchdog：若一段时间无活动则优雅关闭，触发客户端重连
            let started = Instant::now();
            let idle_task = {
                let last = last_activity.clone();
                let inflight = inflight.clone();
                tokio::spawn(async move {
                    // 收紧空闲判定：8s 无事件且无在途请求 -> 重置
                    wait_idle(last, inflight, Duration::from_secs(8)).await;
                })
            };
            let serve_fut = http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service);
            tokio::select! {
                res = serve_fut => {
                    if let Err(e) = res {
                        proxy_log!("[proxy][conn={}] http2 serve_connection error after {:?}: {}", conn_id, started.elapsed(), e);
                    }
                }
                _ = idle_task => {
                    proxy_log!("[proxy][conn={}] h2 idle 15s; closing session", conn_id);
                    // 退出 select 后，io 会被 drop，连接关闭
                }
            }
        } else {
            let io = TokioIo::new(tls_stream);
            proxy_log!("[proxy][conn={}] serving HTTP/1.1 for {}:{} (keep_alive=false)", conn_id, host, port);
            let mut builder = http1::Builder::new();
            builder.keep_alive(false);
            if let Err(e) = builder
                .serve_connection(io, service)
                .await
            {
                proxy_log!("[proxy][conn={}] http1 serve_connection error: {}", conn_id, e);
            }
        }
        proxy_log!("[proxy][conn={}] CONNECT session ended for {}:{}", conn_id, host, port);
        return Ok(());
    } else {
        // Plain HTTP: parse request line minimally, forward to upstream, emit events.
        let mut req_headers = Vec::<Header>::new();
        let head = &data[..head_end];
        for line in String::from_utf8_lossy(head).split("\r\n").skip(1) {
            if line.is_empty() {
                break;
            }
            if let Some((name, val)) = line.split_once(':') {
                req_headers.push(Header {
                    name: name.trim().to_string(),
                    value: val.trim().to_string(),
                });
            }
        }
        let req_line = first;
        let mut rl = req_line.split_whitespace();
        let method = rl.next().unwrap_or("").to_string();
        let full_path = rl.next().unwrap_or("").to_string();
        let version = rl
            .next()
            .unwrap_or("HTTP/1.1")
            .trim_start_matches("HTTP/")
            .to_string();
        // Extract host header
        let host_header = req_headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("host"))
            .map(|h| h.value.clone())
            .unwrap_or_default();
        let (host, port) = if let Some((h, p)) = host_header.split_once(':') {
            (h.to_string(), p.parse::<u16>().unwrap_or(80))
        } else {
            (host_header, 80)
        };
        // Build request event
        let body_slice = if head_end < data.len() {
            &data[head_end + 4..]
        } else {
            &[]
        };
        let mut req_evt = HttpRequestEvent {
            id: gen_id(),
            timestamp: now_rfc3339(),
            src_ip: peer.ip().to_string(),
            src_port: peer.port(),
            dst_ip: host.clone(),
            dst_port: port,
            method: method.clone(),
            path: full_path.clone(),
            version: version.clone(),
            headers: req_headers.clone(),
            body_base64: if body_slice.is_empty() {
                None
            } else {
                Some(general_purpose::STANDARD.encode(body_slice))
            },
            body_len: body_slice.len(),
            process_name: None,
            pid: None,
            is_llm: llm_rules
                .match_request(&HttpRequestEvent {
                    path: full_path.clone(),
                    method: method.clone(),
                    headers: req_headers.clone(),
                    body_base64: if body_slice.is_empty() {
                        None
                    } else {
                        Some(general_purpose::STANDARD.encode(body_slice))
                    },
                    ..req_evt_template(peer.clone(), host.clone(), port)
                })
                .is_some(),
            llm_provider: None,
        };
        // 为纯 HTTP 同样尝试回填源端口对应的进程
        let (pname_http, pid_http) = try_lookup_process(peer.port(), false);
        if pname_http.is_some() || pid_http.is_some() {
            req_evt.process_name = pname_http;
            req_evt.pid = pid_http;
        }
        let _ = app.emit("onHttpRequest", req_evt.clone());

        // Build origin-form request for upstream (convert from absolute-form)
        let path_only = if full_path.starts_with("http://") {
            let s = &full_path["http://".len()..];
            match s.find('/') {
                Some(i) => &s[i..],
                None => "/",
            }
        } else if full_path.starts_with("https://") {
            let s = &full_path["https://".len()..];
            match s.find('/') {
                Some(i) => &s[i..],
                None => "/",
            }
        } else {
            &full_path
        };
        let mut forward = Vec::<u8>::new();
        forward.extend_from_slice(format!("{} {} HTTP/1.1\r\n", method, path_only).as_bytes());
        for h in req_headers.iter() {
            let lname = h.name.to_ascii_lowercase();
            if lname == "proxy-connection" || lname == "proxy-authorization" {
                continue;
            }
            forward.extend_from_slice(format!("{}: {}\r\n", h.name, h.value).as_bytes());
        }
        forward.extend_from_slice(b"\r\n");
        if !body_slice.is_empty() {
            forward.extend_from_slice(body_slice);
        }

        // Connect upstream and stream response back to client (emit events)
        let upstream_addr = format!("{}:{}", host, port);
        eprintln!("[proxy] HTTP direct connect upstream {}", upstream_addr);
        let mut upstream = TcpStream::connect(&upstream_addr)
            .await
            .map_err(|e| e.to_string())?;
        upstream
            .write_all(&forward)
            .await
            .map_err(|e| e.to_string())?;
        // half-close 写方向，提示上游尽快返回，避免长时间等待
        let _ = upstream.shutdown().await;
        eprintln!("[proxy] HTTP forwarded {} bytes", forward.len());

        let mut first_chunk = true;
        let mut total = 0usize;
        let mut resp_headers = Vec::<Header>::new();
        let mut scode: u16 = 200;
        let mut version_str = "1.1".to_string();
        let mut resp_buf = vec![0u8; 65536];
        loop {
            let m = match tokio::time::timeout(Duration::from_secs(30), upstream.read(&mut resp_buf)).await {
                Ok(Ok(v)) => v,
                _ => {
                    eprintln!("[proxy] upstream read timeout/error");
                    break;
                }
            };
            if m == 0 {
                eprintln!("[proxy] upstream closed, total={} bytes", total);
                break;
            }
            total += m;
            inbound
                .write_all(&resp_buf[..m])
                .await
                .map_err(|e| e.to_string())?;

            let data = &resp_buf[..m];
            if first_chunk {
                let head_end = memchr::memmem::find(data, b"\r\n\r\n").unwrap_or(m);
                let first_line_end = memchr::memchr(b'\n', data).unwrap_or(m);
                let first = String::from_utf8_lossy(&data[..first_line_end]).to_string();
                resp_headers.clear();
                for line in String::from_utf8_lossy(&data[..head_end])
                    .split("\r\n")
                    .skip(1)
                {
                    if line.is_empty() {
                        break;
                    }
                    if let Some((name, val)) = line.split_once(':') {
                        resp_headers.push(Header {
                            name: name.trim().to_string(),
                            value: val.trim().to_string(),
                        });
                    }
                }
                if first.starts_with("HTTP/") {
                    let mut it = first.split_whitespace();
                    if let Some(v) = it.next() {
                        version_str = v.trim_start_matches("HTTP/").to_string();
                    }
                    if let Some(c) = it.nth(0) {
                        scode = c.parse::<u16>().unwrap_or(200);
                    }
                }
                let body_slice = if head_end < data.len() {
                    &data[head_end + 4..]
                } else {
                    &[]
                };
                let first_evt = HttpResponseEvent {
                    id: req_evt.id.clone(),
                    timestamp: now_rfc3339(),
                    src_ip: host.clone(),
                    src_port: port,
                    dst_ip: peer.ip().to_string(),
                    dst_port: peer.port(),
                    status_code: scode,
                    reason: None,
                    version: version_str.clone(),
                    headers: resp_headers.clone(),
                    body_base64: if body_slice.is_empty() {
                        None
                    } else {
                        Some(general_purpose::STANDARD.encode(body_slice))
                    },
                    body_len: body_slice.len(),
                    process_name: None,
                    pid: None,
                    is_llm: false,
                    llm_provider: None,
                };
                let _ = app.emit("onHttpResponse", first_evt);
                first_chunk = false;
            } else {
                // stream chunks as subsequent events
                let mut chunk_evt = HttpResponseEvent {
                    id: req_evt.id.clone(),
                    timestamp: now_rfc3339(),
                    src_ip: host.clone(),
                    src_port: port,
                    dst_ip: peer.ip().to_string(),
                    dst_port: peer.port(),
                    status_code: scode,
                    reason: None,
                    version: version_str.clone(),
                    headers: resp_headers.clone(),
                    body_base64: Some(general_purpose::STANDARD.encode(&resp_buf[..m])),
                    body_len: m,
                    process_name: None,
                    pid: None,
                    is_llm: false,
                    llm_provider: None,
                };
                let (pname3, pid3) = try_lookup_process(peer.port(), true);
                if pname3.is_some() || pid3.is_some() {
                    chunk_evt.process_name = pname3;
                    chunk_evt.pid = pid3;
                }
                let _ = app.emit("onHttpResponse", chunk_evt);
            }
        }
        Ok(())
    }
}

fn req_evt_template(peer: SocketAddr, host: String, port: u16) -> HttpRequestEvent {
    HttpRequestEvent {
        id: gen_id(),
        timestamp: now_rfc3339(),
        src_ip: peer.ip().to_string(),
        src_port: peer.port(),
        dst_ip: host,
        dst_port: port,
        method: String::new(),
        path: String::new(),
        version: "1.1".into(),
        headers: Vec::new(),
        body_base64: None,
        body_len: 0,
        process_name: None,
        pid: None,
        is_llm: false,
        llm_provider: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener as TokioTcpListener;
    use rustls::{ClientConfig, RootCertStore};
    use rustls::pki_types::ServerName;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::{Duration, timeout};
    use tokio_rustls::TlsConnector;
    use std::process::Command;
    use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime};

    fn proxy_tests_enabled() -> bool {
        std::env::var("RUN_PROXY_TESTS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }

    async fn tls_over(
        stream: TcpStream,
        host: &str,
    ) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
        let mut roots = RootCertStore::empty();
        if let Ok(certs) = rustls_native_certs::load_native_certs() {
            for c in certs {
                let _ = roots.add(c);
            }
        }
        let cfg = std::sync::Arc::new(
            ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let connector = TlsConnector::from(cfg);
        let server_name =
            ServerName::try_from(host.to_owned()).map_err(|_| "invalid sni".to_string())?;
        connector
            .connect(server_name, stream)
            .await
            .map_err(|e| e.to_string())
    }

    #[tokio::test]
    async fn test_upstream_connect_basic() {
        if !proxy_tests_enabled() {
            return;
        }
        let proxy =
            std::env::var("UPSTREAM_PROXY").unwrap_or_else(|_| "http://127.0.0.1:18080".into());
        let fut = connect_via_upstream(&proxy, "qq.com", 443);
        let res = timeout(Duration::from_secs(10), fut).await;
        match res {
            Ok(Ok(_s)) => { /* ok */ }
            Ok(Err(e)) => panic!("CONNECT via upstream failed: {}", e),
            Err(_) => panic!("CONNECT via upstream timeout"),
        }
    }

    #[tokio::test]
    async fn test_upstream_tls_head_qq() {
        if !proxy_tests_enabled() {
            return;
        }
        let proxy =
            std::env::var("UPSTREAM_PROXY").unwrap_or_else(|_| "http://127.0.0.1:18080".into());
        let s = timeout(
            Duration::from_secs(10),
            connect_via_upstream(&proxy, "qq.com", 443),
        )
        .await
        .expect("timeout")
        .expect("connect");
        let mut tls = tls_over(s, "qq.com").await.expect("tls");
        let req = b"HEAD / HTTP/1.1\r\nHost: qq.com\r\nConnection: close\r\n\r\n";
        tls.write_all(req).await.expect("write");
        let mut buf = vec![0u8; 4096];
        let n = tls.read(&mut buf).await.expect("read");
        assert!(n > 0, "empty response over upstream tls");
    }

    struct CursorStream {
        cursor: std::io::Cursor<Vec<u8>>,
    }
    impl CursorStream {
        fn new(data: Vec<u8>) -> Self {
            Self {
                cursor: std::io::Cursor::new(data),
            }
        }
    }
    impl tokio::io::AsyncRead for CursorStream {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let mut tmp = vec![0u8; buf.remaining()];
            let read = std::io::Read::read(&mut self.cursor, &mut tmp)?;
            buf.put_slice(&tmp[..read]);
            std::task::Poll::Ready(Ok(()))
        }
    }
    impl tokio::io::AsyncWrite for CursorStream {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Ok(0))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn test_read_http_response_head_handles_split_headers() {
        // Simulate headers split across reads: first chunk ends mid-headers
        let part1 = b"HTTP/1.1 302 Found\r\nServer: stgw\r\nDate: Wed, 17 Sep 2025 07:39:37 GMT\r\nContent-Type: text/html\r\nConte".to_vec();
        let part2 = b"nt-Length: 137\r\nLocation: https://www.qq.com/\r\n\r\nBODY".to_vec();
        let mut stream = CursorStream::new([part1, part2].concat());
        let (code, ver, reason, headers, body) = read_http_response_head(&mut stream)
            .await
            .expect("parse head");
        assert_eq!(code, 302);
        assert_eq!(ver, "1.1");
        assert_eq!(reason, "Found");
        let loc = headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("Location"))
            .map(|h| h.value.clone())
            .unwrap_or_default();
        assert_eq!(loc, "https://www.qq.com/");
        assert_eq!(&body[..], b"BODY");
    }

    #[tokio::test]
    async fn test_local_tls_handshake_with_generated_cert() {
        // Use a temp MITM dir to avoid interfering with real files
        let tempdir = tempfile::tempdir().expect("tempdir");
        std::env::set_current_dir(&tempdir).expect("chdir");

        // Prepare CA and leaf for host
        let (ca_pem, ca_key) = crate::ca::ensure_ca_exists().expect("ensure ca");
        let (leaf_der, leaf_key_der, ca_der) = crate::ca::generate_leaf_cert_for_host(
            "test.local",
            &ca_pem,
            &ca_key,
        ).expect("leaf");

        // Build server config
        let certs = vec![CertificateDer::from(leaf_der)];
        let pkcs8: PrivatePkcs8KeyDer<'static> = PrivatePkcs8KeyDer::from(leaf_key_der);
        let priv_key = PrivateKeyDer::Pkcs8(pkcs8);
        let mut server_cfg = RustlsServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, priv_key)
            .map_err(|e| e.to_string()).expect("server cfg");
        server_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = TlsAcceptor::from(std::sync::Arc::new(server_cfg));

        // Start server
        let listener = TokioTcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.expect("accept");
            let _tls = acceptor.accept(&mut s).await.expect("accept tls");
            // Close immediately after handshake
        });

        // Build client trust store with our CA
        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from(ca_der)).expect("add root");
        let cfg = std::sync::Arc::new(ClientConfig::builder().with_root_certificates(roots).with_no_client_auth());
        let connector = TlsConnector::from(cfg);
        let server_name = ServerName::try_from("test.local").expect("sni");

        // Connect and complete handshake
        let tcp = TcpStream::connect(addr).await.expect("connect");
        let _ = timeout(Duration::from_secs(5), connector.connect(server_name, tcp))
            .await
            .expect("timeout")
            .expect("handshake");
    }

    fn integration_tests_enabled() -> bool {
        std::env::var("RUN_PROXY_CURL_TESTS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }

    // 启动完整代理，使用 curl 通过代理顺序请求两个 HTTPS 站点，验证不会 400/卡死
    // 该测试默认跳过，仅当设置 RUN_PROXY_CURL_TESTS=1 时才运行
    #[tokio::test]
    async fn test_proxy_curl_sequence_https() {
        if !integration_tests_enabled() {
            return;
        }

        // 放宽限制以便在未安装系统 CA 时也能 MITM
        unsafe { std::env::set_var("FORCE_MITM", "1"); }
        unsafe { std::env::set_var("PROXY_DEBUG", "1"); }

        // 使用 tauri::test 提供的 MockRuntime 与上下文，避免与真实配置冲突
        let app = mock_builder().build(mock_context(noop_assets())).expect("build mock app");
        let handle = app.handle();

        // 启动代理监听固定测试端口
        let port = 13808u16;
        let addr = format!("127.0.0.1:{}", port);
        let start = super::start_proxy::<MockRuntime, _>(handle.clone(), addr.clone(), None);
        let _ = timeout(Duration::from_secs(3), start)
            .await
            .expect("start proxy timeout")
            .expect("start proxy failed");

        // 给监听一点时间起来
        tokio::time::sleep(Duration::from_millis(200)).await;

        // 1) 访问 baidu.com
        let out1 = Command::new("curl")
            .args([
                "-skL",
                "--max-time", "15",
                "--proxy", &format!("http://{}", addr),
                "https://baidu.com",
                "-o", "/dev/null",
                "-w", "%{http_code}",
            ])
            .output()
            .expect("run curl1");
        assert!(out1.status.success(), "curl1 exit status: {:?}", out1.status);
        let code1 = String::from_utf8_lossy(&out1.stdout).to_string();
        assert!(code1.starts_with("2") || code1.starts_with("3"), "unexpected code1={}", code1);

        // 2) 再访问 api.cherry-ai.com
        let out2 = Command::new("curl")
            .args([
                "-skL",
                "--max-time", "15",
                "--proxy", &format!("http://{}", addr),
                "https://api.cherry-ai.com",
                "-o", "/dev/null",
                "-w", "%{http_code}",
            ])
            .output()
            .expect("run curl2");
        assert!(out2.status.success(), "curl2 exit status: {:?}", out2.status);
        let code2 = String::from_utf8_lossy(&out2.stdout).to_string();
        // 允许 200/302 等，拒绝 400 和 000（网络错误）
        assert!(code2 != "000" && !code2.starts_with("4"), "unexpected code2={}", code2);
    }
}
