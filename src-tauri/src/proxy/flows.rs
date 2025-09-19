use base64::engine::general_purpose;
use base64::Engine as _;
use bytes::Bytes;
use memchr::{memchr, memmem};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::http_shared::{Header, HttpResponseEvent, now_rfc3339};
use crate::process_lookup::try_lookup_process;
use crate::proxy::{
    build_https_client, build_mitm_acceptor, connect_via_upstream, current_upstream_proxy,
    looks_like_http, parse_connect_target, parse_plain_http_request, run_mitm_session, tunnel_with_eager_close,
    build_plain_http_forward, InitialPacket, ConnectTarget, resolve_mitm_flags, now_millis, CONN_SEQ,
};
use crate::proxy_log;

pub(crate) async fn read_initial_packet(inbound: &mut TcpStream) -> Result<Option<InitialPacket>, String> {
    let mut buf = vec![0u8; 65536];
    let n = inbound.read(&mut buf).await.map_err(|e| e.to_string())?;
    if n == 0 { return Ok(None); }
    buf.truncate(n);
    Ok(Some(InitialPacket::parse(buf)))
}

pub(crate) async fn handle_client<R: tauri::Runtime, E: tauri::Emitter<R> + Clone + Send + Sync + 'static>(
    app: &E,
    llm_rules: &crate::llm_rules::LlmRules,
    inbound: &mut TcpStream,
    peer: std::net::SocketAddr,
) -> Result<(), String> {
    proxy_log!("[proxy] handle_client begin, peer={}", peer);

    let packet = match read_initial_packet(inbound).await? { Some(pkt) => pkt, None => { proxy_log!("[proxy] client {} closed before sending data", peer); return Ok(()); } };
    proxy_log!("[proxy] handle_client read {} bytes from client {}", packet.len(), peer);

    let first_line = packet.first_line().to_string();
    proxy_log!("[proxy] request first line: {}", first_line.trim());

    if !looks_like_http(&first_line) {
        proxy_log!("[proxy] non-http initial packet from {} -> close", peer);
        return Ok(());
    }

    if first_line.starts_with("CONNECT ") {
        let target = match parse_connect_target(&first_line) { Some(t) => t, None => return Err("invalid CONNECT request".into()), };
        handle_connect_flow::<R, E>(app, llm_rules, inbound, peer, target).await
    } else {
        handle_plain_http_flow::<R, E>(app, llm_rules, inbound, peer, packet).await
    }
}

pub(crate) async fn handle_connect_tunnel(
    inbound: &mut TcpStream,
    peer: std::net::SocketAddr,
    conn_id: u64,
    target: &ConnectTarget,
) -> Result<(), String> {
    let use_upstream = { current_upstream_proxy() };
    let upstream = if let Some(proxy_url) = use_upstream {
        eprintln!("[proxy][conn={}] tunneling via upstream proxy {}", conn_id, proxy_url);
        connect_via_upstream(&proxy_url, &target.host, target.port).await.map_err(|e| e.to_string())?
    } else {
        eprintln!("[proxy][conn={}] tunneling direct to {}:{}", conn_id, target.host, target.port);
        TcpStream::connect(format!("{}:{}", target.host, target.port)).await.map_err(|e| e.to_string())?
    };
    if let Err(e) = tunnel_with_eager_close(inbound, upstream).await { eprintln!("[proxy] tunnel error: {}", e); }
    proxy_log!("[proxy][conn={}] CONNECT tunnel ended for {} from {}", conn_id, target.host, peer);
    Ok(())
}

pub(crate) async fn handle_connect_flow<R, E>(
    app: &E,
    llm_rules: &crate::llm_rules::LlmRules,
    inbound: &mut TcpStream,
    peer: std::net::SocketAddr,
    target: ConnectTarget,
) -> Result<(), String>
where
    R: tauri::Runtime,
    E: tauri::Emitter<R> + Clone + Send + Sync + 'static,
{
    let ConnectTarget { host, port } = target;
    let conn_id = CONN_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    eprintln!("[proxy][conn={}] CONNECT from {} => {}:{}", conn_id, peer, host, port);

    inbound.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.map_err(|e| e.to_string())?;
    proxy_log!("[proxy][conn={}] CONNECT 200 sent to {}, waiting for TLS/next step", conn_id, peer);

    let (force_mitm, sys_mitm) = resolve_mitm_flags();
    let can_mitm = force_mitm || sys_mitm;
    if !can_mitm {
        eprintln!("[proxy][conn={}] CA not installed or check failed, fallback to pure tunnel for {}:{}", conn_id, host, port);
        return handle_connect_tunnel(inbound, peer, conn_id, &ConnectTarget { host: host.clone(), port }).await;
    }

    proxy_log!("[proxy][conn={}] MITM enabled; generating leaf cert for {}", conn_id, host);
    let acceptor = build_mitm_acceptor(&host)?;
    let client_base = build_https_client();

    proxy_log!("[proxy][conn={}] accepting TLS from client for {}:{}", conn_id, host, port);
    let tls_stream = match acceptor.accept(inbound).await { Ok(s) => s, Err(e) => { proxy_log!("[proxy][conn={}] client TLS accept failed: {}", conn_id, e); return Err(e.to_string()); } };
    proxy_log!("[proxy][conn={}] client TLS established for {}:{}", conn_id, host, port);

    run_mitm_session::<R, E>(app, llm_rules, peer, host, port, conn_id, tls_stream, client_base).await
}

pub(crate) async fn handle_plain_http_flow<R, E>(
    app: &E,
    llm_rules: &crate::llm_rules::LlmRules,
    inbound: &mut TcpStream,
    peer: std::net::SocketAddr,
    packet: InitialPacket,
) -> Result<(), String>
where
    R: tauri::Runtime,
    E: tauri::Emitter<R> + Clone + Send + Sync + 'static,
{
    let request = parse_plain_http_request(&packet)?;
    let mut req_evt = request.build_event(peer, llm_rules);
    let (pname_http, pid_http) = try_lookup_process(peer.port(), false);
    if pname_http.is_some() || pid_http.is_some() { req_evt.process_name = pname_http; req_evt.pid = pid_http; }
    let _ = app.emit("onHttpRequest", req_evt.clone());

    let forward = build_plain_http_forward(&request);
    let upstream_addr = format!("{}:{}", request.host, request.port);
    eprintln!("[proxy] HTTP direct connect upstream {}", upstream_addr);
    let mut upstream = TcpStream::connect(&upstream_addr).await.map_err(|e| e.to_string())?;
    upstream.write_all(&forward).await.map_err(|e| e.to_string())?;
    eprintln!("[proxy] HTTP forwarded {} bytes", forward.len());

    stream_plain_http_response::<R, E>(app, inbound, &mut upstream, peer, &request, &req_evt).await
}

pub(crate) async fn stream_plain_http_response<R, E>(
    app: &E,
    inbound: &mut TcpStream,
    upstream: &mut TcpStream,
    peer: std::net::SocketAddr,
    request: &crate::proxy::PlainHttpRequest,
    req_evt: &crate::http_shared::HttpRequestEvent,
) -> Result<(), String>
where
    R: tauri::Runtime,
    E: tauri::Emitter<R> + Clone + Send + Sync + 'static,
{
    let mut first_chunk = true;
    let mut total = 0usize;
    let mut resp_headers = Vec::<Header>::new();
    let mut scode: u16 = 200;
    let mut version_str = "1.1".to_string();
    let mut resp_buf = vec![0u8; 65536];
    let mut sent_any = false;

    loop {
        let m = match tokio::time::timeout(tokio::time::Duration::from_secs(30), upstream.read(&mut resp_buf)).await {
            Ok(Ok(v)) => v,
            _ => { eprintln!("[proxy] upstream read timeout/error"); break; }
        };
        if m == 0 {
            eprintln!("[proxy] upstream closed, total={} bytes", total);
            if !sent_any {
                let body = b"Bad Gateway";
                let resp = format!("HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: {}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n", body.len());
                let _ = inbound.write_all(resp.as_bytes()).await;
                let _ = inbound.write_all(body).await;
            }
            break;
        }
        total += m;
        inbound.write_all(&resp_buf[..m]).await.map_err(|e| e.to_string())?;
        sent_any = true;

        let data = &resp_buf[..m];
        if first_chunk {
            let head_end = memmem::find(data, b"\r\n\r\n").unwrap_or(m);
            let first_line_end = memchr(b'\n', data).unwrap_or(m);
            let first = String::from_utf8_lossy(&data[..first_line_end]).to_string();
            resp_headers.clear();
            for line in String::from_utf8_lossy(&data[..head_end]).split("\r\n").skip(1) {
                if line.is_empty() { break; }
                if let Some((name, val)) = line.split_once(':') { resp_headers.push(Header { name: name.trim().to_string(), value: val.trim().to_string() }); }
            }
            if first.starts_with("HTTP/") {
                let mut it = first.split_whitespace();
                if let Some(v) = it.next() { version_str = v.trim_start_matches("HTTP/").to_string(); }
                if let Some(c) = it.nth(0) { scode = c.parse::<u16>().unwrap_or(200); }
            }
            let body_slice = if head_end < data.len() { &data[head_end + 4..] } else { &[] };
            let first_evt = HttpResponseEvent {
                id: req_evt.id.clone(),
                timestamp: now_rfc3339(),
                src_ip: request.host.clone(),
                src_port: request.port,
                dst_ip: peer.ip().to_string(),
                dst_port: peer.port(),
                status_code: scode,
                reason: None,
                version: version_str.clone(),
                headers: resp_headers.clone(),
                body_base64: if body_slice.is_empty() { None } else { Some(general_purpose::STANDARD.encode(body_slice)) },
                body_len: body_slice.len(),
                process_name: None,
                pid: None,
                // 继承请求的 LLM 标记，确保 UI 显示 raw/pretty/markdown 选项
                is_llm: req_evt.is_llm,
                llm_provider: req_evt.llm_provider.clone(),
            };
            let _ = app.emit("onHttpResponse", first_evt);
            first_chunk = false;
        } else {
            let mut chunk_evt = HttpResponseEvent {
                id: req_evt.id.clone(),
                timestamp: now_rfc3339(),
                src_ip: request.host.clone(),
                src_port: request.port,
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
                // 同样继承请求的 LLM 标记
                is_llm: req_evt.is_llm,
                llm_provider: req_evt.llm_provider.clone(),
            };
            let (pname3, pid3) = try_lookup_process(peer.port(), true);
            if pname3.is_some() || pid3.is_some() { chunk_evt.process_name = pname3; chunk_evt.pid = pid3; }
            let _ = app.emit("onHttpResponse", chunk_evt);
        }
    }
    Ok(())
}


