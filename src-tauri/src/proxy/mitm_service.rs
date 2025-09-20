use base64::Engine as _;
use base64::engine::general_purpose;
use bytes::Bytes;
use http::{HeaderName, HeaderValue};
use http_body::Frame;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Incoming as IncomingBody;
use hyper::{Request, Response};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
// use hyper_util::rt::TokioExecutor;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::http_shared::{Header, HttpRequestEvent, gen_id, now_rfc3339};
// use crate::llm_rules::load_llm_rules;
use crate::process_lookup::try_lookup_process;
use crate::proxy::{current_upstream_proxy, now_millis};
use crate::proxy_log;

pub(crate) type ProxyBody = Full<Bytes>;
pub(crate) type MitmStreamBody = StreamBody<ReceiverStream<Result<Frame<Bytes>, hyper::Error>>>;
pub(crate) type MitmResponse = Response<MitmStreamBody>;

#[derive(Clone)]
pub(crate) struct MitmRequestContext<E> {
    pub(crate) app: E,
    pub(crate) llm_rules: crate::llm_rules::LlmRules,
    pub(crate) client: Client<hyper_rustls::HttpsConnector<HttpConnector>, ProxyBody>,
    pub(crate) peer: std::net::SocketAddr,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) conn_id: u64,
    pub(crate) last_activity: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub(crate) inflight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl<E> MitmRequestContext<E> {
    pub(crate) fn touch_activity(&self) {
        self.last_activity
            .store(now_millis(), std::sync::atomic::Ordering::Relaxed);
    }
}

pub(crate) struct InflightGuard {
    counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl InflightGuard {
    pub(crate) fn new(counter: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self { counter }
    }
}
impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.counter
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

pub(crate) struct MitmShared<E> {
    pub(crate) app: E,
    pub(crate) llm_rules: crate::llm_rules::LlmRules,
    pub(crate) client: Client<hyper_rustls::HttpsConnector<HttpConnector>, ProxyBody>,
    pub(crate) peer: std::net::SocketAddr,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) conn_id: u64,
    pub(crate) last_activity: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

pub(crate) struct ParsedClientRequest {
    pub(crate) id: String,
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: Vec<Header>,
    pub(crate) body: Bytes,
    pub(crate) uri: String,
    pub(crate) host_header: String,
    pub(crate) req_event: HttpRequestEvent,
}

pub(crate) async fn parse_client_request<E>(
    shared: &MitmShared<E>,
    parts: http::request::Parts,
    body_in: IncomingBody,
) -> Result<ParsedClientRequest, hyper::Error> {
    let mut headers_vec = Vec::<Header>::new();
    for (name, value) in parts.headers.iter() {
        headers_vec.push(Header {
            name: name.as_str().to_string(),
            value: value.to_str().unwrap_or("").to_string(),
        });
    }
    if !headers_vec
        .iter()
        .any(|h| h.name.eq_ignore_ascii_case("host"))
    {
        headers_vec.push(Header {
            name: "host".into(),
            value: shared.host.clone(),
        });
    }

    let method_str = parts.method.as_str().to_string();
    let path_q = parts
        .uri
        .path_and_query()
        .map(|x| x.as_str().to_string())
        .unwrap_or("/".to_string());
    let host_header = headers_vec
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("host"))
        .map(|h| h.value.clone())
        .unwrap_or(shared.host.clone());

    // 读取请求体，并在等待过程中输出心跳日志，便于定位卡在 body 的问题
    let expected_len = headers_vec
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("content-length"))
        .and_then(|h| h.value.parse::<usize>().ok());
    let started_wait = std::time::Instant::now();
    proxy_log!(
        "[proxy][conn={}] begin collect body: {} {} expected={:?}",
        shared.conn_id,
        method_str,
        path_q,
        expected_len
    );
    let collect_fut = body_in.collect();
    tokio::pin!(collect_fut);
    let body_bytes = loop {
        tokio::select! {
            res = &mut collect_fut => {
                let collected = res?;
                let bytes_tmp = collected.to_bytes();
                let size = bytes_tmp.len();
                proxy_log!(
                    "[proxy][conn={}] body collected: {} {} elapsed={}ms size={}",
                    shared.conn_id,
                    method_str,
                    path_q,
                    started_wait.elapsed().as_millis(),
                    size,
                );
                break bytes_tmp;
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {
                proxy_log!(
                    "[proxy][conn={}] waiting body: {} {} elapsed={}s expected={:?}",
                    shared.conn_id,
                    method_str,
                    path_q,
                    started_wait.elapsed().as_secs(),
                    expected_len,
                );
            }
        }
    };
    proxy_log!(
        "[proxy][conn={}] build req_event begin: {} {}",
        shared.conn_id,
        method_str,
        path_q
    );
    let id = gen_id();
    let mut req_evt = HttpRequestEvent {
        id: id.clone(),
        timestamp: now_rfc3339(),
        src_ip: shared.peer.ip().to_string(),
        src_port: shared.peer.port(),
        dst_ip: shared.host.clone(),
        dst_port: shared.port,
        method: method_str.clone(),
        path: path_q.clone(),
        version: crate::proxy::http_version_label(parts.version).into(),
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
    proxy_log!(
        "[proxy][conn={}] build req_event done: {} {}",
        shared.conn_id,
        method_str,
        path_q
    );
    proxy_log!(
        "[proxy][conn={}] llm match begin: {} {}",
        shared.conn_id,
        method_str,
        path_q
    );
    if let Some(provider) = shared.llm_rules.match_request(&req_evt) {
        req_evt.is_llm = true;
        req_evt.llm_provider = Some(provider);
    }
    proxy_log!(
        "[proxy][conn={}] llm match done: {} {}",
        shared.conn_id,
        method_str,
        path_q
    );
    proxy_log!(
        "[proxy][conn={}] proc lookup begin: port={} {} {}",
        shared.conn_id,
        shared.peer.port(),
        method_str,
        path_q
    );
    let (pname, pid) = try_lookup_process(shared.peer.port(), false);
    if pname.is_some() || pid.is_some() {
        req_evt.process_name = pname;
        req_evt.pid = pid;
    }
    proxy_log!(
        "[proxy][conn={}] proc lookup done: {} {}",
        shared.conn_id,
        method_str,
        path_q
    );

    proxy_log!(
        "[proxy][conn={}] parse_client_request returning: {} {}",
        shared.conn_id,
        method_str,
        path_q
    );
    Ok(ParsedClientRequest {
        id,
        method: method_str,
        path: path_q.clone(),
        headers: headers_vec,
        body: body_bytes,
        uri: format!("https://{}{}", host_header, path_q),
        host_header,
        req_event: req_evt,
    })
}

pub(crate) fn emit_request_event<R, E>(
    app: &E,
    event: &HttpRequestEvent,
    last_activity: &std::sync::Arc<std::sync::atomic::AtomicU64>,
) where
    R: tauri::Runtime,
    E: tauri::Emitter<R> + Clone + Send + Sync + 'static,
{
    // 先更新活动时间，避免事件通道阻塞导致 idle 判定提前触发
    last_activity.store(
        crate::proxy::now_millis(),
        std::sync::atomic::Ordering::Relaxed,
    );
    let app_clone = app.clone();
    let ev = event.clone();
    tokio::spawn(async move {
        let _ = app_clone.emit("onHttpRequest", ev);
    });
}

pub(crate) fn build_outgoing_request(
    parsed: &ParsedClientRequest,
) -> Result<Request<ProxyBody>, http::Error> {
    let mut out_req = Request::builder()
        .method(parsed.method.as_str())
        .uri(parsed.uri.as_str())
        .body(http_body_util::Full::new(parsed.body.clone()))?;
    for h in parsed.headers.iter() {
        let lname = h.name.to_ascii_lowercase();
        if matches!(
            lname.as_str(),
            "connection"
                | "proxy-connection"
                | "proxy-authorization"
                | "keep-alive"
                | "upgrade"
                | "te"
                | "trailers"
                | "host"
                | "content-length"
                | "transfer-encoding"
        ) {
            continue;
        }
        if let (Ok(name), Ok(val)) = (h.name.parse::<HeaderName>(), h.value.parse::<HeaderValue>())
        {
            out_req.headers_mut().append(name, val);
        }
    }
    Ok(out_req)
}

pub(crate) async fn build_empty_response(status: u16) -> MitmResponse {
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(1);
    let _ = tx.send(Ok(Frame::data(Bytes::new()))).await;
    let body = StreamBody::new(ReceiverStream::new(rx));
    Response::builder().status(status).body(body).unwrap()
}

pub(crate) fn build_mitm_service<R, E>(
    ctx: MitmRequestContext<E>,
) -> impl hyper::service::Service<
    Request<IncomingBody>,
    Response = MitmResponse,
    Error = hyper::Error,
    Future = impl std::future::Future<Output = Result<MitmResponse, hyper::Error>> + Send,
> + Clone
where
    R: tauri::Runtime,
    E: tauri::Emitter<R> + Clone + Send + Sync + 'static,
{
    hyper::service::service_fn(move |req: Request<IncomingBody>| {
        let ctx_for_request = ctx.clone();
        async move { crate::proxy::handle_mitm_request::<R, E>(ctx_for_request, req).await }
    })
}

pub(crate) async fn handle_mitm_request<R, E>(
    ctx: MitmRequestContext<E>,
    req: Request<IncomingBody>,
) -> Result<MitmResponse, hyper::Error>
where
    R: tauri::Runtime,
    E: tauri::Emitter<R> + Clone + Send + Sync + 'static,
{
    ctx.touch_activity();
    let _guard = InflightGuard::new(ctx.inflight.clone());
    process_mitm_request::<R, E>(ctx, req).await
}

pub(crate) async fn process_mitm_request<R, E>(
    ctx: MitmRequestContext<E>,
    req: Request<IncomingBody>,
) -> Result<MitmResponse, hyper::Error>
where
    R: tauri::Runtime,
    E: tauri::Emitter<R> + Clone + Send + Sync + 'static,
{
    let shared = {
        let MitmRequestContext {
            app,
            llm_rules,
            client,
            peer,
            host,
            port,
            conn_id,
            last_activity,
            inflight: _,
        } = ctx;
        MitmShared {
            app,
            llm_rules,
            client,
            peer,
            host,
            port,
            conn_id,
            last_activity,
        }
    };

    let (parts, body_in) = req.into_parts();
    // 先输出仅基于头部的预日志，避免因等待请求体导致“未见日志”的误判
    {
        let method_str = parts.method.as_str().to_string();
        let path_q = parts
            .uri
            .path_and_query()
            .map(|x| x.as_str().to_string())
            .unwrap_or("/".to_string());
        let headers_cnt = parts.headers.len();
        let headers_preview: String = parts
            .headers
            .iter()
            .map(|(name, value)| {
                let lname = name.as_str().to_ascii_lowercase();
                let mut v = value.to_str().unwrap_or("").to_string();
                if matches!(
                    lname.as_str(),
                    "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
                ) {
                    v = "***".to_string();
                }
                format!("{}: {}", name.as_str(), v)
            })
            .take(20)
            .collect::<Vec<_>>()
            .join(" | ");
        proxy_log!(
            "[proxy][conn={}] pre-received: {} {} http/{} headers={} [{}]",
            shared.conn_id,
            method_str,
            path_q,
            crate::proxy::http_version_label(parts.version),
            headers_cnt,
            headers_preview,
        );
    }
    let parsed = parse_client_request(&shared, parts, body_in).await?;

    // 记录收到的客户端请求概要（尽量早于事件派发，排查阻塞）
    {
        let headers_preview: String = parsed
            .headers
            .iter()
            .map(|h| {
                let lname = h.name.to_ascii_lowercase();
                let mut v = h.value.clone();
                if matches!(
                    lname.as_str(),
                    "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
                ) {
                    v = "***".to_string();
                }
                format!("{}: {}", h.name, v)
            })
            .take(20)
            .collect::<Vec<_>>()
            .join(" | ");
        proxy_log!(
            "[proxy][conn={}][req={}] received: {} {} http/{} headers={} [{}] body_len={}",
            shared.conn_id,
            parsed.id,
            parsed.method,
            parsed.path,
            parsed.req_event.version,
            parsed.headers.len(),
            headers_preview,
            parsed.body.len()
        );
    }

    // 异步派发事件，防止在此处阻塞请求处理
    emit_request_event::<R, _>(&shared.app, &parsed.req_event, &shared.last_activity);

    if let Some(proxy_url) = current_upstream_proxy() {
        proxy_log!(
            "[proxy] using upstream {} for {}:{}",
            proxy_url,
            shared.host,
            shared.port
        );
        crate::proxy::handle_via_upstream_proxy::<R, _>(&shared, parsed, proxy_url).await
    } else {
        crate::proxy::handle_direct_upstream::<R, _>(&shared, parsed).await
    }
}
