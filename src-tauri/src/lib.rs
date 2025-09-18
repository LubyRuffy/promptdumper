mod ca;
mod capture;
pub mod http_shared;
mod llm_rules;
mod process_lookup;
mod proxy;

use capture::{
    NetworkInterfaceInfo, list_network_interfaces as list_ifaces_impl,
    start_capture as start_capture_impl, stop_capture as stop_capture_impl,
};

#[tauri::command]
fn list_network_interfaces() -> Result<Vec<NetworkInterfaceInfo>, String> {
    list_ifaces_impl().map_err(|e| e.to_string())
}

#[derive(Debug, serde::Deserialize)]
struct StartCaptureArgs {
    iface: String,
}

#[tauri::command]
fn start_capture(app: tauri::AppHandle, args: StartCaptureArgs) -> Result<(), String> {
    start_capture_impl(app, &args.iface).map_err(|e| e.to_string())
}

#[tauri::command]
fn stop_capture() {
    stop_capture_impl();
}

#[derive(Debug, serde::Deserialize)]
struct StartProxyCmdArgs {
    addr: Option<String>,
    upstream: Option<String>,
}

#[tauri::command]
async fn start_proxy(app: tauri::AppHandle, args: StartProxyCmdArgs) -> Result<(), String> {
    let addr = args.addr.unwrap_or_else(|| "127.0.0.1:38080".into());
    proxy::start_proxy::<tauri::Wry, _>(app, addr, args.upstream)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn stop_proxy() {
    proxy::stop_proxy();
}

#[tauri::command]
fn ensure_ca() -> Result<(), String> {
    let (cert, _key) = ca::ensure_ca_exists()?;
    match ca::install_ca_to_system_trust(&cert) {
        Ok(()) => Ok(()),
        Err(e) => Err(e),
    }
}

#[tauri::command]
fn is_ca_installed() -> Result<bool, String> {
    ca::is_ca_installed_in_system_trust()
}

#[tauri::command]
fn uninstall_ca() -> Result<(), String> {
    ca::uninstall_ca_from_system_trust()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            list_network_interfaces,
            start_capture,
            stop_capture,
            start_proxy,
            stop_proxy,
            ensure_ca,
            is_ca_installed,
            uninstall_ca
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
