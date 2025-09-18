use rand::{Rng, distributions::Alphanumeric};
use serde::Serialize;
use time::OffsetDateTime;

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

pub fn gen_id() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(16)
        .map(char::from)
        .collect()
}

pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "".into())
}
