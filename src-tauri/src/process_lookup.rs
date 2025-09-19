use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::time::{Duration, Instant};

static PROCESS_CACHE: Lazy<DashMap<u16, (Option<String>, Option<i32>, Instant)>> =
    Lazy::new(|| DashMap::new());
static PROCESS_LOOKUP_INFLIGHT: Lazy<DashMap<u16, ()>> = Lazy::new(|| DashMap::new());
const PROCESS_CACHE_TTL: Duration = Duration::from_secs(10);

// Debug switch for process lookup path
static PROC_DEBUG: Lazy<bool> = Lazy::new(|| match std::env::var("PROCESS_LOOKUP_DEBUG") {
    Ok(v) => v == "1" || v.eq_ignore_ascii_case("true"),
    Err(_) => false,
});
macro_rules! plog { ($($arg:tt)*) => {{ if *PROC_DEBUG { eprintln!($($arg)*); } }}; }

#[cfg(target_os = "macos")]
pub fn try_lookup_process(port: u16, is_server_side: bool) -> (Option<String>, Option<i32>) {
    if let Some(entry) = PROCESS_CACHE.get(&port) {
        let (name, pid, ts) = (&entry.0, &entry.1, &entry.2);
        if ts.elapsed() < PROCESS_CACHE_TTL {
            return (name.clone(), *pid);
        }
    }
    // 若缓存没有，触发一次异步查询
    let spawned = PROCESS_LOOKUP_INFLIGHT.insert(port, ()).is_none();
    if spawned {
        std::thread::spawn(move || {
            use std::process::Command;
            let mut best: Option<(String, i32, i32)> = None; // (pname, pid, score)
            let candidates = ["/usr/sbin/lsof", "lsof"];
            for bin in candidates.iter() {
                if let Ok(output) = Command::new(bin)
                    .arg("-n")
                    .arg("-P")
                    .arg(format!("-iTCP:{}", port))
                    .output()
                {
                    if output.status.success() {
                        let s = String::from_utf8_lossy(&output.stdout);
                        for (idx, line) in s.lines().enumerate() {
                            if idx == 0 {
                                continue;
                            }
                            let parts: Vec<&str> = line.split_whitespace().collect();
                            if parts.len() < 2 {
                                continue;
                            }
                            let pname = parts[0].to_string();
                            let pid = match parts[1].parse::<i32>() {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            let score = if line.contains(&format!(":{}->", port)) {
                                3
                            } else if line.contains(&format!(":{}", port)) {
                                1
                            } else {
                                0
                            };
                            match &best {
                                Some((_, _, bscore)) if *bscore >= score => {}
                                _ => {
                                    best = Some((pname.clone(), pid, score));
                                }
                            }
                        }
                        if best.is_some() {
                            break;
                        }
                    }
                }
            }
            let (name_opt, pid_opt) = match best {
                Some((p, pid, _)) => (Some(p), Some(pid)),
                None => (None, None),
            };
            PROCESS_CACHE.insert(port, (name_opt, pid_opt, Instant::now()));
            PROCESS_LOOKUP_INFLIGHT.remove(&port);
        });
    }
    // 等待策略：
    // - 响应方向/流式路径(is_server_side=true)：不等待，避免阻塞 tokio 线程
    // - 请求方向：最多等待可配置毫秒（默认 50ms），尽量在首次展示时带上进程名
    let wait_ms: u64 = if is_server_side {
        0
    } else {
        // 默认不等待，完全异步，避免请求路径被阻塞。
        std::env::var("PROCESS_LOOKUP_WAIT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    };
    if wait_ms == 0 {
        plog!(
            "[proc] scheduled lookup for port {}, return immediately",
            port
        );
        return (None, None);
    }
    let soft_deadline = Instant::now() + Duration::from_millis(wait_ms);
    while Instant::now() < soft_deadline {
        if let Some(entry) = PROCESS_CACHE.get(&port) {
            let (name, pid, ts) = (&entry.0, &entry.1, &entry.2);
            if ts.elapsed() < PROCESS_CACHE_TTL {
                plog!(
                    "[proc] cache hit after wait: port={} name={:?} pid={:?}",
                    port,
                    name,
                    pid
                );
                return (name.clone(), *pid);
            } else {
                PROCESS_CACHE.remove(&port);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    plog!("[proc] wait timeout for port {}", port);
    (None, None)
}

#[cfg(not(target_os = "macos"))]
pub fn try_lookup_process(_port: u16, _is_server_side: bool) -> (Option<String>, Option<i32>) {
    (None, None)
}

pub fn clear_process_lookup() {
    PROCESS_CACHE.clear();
    PROCESS_LOOKUP_INFLIGHT.clear();
}
