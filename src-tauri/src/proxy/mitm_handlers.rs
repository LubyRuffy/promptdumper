use base64::engine::general_purpose;
use base64::Engine as _;
use bytes::Bytes;
use http::{HeaderName, HeaderValue};
use http_body::Frame;
use http_body_util::StreamBody;
use http_body_util::BodyExt;
use hyper::Response;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::http_shared::{Header, HttpResponseEvent, now_rfc3339};
use crate::process_lookup::try_lookup_process;
use crate::proxy::{connect_via_upstream, http_version_label, now_millis, read_http_response_head};
use crate::proxy_log;

use super::mitm_service::{MitmResponse, MitmShared, ParsedClientRequest, build_empty_response, build_outgoing_request};

pub(crate) async fn handle_via_upstream_proxy<R, E>(
    shared: &MitmShared<E>,
    parsed: ParsedClientRequest,
    proxy_url: String,
) -> Result<MitmResponse, hyper::Error>
where
    R: tauri::Runtime,
    E: tauri::Emitter<R> + Clone + Send + Sync + 'static,
{
    let ParsedClientRequest { id, method, path, headers, body, host_header, req_event, .. } = parsed;

    let host = shared.host.clone();
    let port = shared.port;
    let peer_ip = shared.peer.ip().to_string();
    let peer_port = shared.peer.port();

    let upstream_tcp = match connect_via_upstream(&proxy_url, &host, port).await {
        Ok(s) => s,
        Err(_) => {
            proxy_log!("[proxy] upstream CONNECT failed");
            return Ok(build_empty_response(502).await);
        }
    };

    let mut roots = rustls::RootCertStore::empty();
    if let Ok(certs) = rustls_native_certs::load_native_certs() { for c in certs { let _ = roots.add(c); } }
    let client_cfg = std::sync::Arc::new(rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth());
    let tls_conn = tokio_rustls::TlsConnector::from(client_cfg);
    let sni_leaked: &'static str = Box::leak(host.clone().into_boxed_str());
    let server_name = rustls::pki_types::ServerName::try_from(sni_leaked).unwrap_or_else(|_| rustls::pki_types::ServerName::try_from("localhost").unwrap());
    let mut upstream_tls = match tls_conn.connect(server_name, upstream_tcp).await {
        Ok(v) => v,
        Err(_) => { proxy_log!("[proxy] upstream TLS connect failed"); return Ok(build_empty_response(502).await); }
    };

    let mut forward = Vec::<u8>::new();
    forward.extend_from_slice(format!("{} {} HTTP/1.1\r\n", method, path).as_bytes());
    let mut has_host = false;
    for h in headers.iter() {
        let lname = h.name.to_ascii_lowercase();
        if lname == "host" { has_host = true; }
        if matches!(lname.as_str(), "proxy-connection" | "proxy-authorization" | "connection" | "te") { continue; }
        forward.extend_from_slice(format!("{}: {}\r\n", h.name, h.value).as_bytes());
    }
    if !has_host { forward.extend_from_slice(format!("Host: {}\r\n", host_header).as_bytes()); }
    forward.extend_from_slice(b"\r\n");
    if !body.is_empty() { forward.extend_from_slice(&body); }

    // 记录向上游代理发送的请求概要
    {
        let headers_preview: String = headers
            .iter()
            .map(|h| {
                let lname = h.name.to_ascii_lowercase();
                let mut v = h.value.clone();
                if matches!(lname.as_str(), "authorization" | "proxy-authorization" | "cookie" | "set-cookie") { v = "***".to_string(); }
                format!("{}: {}", h.name, v)
            })
            .take(20)
            .collect::<Vec<_>>()
            .join(" | ");
        proxy_log!(
            "[proxy][conn={}][req={}] upstream-proxy send: {} {} via={} headers={} body_len={}",
            shared.conn_id, id, method, path, proxy_url, headers_preview, body.len()
        );
    }
    if let Err(_) = AsyncWriteExt::write_all(&mut upstream_tls, &forward).await { return Ok(build_empty_response(502).await); }

    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(16);
    let app_clone = shared.app.clone();
    let id_clone = id.clone();
    let peer_ip_clone = peer_ip.clone();
    let host_clone = host.clone();
    let (scode, version_str, reason_phrase, resp_headers, first_body_slice) = match read_http_response_head(&mut upstream_tls).await {
        Ok(v) => v,
        Err(_) => (200u16, "1.1".to_string(), String::new(), Vec::<Header>::new(), Bytes::new()),
    };
    proxy_log!(
        "[proxy][conn={}][req={}] upstream-proxy resp-head: {} http/{} first_chunk={}B headers_cnt={}",
        shared.conn_id, id_clone, scode, version_str, first_body_slice.len(), resp_headers.len()
    );

    let mut head_evt = HttpResponseEvent {
        id: id_clone.clone(),
        timestamp: now_rfc3339(),
        src_ip: host_clone.clone(),
        src_port: port,
        dst_ip: peer_ip_clone.clone(),
        dst_port: peer_port,
        status_code: scode,
        reason: if reason_phrase.is_empty() { None } else { Some(reason_phrase.clone()) },
        version: version_str.clone(),
        headers: resp_headers.clone(),
        body_base64: if first_body_slice.is_empty() { None } else { Some(general_purpose::STANDARD.encode(&first_body_slice)) },
        body_len: first_body_slice.len(),
        process_name: None,
        pid: None,
        is_llm: false,
        llm_provider: None,
    };
    let (pname2, pid2) = try_lookup_process(peer_port, true);
    if pname2.is_some() || pid2.is_some() { head_evt.process_name = pname2; head_evt.pid = pid2; }
    if req_event.is_llm { head_evt.is_llm = true; head_evt.llm_provider = req_event.llm_provider.clone(); }
    let _ = app_clone.emit("onHttpResponse", head_evt);
    shared.last_activity.store(now_millis(), std::sync::atomic::Ordering::Relaxed);

    if !first_body_slice.is_empty() { let _ = tx.send(Ok(Frame::data(first_body_slice.clone()))).await; }

    let resp_headers_spawn = resp_headers.clone();
    let req_is_llm_spawn = req_event.is_llm;
    let req_provider_spawn = req_event.llm_provider.clone();
    let last_activity_spawn = shared.last_activity.clone();
    let shared_conn_id_for_log = shared.conn_id;
    let id_for_log = id_clone.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65536];
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(30), AsyncReadExt::read(&mut upstream_tls, &mut buf)).await {
                Ok(Ok(n)) if n > 0 => {
                    let chunk = Bytes::copy_from_slice(&buf[..n]);
                    if tx.send(Ok(Frame::data(chunk.clone()))).await.is_err() { break; }
                    proxy_log!(
                        "[proxy][conn={}][req={}] upstream-proxy resp-chunk: {}B",
                        shared_conn_id_for_log, id_for_log, n
                    );
                    let mut chunk_evt = HttpResponseEvent {
                        id: id_clone.clone(),
                        timestamp: now_rfc3339(),
                        src_ip: host_clone.clone(),
                        src_port: port,
                        dst_ip: peer_ip_clone.clone(),
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
                    if pname3.is_some() || pid3.is_some() { chunk_evt.process_name = pname3; chunk_evt.pid = pid3; }
                    if req_is_llm_spawn { chunk_evt.is_llm = true; chunk_evt.llm_provider = req_provider_spawn.clone(); }
                    let _ = app_clone.emit("onHttpResponse", chunk_evt);
                    last_activity_spawn.store(now_millis(), std::sync::atomic::Ordering::Relaxed);
                }
                Ok(Ok(_)) => break,
                _ => break,
            }
        }
    });

    let mut rb = Response::builder().status(scode);
    for h in resp_headers.iter() {
        let lname = h.name.to_ascii_lowercase();
        if matches!(lname.as_str(), "connection" | "proxy-connection" | "keep-alive" | "transfer-encoding" | "content-length" | "upgrade" | "proxy-authenticate" | "proxy-authorization" | "te" | "trailers") { continue; }
        if let (Ok(name), Ok(val)) = (h.name.parse::<HeaderName>(), h.value.parse::<HeaderValue>()) { rb = rb.header(name, val); }
    }
    let body_stream = StreamBody::new(ReceiverStream::new(rx));
    Ok(rb.body(body_stream).unwrap())
}

pub(crate) async fn handle_direct_upstream<R, E>(
    shared: &MitmShared<E>,
    parsed: ParsedClientRequest,
) -> Result<MitmResponse, hyper::Error>
where
    R: tauri::Runtime,
    E: tauri::Emitter<R> + Clone + Send + Sync + 'static,
{
    let out_req = match build_outgoing_request(&parsed) { Ok(r) => r, Err(_) => return Ok(build_empty_response(400).await), };
    // 记录将要发送到目标站的请求概要
    {
        let headers_preview: String = parsed
            .headers
            .iter()
            .map(|h| {
                let lname = h.name.to_ascii_lowercase();
                let mut v = h.value.clone();
                if matches!(lname.as_str(), "authorization" | "proxy-authorization" | "cookie" | "set-cookie") { v = "***".to_string(); }
                format!("{}: {}", h.name, v)
            })
            .take(20)
            .collect::<Vec<_>>()
            .join(" | ");
        proxy_log!(
            "[proxy][conn={}][req={}] direct-upstream send: {} {} headers={} body_len={}",
            shared.conn_id, parsed.id, parsed.method, parsed.path, headers_preview, parsed.body.len()
        );
    }
    let ParsedClientRequest { id, req_event, .. } = parsed;

    let client = shared.client.clone();
    let resp = match client.request(out_req).await { Ok(r) => r, Err(err) => { proxy_log!("[proxy][conn={}] req={} upstream request error: {:?}", shared.conn_id, id, err); return Ok(build_empty_response(502).await); } };

    let status = resp.status();
    let resp_version = resp.version();
    let mut resp_headers = Vec::<Header>::new();
    for (name, value) in resp.headers().iter() { resp_headers.push(Header { name: name.as_str().to_string(), value: value.to_str().unwrap_or("").to_string() }); }

    let peer_port = shared.peer.port();
    proxy_log!(
        "[proxy][conn={}][req={}] direct-upstream resp-head: {} http/{} headers_cnt={}",
        shared.conn_id, id, status.as_u16(), http_version_label(resp_version), resp_headers.len()
    );
    let mut head_evt = HttpResponseEvent {
        id: id.clone(),
        timestamp: now_rfc3339(),
        src_ip: shared.host.clone(),
        src_port: shared.port,
        dst_ip: shared.peer.ip().to_string(),
        dst_port: peer_port,
        status_code: status.as_u16(),
        reason: None,
        version: http_version_label(resp_version).into(),
        headers: resp_headers.clone(),
        body_base64: None,
        body_len: 0,
        process_name: None,
        pid: None,
        is_llm: false,
        llm_provider: None,
    };
    let (pname2, pid2) = try_lookup_process(peer_port, true);
    if pname2.is_some() || pid2.is_some() { head_evt.process_name = pname2; head_evt.pid = pid2; }
    if let Some(provider) = shared.llm_rules.match_response(&head_evt) { head_evt.is_llm = true; head_evt.llm_provider = Some(provider); }
    if req_event.is_llm { head_evt.is_llm = true; head_evt.llm_provider = req_event.llm_provider.clone(); }
    let _ = shared.app.emit("onHttpResponse", head_evt);
    shared.last_activity.store(now_millis(), std::sync::atomic::Ordering::Relaxed);

    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(16);
    let mut upstream_body = resp.into_body();
    let app_clone = shared.app.clone();
    let resp_headers_clone = resp_headers.clone();
    let id_clone = id.clone();
    let peer_ip_clone = shared.peer.ip().to_string();
    let status_code_value = status.as_u16();
    let req_is_llm_spawn = req_event.is_llm;
    let req_provider_spawn = req_event.llm_provider.clone();
    let last_activity_spawn = shared.last_activity.clone();
    let host_spawn = shared.host.clone();
    let port = shared.port;
    let shared_conn_id = shared.conn_id;
    tokio::spawn(async move {
        while let Some(frame_res) = upstream_body.frame().await {
            match frame_res {
                Ok(frame) => {
                    if let Some(data) = frame.data_ref() {
                        let bytes = data.clone();
                        if tx.send(Ok(Frame::data(bytes.clone()))).await.is_err() { break; }
                        proxy_log!(
                            "[proxy][conn={}][req={}] direct-upstream resp-chunk: {}B",
                            shared_conn_id, id_clone, bytes.len()
                        );
                        let mut chunk_evt = HttpResponseEvent {
                            id: id_clone.clone(),
                            timestamp: now_rfc3339(),
                            src_ip: host_spawn.clone(),
                            src_port: port,
                            dst_ip: peer_ip_clone.clone(),
                            dst_port: peer_port,
                            status_code: status_code_value,
                            reason: None,
                            version: http_version_label(resp_version).into(),
                            headers: resp_headers_clone.clone(),
                            body_base64: Some(general_purpose::STANDARD.encode(&bytes)),
                            body_len: bytes.len(),
                            process_name: None,
                            pid: None,
                            is_llm: false,
                            llm_provider: None,
                        };
                        let (pname3, pid3) = try_lookup_process(peer_port, true);
                        if pname3.is_some() || pid3.is_some() { chunk_evt.process_name = pname3; chunk_evt.pid = pid3; }
                        if req_is_llm_spawn { chunk_evt.is_llm = true; chunk_evt.llm_provider = req_provider_spawn.clone(); }
                        let _ = app_clone.emit("onHttpResponse", chunk_evt);
                        last_activity_spawn.store(now_millis(), std::sync::atomic::Ordering::Relaxed);
                    } else if frame.is_trailers() {
                        if tx.send(Ok(frame)).await.is_err() { break; }
                    }
                }
                Err(e) => { let _ = tx.send(Err(e)).await; break; }
            }
        }
    });

    let mut rb = Response::builder().status(status);
    for h in resp_headers.iter() {
        if let (Ok(name), Ok(val)) = (h.name.parse::<HeaderName>(), h.value.parse::<HeaderValue>()) { rb = rb.header(name, val); }
    }
    let body_stream = StreamBody::new(ReceiverStream::new(rx));
    Ok(rb.body(body_stream).unwrap())
}


