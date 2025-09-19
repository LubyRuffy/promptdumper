use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;

use crate::proxy::{now_millis, wait_idle};
use crate::proxy_log;

use super::mitm_service::{MitmRequestContext, ProxyBody, build_mitm_service};

pub(crate) async fn run_mitm_session<'a, R, E>(
    app: &E,
    llm_rules: &crate::llm_rules::LlmRules,
    peer: SocketAddr,
    host: String,
    port: u16,
    conn_id: u64,
    tls_stream: TlsStream<&'a mut TcpStream>,
    client_base: hyper_util::client::legacy::Client<hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>, ProxyBody>,
) -> Result<(), String>
where
    R: tauri::Runtime,
    E: tauri::Emitter<R> + Clone + Send + Sync + 'static,
{
    let last_activity = Arc::new(std::sync::atomic::AtomicU64::new(now_millis()));
    let inflight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ctx = MitmRequestContext { app: app.clone(), llm_rules: llm_rules.clone(), client: client_base, peer, host: host.clone(), port, conn_id, last_activity: last_activity.clone(), inflight: inflight.clone() };

    let negotiated_h2 = {
        let (_s, conn) = tls_stream.get_ref();
        match conn.alpn_protocol() { Some(proto) => { let bytes: &[u8] = proto.as_ref(); bytes == b"h2" } None => false }
    };
    proxy_log!("[proxy][conn={}] ALPN negotiated: {}", conn_id, if negotiated_h2 { "h2" } else { "http/1.1" });

    if negotiated_h2 {
        let service = build_mitm_service::<R, E>(ctx.clone());
        let io = TokioIo::new(tls_stream);
        use hyper::server::conn::http2;
        proxy_log!("[proxy][conn={}] serving HTTP/2 for {}:{}", conn_id, host, port);
        let started = Instant::now();
        // 可配置的 h2 空闲关闭时间（秒）。0 或未设置表示不因空闲自动关闭。
        let h2_idle_secs = std::env::var("PROXY_H2_IDLE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        let mut h2_builder = http2::Builder::new(hyper_util::rt::TokioExecutor::new());
        h2_builder.timer(hyper_util::rt::TokioTimer::new());
        // 可选的 H2 ping keepalive 设置，避免 NAT 或对端闲置回收
        if let Ok(interval_ms) = std::env::var("PROXY_H2_PING_INTERVAL_MS").and_then(|v| v.parse::<u64>().map_err(|_| std::env::VarError::NotPresent)) {
            if interval_ms > 0 { h2_builder.keep_alive_interval(std::time::Duration::from_millis(interval_ms.into())); }
        }
        if let Ok(timeout_ms) = std::env::var("PROXY_H2_PING_TIMEOUT_MS").and_then(|v| v.parse::<u64>().map_err(|_| std::env::VarError::NotPresent)) {
            if timeout_ms > 0 { h2_builder.keep_alive_timeout(std::time::Duration::from_millis(timeout_ms.into())); }
        }
        let serve_fut = h2_builder.serve_connection(io, service);

        if h2_idle_secs == 0 {
            if let Err(err) = serve_fut.await {
                proxy_log!("[proxy][conn={}] http2 serve_connection error after {:?}: {}", conn_id, started.elapsed(), err);
            }
        } else {
            let idle_task = {
                let last = ctx.last_activity.clone();
                let inflight = ctx.inflight.clone();
                let idle = tokio::time::Duration::from_secs(h2_idle_secs);
                tokio::spawn(async move { wait_idle(last, inflight, idle).await; })
            };
            tokio::select! {
                res = serve_fut => {
                    if let Err(err) = res { proxy_log!("[proxy][conn={}] http2 serve_connection error after {:?}: {}", conn_id, started.elapsed(), err); }
                }
                _ = idle_task => { proxy_log!("[proxy][conn={}] h2 idle {}s; closing session", conn_id, h2_idle_secs); }
            }
        }
    } else {
        let service = build_mitm_service::<R, E>(ctx.clone());
        let io = TokioIo::new(tls_stream);
        proxy_log!("[proxy][conn={}] serving HTTP/1.1 for {}:{} (keep_alive=false)", conn_id, host, port);
        let mut builder = http1::Builder::new();
        builder.keep_alive(false);
        if let Err(e) = builder.serve_connection(io, service).await { proxy_log!("[proxy][conn={}] http1 serve_connection error: {}", conn_id, e); }
    }

    proxy_log!("[proxy][conn={}] CONNECT session ended for {}:{}", conn_id, host, port);
    Ok(())
}


