mod capture;

use capture::{list_network_interfaces as list_ifaces_impl, start_capture as start_capture_impl, stop_capture as stop_capture_impl, NetworkInterfaceInfo};

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![list_network_interfaces, start_capture, stop_capture])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
