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
use tauri::Emitter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
// use rustls_native_certs as native_certs;
use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::http_shared::{Header, HttpRequestEvent, HttpResponseEvent, gen_id, now_rfc3339};
use crate::llm_rules::load_llm_rules;

static PROXY_RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, serde::Deserialize)]
pub struct StartProxyArgs {
    pub addr: String,
}

static UPSTREAM_PROXY: once_cell::sync::Lazy<std::sync::Mutex<Option<String>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(None));

// 注意：我们强制保持 MITM 模式用于抓包，不再做自动绕过

pub async fn start_proxy(
    app: tauri::AppHandle,
    addr: String,
    upstream: Option<String>,
) -> Result<(), String> {
    if PROXY_RUNNING.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    {
        let mut g = UPSTREAM_PROXY.lock().unwrap();
        *g = upstream;
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
                Err(_) => {
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
async fn handle_client(
    app: &tauri::AppHandle,
    llm_rules: &crate::llm_rules::LlmRules,
    inbound: &mut TcpStream,
    peer: SocketAddr,
) -> Result<(), String> {
    // Minimal HTTP/1 CONNECT parser + plain HTTP forwarder placeholder (MITM to be filled)
    let mut buf = vec![0u8; 65536];
    let n = inbound.read(&mut buf).await.map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(());
    }
    let data = &buf[..n];
    let head_end = memchr::memmem::find(data, b"\r\n\r\n").unwrap_or(n);
    let first_line_end = memchr::memchr(b'\n', data).unwrap_or(n);
    let first = String::from_utf8_lossy(&data[..first_line_end]).to_string();
    if first.starts_with("CONNECT ") {
        let host_port = first.split_whitespace().nth(1).unwrap_or("");
        let mut parts = host_port.split(':');
        let host = parts.next().unwrap_or("");
        let port = parts.next().unwrap_or("443").parse::<u16>().unwrap_or(443);
        eprintln!(
            "[proxy] CONNECT from {} => {}:{}",
            peer,
            host,
            port
        );
        // Respond OK to switch to TLS
        inbound
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .map_err(|e| e.to_string())?;

        // If系统未安装我们的根证书，则不要做 MITM，直接按标准 CONNECT 建立隧道，避免客户端因证书校验/钉扎而主动断开
        let can_mitm = match crate::ca::is_ca_installed_in_system_trust() {
            Ok(v) => v,
            Err(_) => false,
        };
        if !can_mitm {
            eprintln!(
                "[proxy] CA not installed or check failed, fallback to pure tunnel for {}:{}",
                host, port
            );
            // Upstream may be another HTTP proxy; otherwise direct connect
            let use_upstream = { UPSTREAM_PROXY.lock().unwrap().clone() };
            let upstream = if let Some(proxy_url) = use_upstream {
                eprintln!("[proxy] tunneling via upstream proxy {}", proxy_url);
                connect_via_upstream(&proxy_url, host, port).await.map_err(|e| e.to_string())?
            } else {
                eprintln!("[proxy] tunneling direct to {}:{}", host, port);
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
        eprintln!("[proxy] MITM enabled; generating leaf cert for {}", host);
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
        // Offer h2 and http/1.1 so clients can negotiate HTTP/2 when supported
        server_cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        // 允许 TLS1.2/1.3，移除不常用椭圆曲线以更贴近常见客户端期望
        server_cfg.max_fragment_size = None;
        let acceptor = TlsAcceptor::from(std::sync::Arc::new(server_cfg));

        // Accept TLS from client
        eprintln!("[proxy] accepting TLS from client for {}:{}", host, port);
        let tls_stream = match acceptor.accept(inbound).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[proxy] client TLS accept failed: {}", e);
                return Err(e.to_string());
            }
        };

        // Hyper client for upstream direct (supports http/1.1 and http/2 over TLS)
        let https = HttpsConnectorBuilder::new()
            .with_native_roots()
            .expect("native roots")
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();
        type ReqBody = Full<Bytes>;
        let client: Client<_, ReqBody> = Client::builder(TokioExecutor::new()).build(https);

        // Serve HTTP over the decrypted TLS stream (HTTP/2 or HTTP/1.1 depending on ALPN)
        let app_handle2 = app.clone();
        let llm_rules2 = llm_rules.clone();
        let host_owned = host.to_string();

        let service = service_fn(move |req: Request<IncomingBody>| {
            let app3 = app_handle2.clone();
            let client3 = client.clone();
            let llm3 = llm_rules2.clone();
            let peer_ip = peer.ip().to_string();
            let peer_port = peer.port();
            let host3 = host_owned.clone();
            async move {
                let (parts, body_in) = req.into_parts();
                let mut headers_vec = Vec::<Header>::new();
                for (name, value) in parts.headers.iter() {
                    headers_vec.push(Header {
                        name: name.as_str().to_string(),
                        value: value.to_str().unwrap_or("").to_string(),
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
                let _ = app3.emit("onHttpRequest", req_evt.clone());

                // Build upstream absolute URI
                let host_header = headers_vec
                    .iter()
                    .find(|h| h.name.eq_ignore_ascii_case("host"))
                    .map(|h| h.value.clone())
                    .unwrap_or(host3.clone());
                let uri = format!("https://{}{}", host_header, path_q);

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
                // copy headers
                for h in headers_vec.iter() {
                    if let (Ok(name), Ok(val)) =
                        (h.name.parse::<HeaderName>(), h.value.parse::<HeaderValue>())
                    {
                        out_req.headers_mut().append(name, val);
                    }
                }

                // 上游代理生效：若配置了上游，则通过上游 CONNECT + TLS，手工写入请求并流式回传
                let use_upstream = { UPSTREAM_PROXY.lock().unwrap().clone() };
                if let Some(proxy_url) = use_upstream {
                    // 1) 建立到上游的 CONNECT 隧道
                    let upstream_tcp = match connect_via_upstream(&proxy_url, &host3, port).await {
                        Ok(s) => s,
                        Err(_) => {
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

                    // emit 首包事件（包含头和首段 body）
                    let head_evt = HttpResponseEvent {
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
                    let _ = app4.emit("onHttpResponse", head_evt);

                    // 把首段 body 送入下游
                    if !first_body_slice.is_empty() {
                        let _ = tx.send(Ok(Frame::data(first_body_slice.clone()))).await;
                    }

                    // 后续 body 流式转发
                    let resp_headers_spawn = resp_headers.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 65536];
                        loop {
                            match upstream_tls.read(&mut buf).await {
                                Ok(n) if n > 0 => {
                                    let chunk = Bytes::copy_from_slice(&buf[..n]);
                                    if tx.send(Ok(Frame::data(chunk.clone()))).await.is_err() {
                                        // downstream gone; stop reading and close upstream
                                        break;
                                    }
                                    let chunk_evt = HttpResponseEvent {
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
                                    let _ = app4.emit("onHttpResponse", chunk_evt);
                                }
                                _ => break,
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
                    Ok::<_, hyper::Error>(rb.body(body).unwrap())
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
                    if let Some(provider) = llm3.match_response(&head_evt) {
                        head_evt.is_llm = true;
                        head_evt.llm_provider = Some(provider);
                    }
                    let _ = app3.emit("onHttpResponse", head_evt);
                    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(16);
                    let mut upstream_body = resp.into_body();
                    let app4 = app3.clone();
                    let resp_headers4 = resp_headers.clone();
                    let id4 = id.clone();
                    let peer_ip4 = peer_ip.clone();
                    let status_code_value = status.as_u16();
                    tokio::spawn(async move {
                        while let Some(frame_res) = upstream_body.frame().await {
                            match frame_res {
                                Ok(frame) => {
                                    if let Some(data) = frame.data_ref() {
                                        let bytes = data.clone();
                                        if tx.send(Ok(Frame::data(bytes.clone()))).await.is_err() {
                                            break;
                                        }
                                        let chunk_evt = HttpResponseEvent {
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
                                        let _ = app4.emit("onHttpResponse", chunk_evt);
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
                    Ok::<_, hyper::Error>(rb.body(body).unwrap())
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
        eprintln!(
            "[proxy] ALPN negotiated: {}",
            if negotiated_h2 { "h2" } else { "http/1.1" }
        );
        if negotiated_h2 {
            let io = TokioIo::new(tls_stream);
            use hyper::server::conn::http2;
            eprintln!("[proxy] serving HTTP/2 for {}:{}", host, port);
            http2::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await
                .map_err(|e| e.to_string())?;
        } else {
            let io = TokioIo::new(tls_stream);
            eprintln!("[proxy] serving HTTP/1.1 for {}:{}", host, port);
            http1::Builder::new()
                .serve_connection(io, service)
                .await
                .map_err(|e| e.to_string())?;
        }
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
        let req_evt = HttpRequestEvent {
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
        eprintln!("[proxy] HTTP forwarded {} bytes", forward.len());

        let mut first_chunk = true;
        let mut total = 0usize;
        let mut resp_headers = Vec::<Header>::new();
        let mut scode: u16 = 200;
        let mut version_str = "1.1".to_string();
        let mut resp_buf = vec![0u8; 65536];
        loop {
            let m = upstream
                .read(&mut resp_buf)
                .await
                .map_err(|e| e.to_string())?;
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
                let chunk_evt = HttpResponseEvent {
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
}
