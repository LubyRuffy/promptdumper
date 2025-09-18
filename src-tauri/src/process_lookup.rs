use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::time::{Duration, Instant};

static PROCESS_CACHE: Lazy<DashMap<u16, (Option<String>, Option<i32>, Instant)>> =
    Lazy::new(|| DashMap::new());
static PROCESS_LOOKUP_INFLIGHT: Lazy<DashMap<u16, ()>> = Lazy::new(|| DashMap::new());
const PROCESS_CACHE_TTL: Duration = Duration::from_secs(10);

#[cfg(target_os = "macos")]
pub fn try_lookup_process(port: u16, _is_server_side: bool) -> (Option<String>, Option<i32>) {
    if let Some(entry) = PROCESS_CACHE.get(&port) {
        let (name, pid, ts) = (&entry.0, &entry.1, &entry.2);
        if ts.elapsed() < PROCESS_CACHE_TTL {
            return (name.clone(), *pid);
        }
    }
    if PROCESS_LOOKUP_INFLIGHT.insert(port, ()).is_none() {
        std::thread::spawn(move || {
            use std::process::Command;
            let mut best: Option<(String, i32, i32)> = None; // (pname, pid, score)
            if let Ok(output) = Command::new("/usr/sbin/lsof")
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
