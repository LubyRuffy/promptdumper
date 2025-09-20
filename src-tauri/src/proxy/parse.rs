use memchr::{memchr, memmem};
use std::net::SocketAddr;

use crate::http_shared::{Header, HttpRequestEvent, gen_id, now_rfc3339};
use base64::Engine as _;
use base64::engine::general_purpose;

#[derive(Debug, Clone)]
pub(crate) struct InitialPacket {
    pub(crate) data: Vec<u8>,
    head_end: usize,
    first_line: String,
}

impl InitialPacket {
    pub(crate) fn parse(data: Vec<u8>) -> Self {
        let len = data.len();
        let head_end = memmem::find(&data, b"\r\n\r\n").unwrap_or(len);
        let first_line_end = memchr(b'\n', &data).unwrap_or(len);
        let first_line = String::from_utf8_lossy(&data[..first_line_end]).into_owned();
        Self {
            data,
            head_end,
            first_line,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.data.len()
    }
    pub(crate) fn first_line(&self) -> &str {
        &self.first_line
    }
    pub(crate) fn head_bytes(&self) -> &[u8] {
        let end = self.head_end.min(self.data.len());
        &self.data[..end]
    }
    pub(crate) fn body_bytes(&self) -> &[u8] {
        if self.head_end + 4 <= self.data.len() {
            &self.data[self.head_end + 4..]
        } else {
            &[]
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectTarget {
    pub(crate) host: String,
    pub(crate) port: u16,
}

#[derive(Debug, Clone)]
pub(crate) struct PlainHttpRequest {
    pub(crate) method: String,
    pub(crate) full_path: String,
    pub(crate) version: String,
    pub(crate) headers: Vec<Header>,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) body: Vec<u8>,
}

impl PlainHttpRequest {
    pub(crate) fn origin_form_path(&self) -> String {
        if let Some(rest) = self.full_path.strip_prefix("http://") {
            rest.find('/')
                .map(|idx| rest[idx..].to_string())
                .unwrap_or_else(|| "/".to_string())
        } else if let Some(rest) = self.full_path.strip_prefix("https://") {
            rest.find('/')
                .map(|idx| rest[idx..].to_string())
                .unwrap_or_else(|| "/".to_string())
        } else {
            self.full_path.clone()
        }
    }

    pub(crate) fn build_event(
        &self,
        peer: SocketAddr,
        llm_rules: &crate::llm_rules::LlmRules,
    ) -> HttpRequestEvent {
        let mut event = HttpRequestEvent {
            id: gen_id(),
            timestamp: now_rfc3339(),
            src_ip: peer.ip().to_string(),
            src_port: peer.port(),
            dst_ip: self.host.clone(),
            dst_port: self.port,
            method: self.method.clone(),
            // use origin-form path for rule matching and UI consistency
            path: self.origin_form_path(),
            version: self.version.clone(),
            headers: self.headers.clone(),
            body_base64: if self.body.is_empty() {
                None
            } else {
                Some(general_purpose::STANDARD.encode(&self.body))
            },
            body_len: self.body.len(),
            process_name: None,
            pid: None,
            is_llm: false,
            llm_provider: None,
        };

        if let Some(provider) = llm_rules.match_request(&event) {
            event.is_llm = true;
            event.llm_provider = Some(provider);
        }

        event
    }
}

pub(crate) fn looks_like_http(first_line: &str) -> bool {
    if let Some(token) = first_line.split_whitespace().next() {
        if token.eq_ignore_ascii_case("CONNECT") {
            return true;
        }
        const METHODS: &[&str] = &[
            "GET", "POST", "HEAD", "PUT", "DELETE", "OPTIONS", "TRACE", "PATCH",
        ];
        METHODS.iter().any(|m| token.eq_ignore_ascii_case(m))
    } else {
        false
    }
}

pub(crate) fn parse_connect_target(first_line: &str) -> Option<ConnectTarget> {
    let mut parts = first_line.split_whitespace();
    if !parts.next()?.eq_ignore_ascii_case("CONNECT") {
        return None;
    }
    let host_port = parts.next()?;
    let mut hp = host_port.split(':');
    let host = hp.next().unwrap_or("").to_string();
    let port = hp.next().unwrap_or("443").parse::<u16>().unwrap_or(443);
    Some(ConnectTarget { host, port })
}

pub(crate) fn parse_plain_http_request(packet: &InitialPacket) -> Result<PlainHttpRequest, String> {
    let mut headers = Vec::<Header>::new();
    for line in String::from_utf8_lossy(packet.head_bytes())
        .split("\r\n")
        .skip(1)
    {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push(Header {
                name: name.trim().to_string(),
                value: value.trim().to_string(),
            });
        }
    }

    let mut rl = packet.first_line().split_whitespace();
    let method = rl.next().unwrap_or("").to_string();
    let full_path = rl.next().unwrap_or("").to_string();
    let version = rl
        .next()
        .unwrap_or("HTTP/1.1")
        .trim_start_matches("HTTP/")
        .to_string();

    let host_header = headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("host"))
        .map(|h| h.value.clone())
        .unwrap_or_default();
    let (host, port) = if let Some((h, p)) = host_header.split_once(':') {
        (h.to_string(), p.parse::<u16>().unwrap_or(80))
    } else {
        (host_header, 80)
    };

    Ok(PlainHttpRequest {
        method,
        full_path,
        version,
        headers,
        host,
        port,
        body: packet.body_bytes().to_vec(),
    })
}

pub(crate) fn build_plain_http_forward(req: &PlainHttpRequest) -> Vec<u8> {
    let mut forward = Vec::<u8>::new();
    forward.extend_from_slice(
        format!("{} {} HTTP/1.1\r\n", req.method, req.origin_form_path()).as_bytes(),
    );
    for header in req.headers.iter() {
        let lname = header.name.to_ascii_lowercase();
        if lname == "proxy-connection" || lname == "proxy-authorization" {
            continue;
        }
        forward.extend_from_slice(format!("{}: {}\r\n", header.name, header.value).as_bytes());
    }
    forward.extend_from_slice(b"Connection: close\r\n\r\n");
    if !req.body.is_empty() {
        forward.extend_from_slice(&req.body);
    }
    forward
}
