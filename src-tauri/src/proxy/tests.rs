#![cfg(test)]
use super::*;

#[test]
fn test_looks_like_http_detection() {
    assert!(looks_like_http("GET / HTTP/1.1"));
    assert!(looks_like_http("connect example.com:443 HTTP/1.1"));
    assert!(!looks_like_http("SSH-2.0-OpenSSH_8.9"));
}

#[test]
fn test_parse_connect_target_basic() {
    let target = parse_connect_target("CONNECT example.com:8443 HTTP/1.1\r").unwrap();
    assert_eq!(target.host, "example.com");
    assert_eq!(target.port, 8443);
    let target_default = parse_connect_target("CONNECT example.org HTTP/1.1\r").unwrap();
    assert_eq!(target_default.host, "example.org");
    assert_eq!(target_default.port, 443);
}

#[test]
fn test_parse_plain_http_request() {
    let raw = b"GET http://example.com/index.html HTTP/1.1\r\nHost: example.com\r\nUser-Agent: test\r\n\r\nbody".to_vec();
    let packet = InitialPacket::parse(raw);
    let req = parse_plain_http_request(&packet).expect("parse plain http");
    assert_eq!(req.method, "GET");
    assert_eq!(req.full_path, "http://example.com/index.html");
    assert_eq!(req.version, "1.1");
    assert_eq!(req.host, "example.com");
    assert_eq!(req.port, 80);
    assert_eq!(req.body, b"body");
    assert_eq!(req.origin_form_path(), "/index.html");
}
