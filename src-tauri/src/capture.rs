use std::collections::{VecDeque, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::thread::yield_now;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use base64::{engine::general_purpose, Engine as _};
use once_cell::sync::Lazy;
use pcap::{Active, Capture, Device, Linktype};
use pcap::Error as PcapError;
use etherparse::{SlicedPacket, InternetSlice, TransportSlice};
use tauri::Emitter;
use rand::{distributions::Alphanumeric, Rng};
use serde::{Serialize, Deserialize};
use regex::Regex;
use thiserror::Error;
use time::OffsetDateTime;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("pcap error: {0}")]
    Pcap(String),
    #[error("device not found: {0}")]
    DeviceNotFound(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkInterfaceInfo {
    pub name: String,
    pub desc: Option<String>,
    pub ip: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HttpRequestEvent {
    pub id: String,
    pub timestamp: String,
    pub src_ip: String,
    pub src_port: u16,
    pub dst_ip: String,
    pub dst_port: u16,
    pub method: String,
    pub path: String,
    pub version: String,
    pub headers: Vec<Header>,
    pub body_base64: Option<String>,
    pub body_len: usize,
    pub process_name: Option<String>,
    pub pid: Option<i32>,
    pub is_llm: bool,
    pub llm_provider: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HttpResponseEvent {
    pub id: String,
    pub timestamp: String,
    pub src_ip: String,
    pub src_port: u16,
    pub dst_ip: String,
    pub dst_port: u16,
    pub status_code: u16,
    pub reason: Option<String>,
    pub version: String,
    pub headers: Vec<Header>,
    pub body_base64: Option<String>,
    pub body_len: usize,
    pub process_name: Option<String>,
    pub pid: Option<i32>,
    pub is_llm: bool,
    pub llm_provider: Option<String>,
}

#[derive(Debug, Clone)]
struct ConnectionKey {
    a: String,
    b: String,
}

impl ConnectionKey {
    fn new(src_ip: &str, src_port: u16, dst_ip: &str, dst_port: u16) -> Self {
        let left = format!("{}:{}", src_ip, src_port);
        let right = format!("{}:{}", dst_ip, dst_port);
        if left <= right {
            Self { a: left, b: right }
        } else {
            Self { a: right, b: left }
        }
    }
}

impl std::hash::Hash for ConnectionKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.a.hash(state);
        self.b.hash(state);
    }
}

impl PartialEq for ConnectionKey {
    fn eq(&self, other: &Self) -> bool {
        self.a == other.a && self.b == other.b
    }
}

impl Eq for ConnectionKey {}

#[derive(Debug, Default)]
struct ConnectionBuffers {
    req_buf: Vec<u8>,
    resp_buf: Vec<u8>,
    pending_request_ids: VecDeque<String>,
    client_endpoint: Option<(String, u16)>,
    server_endpoint: Option<(String, u16)>,
    streaming_active: bool,
    streaming_resp_id: Option<String>,
    streaming_content_type: Option<String>,
    streaming_llm_provider: Option<String>,
    streaming_headers: Option<Vec<Header>>, 
    pending_llm_provider: VecDeque<Option<String>>,
}

static CONNECTIONS: Lazy<DashMap<ConnectionKey, ConnectionBuffers>> =
    Lazy::new(|| DashMap::new());

static CAPTURE_THREAD: Lazy<Mutex<Option<JoinHandle<()>>>> = Lazy::new(|| Mutex::new(None));
static CAPTURE_RUNNING: AtomicBool = AtomicBool::new(false);

// Cache process lookup results to avoid spawning lsof repeatedly and blocking the capture loop
static PROCESS_CACHE: Lazy<DashMap<u16, (Option<String>, Option<i32>, Instant)>> =
    Lazy::new(|| DashMap::new());
static PROCESS_LOOKUP_INFLIGHT: Lazy<DashMap<u16, ()>> = Lazy::new(|| DashMap::new());
const PROCESS_CACHE_TTL: Duration = Duration::from_secs(10);

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_else(|_| "".into())
}

// -------------------- LLM Rules (configurable) --------------------

#[derive(Debug, Clone, Deserialize)]
struct RawHeaderRule {
    #[serde(default)]
    name_regex: Option<String>,
    #[serde(default)]
    value_regex: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawRuleSide {
    #[serde(default)]
    methods: Option<Vec<String>>, // e.g., ["POST"]
    #[serde(default)]
    path_regex: Option<String>,
    #[serde(default)]
    headers: Option<Vec<RawHeaderRule>>, // all must be satisfied (any header can satisfy each rule)
    #[serde(default)]
    body_contains_any: Option<Vec<String>>, // simple substring contains
}

#[derive(Debug, Clone, Deserialize)]
struct RawLlmRule {
    provider: String,
    #[serde(default)]
    provider_by_port: Option<HashMap<u16, String>>, // per-rule override by server port
    #[serde(default)]
    request: Option<RawRuleSide>,
    #[serde(default)]
    response: Option<RawRuleSide>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawLlmRules {
    rules: Vec<RawLlmRule>,
}

#[derive(Debug, Clone)]
struct HeaderRuleCompiled {
    name: Option<Regex>,
    value: Option<Regex>,
}

#[derive(Debug, Clone)]
struct RuleSideCompiled {
    methods: Option<Vec<String>>, // uppercased
    path: Option<Regex>,
    headers: Vec<HeaderRuleCompiled>,
    body_contains_any: Vec<String>,
}

#[derive(Debug, Clone)]
struct LlmRuleCompiled {
    provider: String,
    provider_by_port: HashMap<u16, String>,
    request: Option<RuleSideCompiled>,
    response: Option<RuleSideCompiled>,
}

#[derive(Debug, Clone)]
struct LlmRules {
    rules: Vec<LlmRuleCompiled>,
}

const DEFAULT_LLM_RULES_JSON: &str = r#"{
  "rules": [
    {
      "provider": "openai_compatible",
      "provider_by_port": { "1234": "lmstudio", "11434": "ollama" },
      "request": {
        "methods": ["POST"],
        "path_regex": "^/v1/(chat/completions|completions)",
        "body_contains_any": ["\"model\"", "\"messages\"", "\"prompt\""]
      },
      "response": {
        "body_contains_any": ["\"choices\""]
      }
    },
    {
      "provider": "ollama",
      "request": {
        "methods": ["POST"],
        "path_regex": "^/api/(generate|chat)"
      },
      "response": {
        "body_contains_any": ["\"response\"", "\"message\"", "\"model\"", "\"choices\""]
      }
    }
  ]
}"#;

fn compile_header_rule(r: &RawHeaderRule) -> Option<HeaderRuleCompiled> {
    let name = match &r.name_regex {
        Some(s) if !s.is_empty() => match Regex::new(s) { Ok(rx) => Some(rx), Err(_) => None },
        _ => None,
    };
    let value = match &r.value_regex {
        Some(s) if !s.is_empty() => match Regex::new(s) { Ok(rx) => Some(rx), Err(_) => None },
        _ => None,
    };
    Some(HeaderRuleCompiled { name, value })
}

fn compile_side(r: &RawRuleSide) -> Option<RuleSideCompiled> {
    let methods = r.methods.as_ref().map(|v| v.iter().map(|s| s.to_ascii_uppercase()).collect::<Vec<_>>());
    let path = match &r.path_regex {
        Some(s) if !s.is_empty() => match Regex::new(s) { Ok(rx) => Some(rx), Err(_) => None },
        _ => None,
    };
    let headers_raw = r.headers.clone().unwrap_or_default();
    let mut headers = Vec::new();
    for hr in headers_raw.iter() {
        if let Some(comp) = compile_header_rule(hr) { headers.push(comp); }
    }
    let body_contains_any = r.body_contains_any.clone().unwrap_or_default();
    Some(RuleSideCompiled { methods, path, headers, body_contains_any })
}

fn compile_rules(raw: RawLlmRules) -> LlmRules {
    let mut rules = Vec::new();
    for rr in raw.rules.into_iter() {
        let request = rr.request.as_ref().and_then(compile_side);
        let response = rr.response.as_ref().and_then(compile_side);
        let provider_by_port = rr.provider_by_port.unwrap_or_default();
        rules.push(LlmRuleCompiled { provider: rr.provider, provider_by_port, request, response });
    }
    LlmRules { rules }
}

fn load_llm_rules_from_json_str(s: &str) -> Option<LlmRules> {
    let raw: RawLlmRules = serde_json::from_str(s).ok()?;
    Some(compile_rules(raw))
}

fn load_llm_rules() -> LlmRules {
    // Try load external file from working directory
    if let Ok(s) = std::fs::read_to_string("llm_rules.json") {
        if let Some(r) = load_llm_rules_from_json_str(&s) { return r; }
    }
    // Fallback: load from default
    load_llm_rules_from_json_str(DEFAULT_LLM_RULES_JSON).unwrap_or(LlmRules { rules: Vec::new() })
}

fn headers_match(compiled: &RuleSideCompiled, headers: &Vec<Header>) -> bool {
    if compiled.headers.is_empty() { return true; }
    // All header rules must be satisfied by some header
    'rules: for hr in compiled.headers.iter() {
        for h in headers.iter() {
            let name_ok = match &hr.name { Some(rx) => rx.is_match(&h.name), None => true };
            let val_ok = match &hr.value { Some(rx) => rx.is_match(&h.value), None => true };
            if name_ok && val_ok { continue 'rules; }
        }
        return false;
    }
    true
}

fn body_contains_any(compiled: &RuleSideCompiled, body_b64: &Option<String>) -> bool {
    if compiled.body_contains_any.is_empty() { return true; }
    let mut body = String::new();
    if let Some(b64) = body_b64 {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
            body = String::from_utf8_lossy(&bytes).to_string();
        }
    }
    compiled.body_contains_any.iter().any(|needle| body.contains(needle))
}

impl LlmRules {
    fn match_request(&self, evt: &HttpRequestEvent) -> Option<String> {
        for r in &self.rules {
            if let Some(side) = &r.request {
                if let Some(ms) = &side.methods { if !ms.iter().any(|m| m == &evt.method.to_ascii_uppercase()) { continue; } }
                if let Some(rx) = &side.path { if !rx.is_match(&evt.path) { continue; } }
                if !headers_match(side, &evt.headers) { continue; }
                if !body_contains_any(side, &evt.body_base64) { continue; }
                // prefer per-rule provider_by_port override (server port is dst_port on request)
                if let Some(p) = r.provider_by_port.get(&evt.dst_port) { return Some(p.clone()); }
                return Some(r.provider.clone());
            }
        }
        None
    }
    fn match_response(&self, evt: &HttpResponseEvent) -> Option<String> {
        for r in &self.rules {
            if let Some(side) = &r.response {
                if !headers_match(side, &evt.headers) { continue; }
                if !body_contains_any(side, &evt.body_base64) { continue; }
                // on response, server port is src_port
                if let Some(p) = r.provider_by_port.get(&evt.src_port) { return Some(p.clone()); }
                return Some(r.provider.clone());
            }
        }
        None
    }
    fn match_text_only(&self, text: &str) -> Option<String> {
        for r in &self.rules {
            if let Some(side) = &r.response {
                if side.body_contains_any.iter().any(|needle| text.contains(needle)) {
                    return Some(r.provider.clone());
                }
            }
        }
        None
    }
}

fn gen_id() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(16)
        .map(char::from)
        .collect()
}

pub fn list_network_interfaces() -> Result<Vec<NetworkInterfaceInfo>, CaptureError> {
    let devices = Device::list().map_err(|e| CaptureError::Pcap(e.to_string()))?;
    Ok(devices
        .into_iter()
        .filter_map(|d| {
            // prefer first IPv4 if present, otherwise first address
            let ip = if let Some(v4) = d
                .addresses
                .iter()
                .find_map(|a| match a.addr {
                    IpAddr::V4(v4) => Some(v4.to_string()),
                    _ => None,
                })
            {
                Some(v4)
            } else if let Some(any_ip) = d.addresses.iter().map(|a| a.addr.to_string()).next() {
                Some(any_ip)
            } else {
                None
            };

            match ip {
                Some(ip) => Some(NetworkInterfaceInfo { name: d.name, desc: d.desc, ip: Some(ip) }),
                None => None, // hide interfaces without IP
            }
        })
        .collect())
}

fn get_linktype(cap: &Capture<Active>) -> Linktype {
    cap.get_datalink()
}

fn extract_l3_payload<'a>(linktype: Linktype, data: &'a [u8]) -> Option<&'a [u8]> {
    match linktype {
        // DLT_EN10MB (Ethernet)
        Linktype(1) => Some(data),
        // DLT_NULL / BSD loopback
        Linktype(0) | Linktype(12) => {
            if data.len() > 4 {
                Some(&data[4..])
            } else {
                None
            }
        }
        _ => Some(data),
    }
}

fn classify_tcp_endpoints_and_payload(
    l3: &[u8],
    linktype: Linktype,
) -> Option<(String, u16, String, u16, Vec<u8>)> {
    let sliced = if linktype == Linktype(1) {
        SlicedPacket::from_ethernet(l3).ok()?
    } else {
        SlicedPacket::from_ip(l3).ok()?
    };

    let (src_ip, dst_ip) = match sliced.net {
        Some(InternetSlice::Ipv4(h)) => (
            IpAddr::V4(Ipv4Addr::from(h.header().source())),
            IpAddr::V4(Ipv4Addr::from(h.header().destination())),
        ),
        Some(InternetSlice::Ipv6(h)) => (
            IpAddr::V6(Ipv6Addr::from(h.header().source())),
            IpAddr::V6(Ipv6Addr::from(h.header().destination())),
        ),
        _ => return None,
    };
    let (src_port, dst_port, payload) = match sliced.transport {
        Some(TransportSlice::Tcp(tcp)) => (tcp.source_port(), tcp.destination_port(), tcp.payload().to_vec()),
        _ => return None,
    };
    Some((src_ip.to_string(), src_port, dst_ip.to_string(), dst_port, payload))
}

fn guess_is_request_from_prefix(payload: &[u8]) -> Option<bool> {
    let max = payload.len().min(64);
    let line_end = payload[..max].iter().position(|&b| b == b'\n').unwrap_or(max);
    let head = std::str::from_utf8(&payload[..line_end]).ok()?.trim_start_matches(['\r','\n']).trim_start();
    if head.starts_with("HTTP/") {
        return Some(false);
    }
    const METHODS: [&str; 9] = [
        "GET ", "POST ", "PUT ", "DELETE ", "HEAD ", "OPTIONS ", "PATCH ", "CONNECT ", "TRACE ",
    ];
    if METHODS.iter().any(|m| head.starts_with(m)) {
        return Some(true);
    }
    None
}

fn parse_http_request(buf: &[u8]) -> Option<(usize, HttpRequestEvent)> {
    // Use a larger header buffer to avoid dropping headers in verbose clients
    let mut headers = [httparse::EMPTY_HEADER; 256];
    let mut req = httparse::Request::new(&mut headers);
    let status = req.parse(buf).ok()?;
    if !status.is_complete() {
        return None;
    }
    let header_len = status.unwrap();
    let method = req.method?.to_string();
    let path = req.path?.to_string();
    let version = format!("1.{}", req.version.unwrap_or(1));
    let mut headers_vec = Vec::new();
    let mut content_length: usize = 0;
    for h in req.headers.iter() {
        let name = h.name.to_string();
        let value = String::from_utf8_lossy(h.value).to_string();
        if name.eq_ignore_ascii_case("content-length") {
            if let Ok(v) = value.trim().parse::<usize>() {
                content_length = v;
            }
        }
        headers_vec.push(Header { name, value });
    }
    let body_start = header_len;
    // If Content-Length is present and body is incomplete, wait for more bytes
    if content_length > 0 && buf.len() < body_start + content_length {
        return None;
    }
    let body_end = (body_start + content_length).min(buf.len());
    let body_slice = &buf[body_start..body_end];
    let body_b64 = if !body_slice.is_empty() {
        Some(general_purpose::STANDARD.encode(body_slice))
    } else {
        None
    };
    // naive LLM detection: JSON body with model field, or path hints
    let mut is_llm = false;
    let mut llm_provider: Option<String> = None;
    if let Ok(s) = std::str::from_utf8(body_slice) {
        if s.trim_start().starts_with('{') && s.contains("\"model\"") {
            is_llm = true;
        }
    }
    if path.contains("/v1/chat/completions") || path.contains("/v1/completions") {
        is_llm = true;
        llm_provider = Some("openai_compatible".into());
    }
    let evt = HttpRequestEvent {
        id: gen_id(),
        timestamp: now_rfc3339(),
        src_ip: String::new(),
        src_port: 0,
        dst_ip: String::new(),
        dst_port: 0,
        method,
        path,
        version,
        headers: headers_vec,
        body_base64: body_b64,
        body_len: body_slice.len(),
        process_name: None,
        pid: None,
        is_llm,
        llm_provider,
    };
    Some((header_len + content_length, evt))
}

fn parse_http_response(buf: &[u8]) -> Option<(usize, HttpResponseEvent)> {
    // Use a larger header buffer to avoid dropping headers
    let mut headers = [httparse::EMPTY_HEADER; 256];
    let mut resp = httparse::Response::new(&mut headers);
    let status = resp.parse(buf).ok()?;
    if !status.is_complete() {
        return None;
    }
    let header_len = status.unwrap();
    let code = resp.code? as u16;
    let version = format!("1.{}", resp.version.unwrap_or(1));
    let mut headers_vec = Vec::new();
    let mut content_length: usize = 0;
    for h in resp.headers.iter() {
        let name = h.name.to_string();
        let value = String::from_utf8_lossy(h.value).to_string();
        if name.eq_ignore_ascii_case("content-length") {
            if let Ok(v) = value.trim().parse::<usize>() {
                content_length = v;
            }
        }
        headers_vec.push(Header { name, value });
    }
    let body_start = header_len;
    // If Content-Length is present and body is incomplete, wait for more bytes
    if content_length > 0 && buf.len() < body_start + content_length {
        return None;
    }
    let body_end = (body_start + content_length).min(buf.len());
    let body_slice = &buf[body_start..body_end];
    let body_b64 = if !body_slice.is_empty() {
        Some(general_purpose::STANDARD.encode(body_slice))
    } else {
        None
    };
    let evt = HttpResponseEvent {
        id: String::new(),
        timestamp: now_rfc3339(),
        src_ip: String::new(),
        src_port: 0,
        dst_ip: String::new(),
        dst_port: 0,
        status_code: code,
        reason: None,
        version,
        headers: headers_vec,
        body_base64: body_b64,
        body_len: body_slice.len(),
        process_name: None,
        pid: None,
        is_llm: false,
        llm_provider: None,
    };
    Some((header_len + content_length, evt))
}

fn enrich_req_with_endpoints(mut evt: HttpRequestEvent, src_ip: &str, src_port: u16, dst_ip: &str, dst_port: u16) -> HttpRequestEvent {
    evt.src_ip = src_ip.to_string();
    evt.src_port = src_port;
    evt.dst_ip = dst_ip.to_string();
    evt.dst_port = dst_port;
    evt
}

fn enrich_resp_with_endpoints(mut evt: HttpResponseEvent, src_ip: &str, src_port: u16, dst_ip: &str, dst_port: u16) -> HttpResponseEvent {
    evt.src_ip = src_ip.to_string();
    evt.src_port = src_port;
    evt.dst_ip = dst_ip.to_string();
    evt.dst_port = dst_port;
    evt
}

#[cfg(target_os = "macos")]
fn try_lookup_process(port: u16, _is_server_side: bool) -> (Option<String>, Option<i32>) {
    // 命中缓存且未过期，直接返回
    if let Some(entry) = PROCESS_CACHE.get(&port) {
        let (name, pid, ts) = (&entry.0, &entry.1, &entry.2);
        if ts.elapsed() < PROCESS_CACHE_TTL {
            return (name.clone(), *pid);
        }
    }

    // 未命中或过期：如果没有正在查询，则异步发起一次 lsof 查询
    if PROCESS_LOOKUP_INFLIGHT.insert(port, ()).is_none() {
        thread::spawn(move || {
            use std::process::Command;
            let mut best: Option<(String, i32, i32)> = None; // (pname, pid, score)
            if let Ok(output) = Command::new("/usr/sbin/lsof").arg("-n").arg("-P").arg(format!("-iTCP:{}", port)).output() {
                if output.status.success() {
                    let s = String::from_utf8_lossy(&output.stdout);
                    for (idx, line) in s.lines().enumerate() {
                        if idx == 0 { continue; }
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        if parts.len() < 2 { continue; }
                        let pname = parts[0].to_string();
                        let pid = match parts[1].parse::<i32>() { Ok(v) => v, Err(_) => continue };
                        // Score based on whole line to avoid picking (ESTABLISHED) token
                        let score = if line.contains(&format!(":{}->", port)) { 3 }
                                    else if line.contains(&format!(":{}", port)) { 1 }
                                    else { 0 };
                        match &best {
                            Some((_, _, bscore)) if *bscore >= score => {}
                            _ => { best = Some((pname.clone(), pid, score)); }
                        }
                    }
                }
            }
            let (name_opt, pid_opt) = match best { Some((p, pid, _)) => (Some(p), Some(pid)), None => (None, None) };
            PROCESS_CACHE.insert(port, (name_opt, pid_opt, Instant::now()));
            PROCESS_LOOKUP_INFLIGHT.remove(&port);
        });
    }

    // 立即返回占位值，不阻塞抓包循环
    (None, None)
}

#[cfg(not(target_os = "macos"))]
fn try_lookup_process(_port: u16, _is_server_side: bool) -> (Option<String>, Option<i32>) {
    (None, None)
}

pub fn start_capture(app: tauri::AppHandle, iface: &str) -> Result<(), CaptureError> {
    if CAPTURE_RUNNING.swap(true, Ordering::SeqCst) {
        return Ok(()); // already running
    }

    // Load LLM rules once at start
    let llm_rules = load_llm_rules();

    let device = Device::list()
        .map_err(|e| CaptureError::Pcap(e.to_string()))?
        .into_iter()
        .find(|d| d.name == iface)
        .ok_or_else(|| CaptureError::DeviceNotFound(iface.to_string()))?;

    let mut cap = Capture::from_device(device)
        .map_err(|e| CaptureError::Pcap(e.to_string()))?
        .promisc(true)
        .snaplen(65535)
        .immediate_mode(true)
        .open()
        .map_err(|e| CaptureError::Pcap(e.to_string()))?;
    // Use non-blocking to allow graceful stop without hanging on next_packet
    cap = cap
        .setnonblock()
        .map_err(|e| CaptureError::Pcap(e.to_string()))?;

    // Only TCP over common HTTP ports (including LM Studio default 1234)
    let _ = cap.filter("tcp", true);

    let linktype = get_linktype(&cap);
    let app_handle = app.clone();
    let llm_rules_for_thread = llm_rules.clone();

    let handle = thread::spawn(move || {
        while CAPTURE_RUNNING.load(Ordering::SeqCst) {
            match cap.next_packet() {
                Ok(packet) => {
                    if let Some(l3) = extract_l3_payload(linktype, packet.data) {
                        if let Some((src_ip, src_port, dst_ip, dst_port, payload)) =
                            classify_tcp_endpoints_and_payload(l3, linktype)
                        {
                            let key = ConnectionKey::new(&src_ip, src_port, &dst_ip, dst_port);
                            let mut state = CONNECTIONS.entry(key).or_insert_with(ConnectionBuffers::default);
                            // Prefer direction by known endpoints; fallback to payload prefix guess
                            let dir_is_req = if let (Some(client), Some(server)) = (&state.client_endpoint, &state.server_endpoint) {
                                if src_ip == server.0 && src_port == server.1 && dst_ip == client.0 && dst_port == client.1 {
                                    false
                                } else if src_ip == client.0 && src_port == client.1 && dst_ip == server.0 && dst_port == server.1 {
                                    true
                                } else {
                                    guess_is_request_from_prefix(&payload).unwrap_or(true)
                                }
                            } else {
                                guess_is_request_from_prefix(&payload).unwrap_or(true)
                            };
                            if dir_is_req {
                                state.client_endpoint.get_or_insert((src_ip.clone(), src_port));
                                state.server_endpoint.get_or_insert((dst_ip.clone(), dst_port));
                                state.req_buf.extend_from_slice(&payload);
                                while let Some((consumed, mut evt)) = parse_http_request(&state.req_buf) {
                                    let (pname, pid) = try_lookup_process(src_port, false);
                                    evt = enrich_req_with_endpoints(evt, &src_ip, src_port, &dst_ip, dst_port);
                                    // Apply request rules
                                    if let Some(provider) = llm_rules_for_thread.match_request(&evt) {
                                        evt.is_llm = true;
                                        evt.llm_provider = Some(provider.clone());
                                    }
                                    let id = evt.id.clone();
                                    state.pending_request_ids.push_back(id.clone());
                                    state.pending_llm_provider.push_back(evt.llm_provider.clone());
                                    evt.process_name = pname;
                                    evt.pid = pid;
                                    if consumed <= state.req_buf.len() { state.req_buf.drain(0..consumed); } else { state.req_buf.clear(); }
                                    let _ = app_handle.emit("onHttpRequest", evt);
                                }
                            } else {
                                state.client_endpoint.get_or_insert((dst_ip.clone(), dst_port));
                                state.server_endpoint.get_or_insert((src_ip.clone(), src_port));
                                state.resp_buf.extend_from_slice(&payload);
                                while let Some((consumed, mut evt)) = parse_http_response(&state.resp_buf) {
                                    if let Some(id) = state.pending_request_ids.pop_front() {
                                        evt.id = id;
                                    } else {
                                        if consumed <= state.resp_buf.len() { state.resp_buf.drain(0..consumed); } else { state.resp_buf.clear(); }
                                        continue;
                                    }
                                    let (pname, pid) = try_lookup_process(dst_port, true);
                                    evt = enrich_resp_with_endpoints(evt, &src_ip, src_port, &dst_ip, dst_port);
                                    evt.process_name = pname;
                                    evt.pid = pid;
                                    // Prefer request side decision, but also try response rules
                                    match state.pending_llm_provider.pop_front() {
                                        Some(p) => { evt.is_llm = p.is_some(); evt.llm_provider = p; },
                                        None => {}
                                    }
                                    if !evt.is_llm {
                                        if let Some(provider) = llm_rules_for_thread.match_response(&evt) {
                                            evt.is_llm = true;
                                            evt.llm_provider = Some(provider.clone());
                                        }
                                    }
                                    if consumed <= state.resp_buf.len() { state.resp_buf.drain(0..consumed); } else { state.resp_buf.clear(); }
                                    let mut is_streaming = false;
                                    let mut resp_ct_header: Option<String> = None;
                                    for h in evt.headers.iter() {
                                        let name = h.name.to_ascii_lowercase();
                                        let val = h.value.to_ascii_lowercase();
                                        if name == "transfer-encoding" && val.contains("chunked") {
                                            is_streaming = true;
                                        }
                                        if name == "content-type" {
                                            resp_ct_header = Some(h.value.clone());
                                            if val.contains("text/event-stream") {
                                                is_streaming = true;
                                                state.streaming_content_type = Some(h.value.clone());
                                            }
                                        }
                                    }
                                    // 对于非 SSE 的 chunked（如 NDJSON），也要带上 content-type，便于前端识别
                                    if is_streaming && state.streaming_content_type.is_none() {
                                        if let Some(ct) = resp_ct_header { state.streaming_content_type = Some(ct); }
                                    }
                                    if is_streaming {
                                        state.streaming_active = true;
                                        state.streaming_resp_id = Some(evt.id.clone());
                                        if evt.is_llm { state.streaming_llm_provider = evt.llm_provider.clone(); }
                                        // 保留首个响应的完整头用于后续 chunk 复用，避免只保留 content-type
                                        state.streaming_headers = Some(evt.headers.clone());
                                    }
                                    let _ = app_handle.emit("onHttpResponse", evt);
                                }
                                if state.streaming_active && !state.resp_buf.is_empty() {
                                    let chunk = std::mem::take(&mut state.resp_buf);
                                    if chunk.is_empty() { /* do not emit empty chunks */ } else {
                                    let mut evt = HttpResponseEvent {
                                        id: state.streaming_resp_id.clone().unwrap_or_else(gen_id),
                                        timestamp: now_rfc3339(),
                                        src_ip: String::new(),
                                        src_port: 0,
                                        dst_ip: String::new(),
                                        dst_port: 0,
                                        status_code: 200,
                                        reason: None,
                                        version: "1.1".into(),
                                        headers: state.streaming_headers.clone().unwrap_or_else(|| match &state.streaming_content_type {
                                            Some(ct) => vec![Header { name: "content-type".into(), value: ct.clone() }],
                                            None => Vec::new(),
                                        }),
                                        body_base64: Some(general_purpose::STANDARD.encode(&chunk)),
                                        body_len: chunk.len(),
                                        process_name: None,
                                        pid: None,
                                        is_llm: state.streaming_llm_provider.is_some(),
                                        llm_provider: state.streaming_llm_provider.clone(),
                                    };
                                    evt = enrich_resp_with_endpoints(evt, &src_ip, src_port, &dst_ip, dst_port);
                                    let (pname, pid) = try_lookup_process(dst_port, true);
                                    evt.process_name = pname;
                                    evt.pid = pid;
                                    // If provider unknown yet, try textual match on chunk
                                    if !evt.is_llm {
                                        if let Ok(text) = String::from_utf8(chunk.clone()) {
                                            if let Some(provider) = llm_rules_for_thread.match_text_only(&text) {
                                                evt.is_llm = true;
                                                evt.llm_provider = Some(provider.clone());
                                                state.streaming_llm_provider = evt.llm_provider.clone();
                                            }
                                        }
                                    }
                                    let _ = app_handle.emit("onHttpResponse", evt);
                                    }
                                }
                                if state.streaming_active {
                                    let done_marker = b"[DONE]";
                                    if payload.windows(done_marker.len()).any(|w| w == done_marker) {
                                        let mut evt = HttpResponseEvent {
                                            id: state.streaming_resp_id.clone().unwrap_or_else(gen_id),
                                            timestamp: now_rfc3339(),
                                            src_ip: String::new(),
                                            src_port: 0,
                                            dst_ip: String::new(),
                                            dst_port: 0,
                                            status_code: 200,
                                            reason: None,
                                            version: "1.1".into(),
                                            headers: state.streaming_headers.clone().unwrap_or_else(|| match &state.streaming_content_type {
                                                Some(ct) => vec![Header { name: "content-type".into(), value: ct.clone() }],
                                                None => Vec::new(),
                                            }),
                                            body_base64: Some(general_purpose::STANDARD.encode(done_marker)),
                                            body_len: done_marker.len(),
                                            process_name: None,
                                            pid: None,
                                            is_llm: state.streaming_llm_provider.is_some(),
                                            llm_provider: state.streaming_llm_provider.clone(),
                                        };
                                        evt = enrich_resp_with_endpoints(evt, &src_ip, src_port, &dst_ip, dst_port);
                                        let (pname, pid) = try_lookup_process(dst_port, true);
                                        evt.process_name = pname;
                                        evt.pid = pid;
                                        let _ = app_handle.emit("onHttpResponse", evt);
                                        state.streaming_active = false;
                                        state.streaming_resp_id = None;
                                        state.streaming_content_type = None;
                                        state.streaming_llm_provider = None;
                                        state.streaming_headers = None;
                                        state.resp_buf.clear();
                                    }
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    match err {
                        PcapError::NoMorePackets => yield_now(),
                        PcapError::TimeoutExpired => yield_now(),
                        _ => std::thread::sleep(Duration::from_millis(1)),
                    }
                }
            }
        }
    });

    {
        let mut g = CAPTURE_THREAD.lock().unwrap();
        *g = Some(handle);
    }

    Ok(())
}

pub fn stop_capture() {
    if !CAPTURE_RUNNING.swap(false, Ordering::SeqCst) {
        return;
    }
    if let Some(handle) = CAPTURE_THREAD.lock().unwrap().take() {
        let _ = handle.join();
    }
    CONNECTIONS.clear();
    PROCESS_CACHE.clear();
    PROCESS_LOOKUP_INFLIGHT.clear();
}


