use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use tokio::net::TcpListener;

use crate::llm_rules::load_llm_rules;

mod parse;
mod upstream;
mod tls;
mod mitm_service;
mod mitm_handlers;
mod mitm_session;
mod flows;

#[cfg(test)]
mod tests;

// Global proxy state and utilities shared across submodules
static PROXY_RUNNING: AtomicBool = AtomicBool::new(false);

// Debug logging switch for proxy module
pub(crate) static PROXY_DEBUG: Lazy<bool> = Lazy::new(|| {
    match std::env::var("PROXY_DEBUG") {
        Ok(v) => v == "1" || v.eq_ignore_ascii_case("true"),
        Err(_) => true, // default on to aid troubleshooting; set PROXY_DEBUG=0 to mute
    }
});

// Macro available to all submodules
#[macro_export]
macro_rules! proxy_log {
    ($($arg:tt)*) => {{
        if *$crate::proxy::PROXY_DEBUG { eprintln!($($arg)*); }
    }};
}

// Upstream proxy config and a connection sequence id
static UPSTREAM_PROXY: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));
pub(crate) static CONN_SEQ: Lazy<AtomicU64> = Lazy::new(|| AtomicU64::new(1));

// Shared helpers
pub type ProxyBody = http_body_util::Full<bytes::Bytes>;
pub(crate) fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(crate) fn http_version_label(ver: http::Version) -> &'static str {
    match ver {
        http::Version::HTTP_09 => "0.9",
        http::Version::HTTP_10 => "1.0",
        http::Version::HTTP_11 => "1.1",
        http::Version::HTTP_2 => "2",
        http::Version::HTTP_3 => "3",
        _ => "",
    }
}

pub(crate) async fn wait_idle(
    last_activity_ms: std::sync::Arc<AtomicU64>,
    inflight: std::sync::Arc<AtomicUsize>,
    idle: tokio::time::Duration,
) {
    use tokio::time::Duration;
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

pub(crate) fn current_upstream_proxy() -> Option<String> {
    UPSTREAM_PROXY.lock().unwrap().clone()
}

// (StartProxyArgs removed; not used within this module)

// Public API
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
                            flows::handle_client::<R, E>(&app_handle, &llm_rules_cloned, &mut inbound, peer).await
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

// Expose commonly used items to submodules via crate::proxy path
pub(crate) use parse::{
    build_plain_http_forward, looks_like_http, parse_connect_target, parse_plain_http_request, ConnectTarget,
    InitialPacket, PlainHttpRequest,
};
pub(crate) use tls::{build_https_client, build_mitm_acceptor, resolve_mitm_flags};
pub(crate) use upstream::{connect_via_upstream, read_http_response_head, tunnel_with_eager_close};
// only re-export the symbols actually referenced across modules to avoid unused warnings
pub(crate) use mitm_service::handle_mitm_request;
pub(crate) use mitm_handlers::{handle_direct_upstream, handle_via_upstream_proxy};
pub(crate) use mitm_session::run_mitm_session;
// don't re-export handle_client here to avoid unused import warnings in other modules
// modules needing it can path-reference flows::handle_client directly


