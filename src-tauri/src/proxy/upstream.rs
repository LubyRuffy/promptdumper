use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::http_shared::Header;
use base64::Engine as _;

// Establish CONNECT through an upstream HTTP proxy
pub(crate) async fn connect_via_upstream(
    proxy_url: &str,
    dst_host: &str,
    dst_port: u16,
) -> Result<TcpStream, String> {
    let url = proxy_url.trim();
    let without_scheme = url.strip_prefix("http://").ok_or("only http upstream supported")?;
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

    let mut s = TcpStream::connect(format!("{}:{}", phost, pport)).await.map_err(|e| e.to_string())?;
    let auth_header = if !user.is_empty() {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", user, pass));
        format!("Proxy-Authorization: Basic {}\r\n", token)
    } else { String::new() };
    let connect_req = format!(
        "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\n{}Proxy-Connection: Keep-Alive\r\n\r\n",
        dst_host, dst_port, dst_host, dst_port, auth_header
    );
    s.write_all(connect_req.as_bytes()).await.map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 4096];
    let n = s.read(&mut buf).await.map_err(|e| e.to_string())?;
    if n == 0 { return Err("upstream proxy closed".into()); }
    let head = String::from_utf8_lossy(&buf[..n]);
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return Err(format!("upstream proxy CONNECT failed: {}", head.lines().next().unwrap_or("")));
    }
    Ok(s)
}

// Read response head from an AsyncRead stream; returns head info and first body bytes
pub(crate) async fn read_http_response_head<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<(u16, String, String, Vec<Header>, Bytes), String> {
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut tmp = vec![0u8; 8192];
    let max = 1024 * 256; // 256 KiB cap for headers
    let head_end;
    loop {
        if buf.len() > max { return Err("response header too large".into()); }
        let n = reader.read(&mut tmp).await.map_err(|e| e.to_string())?;
        if n == 0 { return Err("upstream closed before sending headers".into()); }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = memchr::memmem::find(&buf, b"\r\n\r\n") { head_end = pos; break; }
    }
    let first_line_end = memchr::memchr(b'\n', &buf).unwrap_or(buf.len());
    let first = String::from_utf8_lossy(&buf[..first_line_end]).to_string();
    let mut headers_acc = Vec::<Header>::new();
    for line in String::from_utf8_lossy(&buf[..head_end]).split("\r\n").skip(1) {
        if line.is_empty() { break; }
        if let Some((name, val)) = line.split_once(':') {
            headers_acc.push(Header { name: name.trim().to_string(), value: val.trim().to_string() });
        }
    }
    let mut scode: u16 = 200;
    let mut version = "1.1".to_string();
    let mut reason = String::new();
    if first.starts_with("HTTP/") {
        let parts: Vec<&str> = first.trim().splitn(3, ' ').collect();
        if parts.len() >= 2 {
            version = parts[0].trim_start_matches("HTTP/").to_string();
            scode = parts[1].parse::<u16>().unwrap_or(200);
            if parts.len() == 3 { reason = parts[2].trim().to_string(); }
        }
    }
    let body_slice = if head_end + 4 < buf.len() { Bytes::copy_from_slice(&buf[head_end + 4..]) } else { Bytes::new() };
    Ok((scode, version, reason, headers_acc, body_slice))
}

// Bidirectional tunnel with eager close
pub(crate) async fn tunnel_with_eager_close(
    inbound: &mut TcpStream,
    mut upstream: TcpStream,
) -> Result<(), std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut in_r, mut in_w) = inbound.split();
    let (mut up_r, mut up_w) = upstream.split();

    let client_to_upstream = async {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = in_r.read(&mut buf).await?;
            if n == 0 {
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


