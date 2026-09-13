//! RADIN FFI surface (cbindgen-compatible C ABI).
//!
//! Exposes the pure `RouteEngine` to Flutter/Dart through `dart:ffi`
//! (spec 40 boundary: the pure Rust brain is the single source of routing
//! truth; the UI shows what the engine measured). Keep this surface small and
//! panic-free — all allocation crossing the boundary is owned by the Rust side
//! and returned as opaque handles + JSON snapshots. Every entry re-enters
//! only through a Mutex'd engine, and nothing here ever accepts synthetic
//! measurements.

use std::ffi::c_char;
use std::sync::{Mutex, MutexGuard};

use radin_core::engine::{EdgeObservation, EngineConfig, RouteEngine};
use radin_core::model::{NetworkType, TransportKind, VpnState};

/// Opaque engine handle owned by Rust. `last_error` keeps the most recent
/// diagnostic string available to the host without allocating cross-calls.
#[repr(C)]
pub struct RadinEngine {
    engine: Mutex<RouteEngine>,
    last_error: Mutex<String>,
}

/// C-ABI error codes (negative values carry a `last_error` string).
pub const ERR_OK: i32 = 0;
pub const ERR_NULL: i32 = -1;
pub const ERR_PARSE: i32 = -2;
pub const ERR_BAD_TRANSPORT: i32 = -3;
pub const ERR_PANIC: i32 = -4;

fn record<E: std::fmt::Display>(handle: *mut RadinEngine, prefix: &str, e: E) -> i32 {
    unsafe {
        if !handle.is_null() {
            if let Ok(mut slot) = (*handle).last_error.lock() {
                *slot = format!("{prefix}: {e}");
            }
        }
    }
    ERR_PARSE
}

#[no_mangle]
pub extern "C" fn radin_version() -> *const c_char {
    static V: &[u8] = b"radin/0.1.0\0";
    V.as_ptr() as *const c_char
}

/// Create an engine from a JSON-serialized `EngineConfig`. Returns an opaque
/// handle or NULL on parse failure (see `radin_engine_last_error`).
#[no_mangle]
pub extern "C" fn radin_engine_new(
    config_json: *const c_char,
    config_len: usize,
) -> *mut RadinEngine {
    let out = std::panic::catch_unwind(|| {
        if config_json.is_null() {
            return std::ptr::null_mut();
        }
        let slice = unsafe { std::slice::from_raw_parts(config_json as *const u8, config_len) };
        let cfg = match serde_json::from_slice::<EngineConfig>(slice) {
            Ok(c) => c,
            Err(e) => {
                let _ = e;
                return std::ptr::null_mut();
            }
        };
        Box::into_raw(Box::new(RadinEngine {
            engine: Mutex::new(RouteEngine::new(cfg)),
            last_error: Mutex::new(String::new()),
        }))
    });
    out.unwrap_or(std::ptr::null_mut())
}

/// Free an engine created by `radin_engine_new`.
///
/// # Safety
/// `handle` must be a live `radin_engine_new` result, freed exactly once.
#[no_mangle]
pub unsafe extern "C" fn radin_engine_free(handle: *mut RadinEngine) {
    if !handle.is_null() {
        unsafe {
            drop(Box::from_raw(handle));
        }
    }
}

/// Fault-free messaging: copies the most recent error into `buf` (≤ `cap`).
/// Returns the number of bytes written (0 = no error recorded).
///
/// # Safety
/// `handle` must be a live engine handle; `buf` must point to at least `cap`
/// writable bytes. `cap == 0` is tolerated and writes nothing.
#[no_mangle]
pub unsafe extern "C" fn radin_engine_last_error(
    handle: *const RadinEngine,
    buf: *mut c_char,
    cap: usize,
) -> usize {
    if handle.is_null() || buf.is_null() || cap == 0 {
        return 0;
    }
    let msg = unsafe { (*handle).last_error.lock() }
        .map(|s| s.clone())
        .unwrap_or_default();
    let bytes = msg.as_bytes();
    let n = bytes.len().min(cap.saturating_sub(1));
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, n);
        *buf.add(n) = 0;
    }
    n
}

fn engine_guard(check: *mut RadinEngine) -> Option<MutexGuard<'static, RouteEngine>> {
    if check.is_null() {
        return None;
    }
    unsafe { (*check).engine.lock() }.ok()
}

#[no_mangle]
pub extern "C" fn radin_engine_set_edges(
    handle: *mut RadinEngine,
    edges_json: *const c_char,
    edges_len: usize,
    now_ms: u64,
) -> i32 {
    let result = std::panic::catch_unwind(|| {
        if edges_json.is_null() {
            return ERR_NULL;
        }
        let edge_json = unsafe { std::slice::from_raw_parts(edges_json as *const u8, edges_len) };
        let edges: Vec<radin_core::model::EdgeInfo> = match serde_json::from_slice(edge_json) {
            Ok(e) => e,
            Err(e) => return record(handle, "parse edges", e),
        };
        let mut g = match engine_guard(handle) {
            Some(g) => g,
            None => return ERR_NULL,
        };
        match g.set_edges(edges, now_ms) {
            Ok(_) => ERR_OK,
            Err(e) => record(handle, "set_edges", e),
        }
    });
    result.unwrap_or_else(|_| record(handle, "panic", "set_edges panicked"))
}

#[no_mangle]
pub extern "C" fn radin_engine_set_network(
    handle: *mut RadinEngine,
    network: i32,
    now_ms: u64,
) -> i32 {
    let net = match network {
        0 => NetworkType::Wifi,
        1 => NetworkType::FiveG,
        2 => NetworkType::FourG,
        3 => NetworkType::ThreeG,
        4 => NetworkType::Ethernet,
        _ => NetworkType::Unknown,
    };
    let mut g = match engine_guard(handle) {
        Some(g) => g,
        None => return ERR_NULL,
    };
    g.set_network_type(net, now_ms);
    ERR_OK
}

#[no_mangle]
pub extern "C" fn radin_engine_set_vpn(handle: *mut RadinEngine, vpn: i32, now_ms: u64) -> i32 {
    let state = match vpn {
        0 => VpnState::Disconnected,
        1 => VpnState::Connecting,
        2 => VpnState::Connected,
        _ => VpnState::Disconnected,
    };
    let mut g = match engine_guard(handle) {
        Some(g) => g,
        None => return ERR_NULL,
    };
    g.set_vpn_state(state, now_ms);
    ERR_OK
}

#[no_mangle]
pub extern "C" fn radin_engine_observe(
    handle: *mut RadinEngine,
    edge_id: *const c_char,
    edge_id_len: usize,
    latency_ms: f64,
    jitter_ms: f64,
    lost: i32,
    now_ms: u64,
) -> i32 {
    let result = std::panic::catch_unwind(|| {
        if edge_id.is_null() {
            return ERR_NULL;
        }
        let id = unsafe { std::slice::from_raw_parts(edge_id as *const u8, edge_id_len) };
        let id = String::from_utf8_lossy(id).into_owned();
        let obs = EdgeObservation {
            edge_id: id,
            latency_ms,
            jitter_ms,
            lost: lost != 0,
            handshake_ms: None,
        };
        let mut g = match engine_guard(handle) {
            Some(g) => g,
            None => return ERR_NULL,
        };
        match g.on_edge_observation(&obs, now_ms) {
            Ok(()) => ERR_OK,
            Err(e) => record(handle, "observe", e),
        }
    });
    result.unwrap_or_else(|_| record(handle, "panic", "observe panicked"))
}

#[no_mangle]
pub extern "C" fn radin_engine_report_transport_failure(
    handle: *mut RadinEngine,
    transport: i32,
    now_ms: u64,
) -> i32 {
    let t = match transport_kind(transport) {
        Some(t) => t,
        None => return ERR_BAD_TRANSPORT,
    };
    let mut g = match engine_guard(handle) {
        Some(g) => g,
        None => return ERR_NULL,
    };
    g.report_transport_failure(t, now_ms);
    ERR_OK
}

#[no_mangle]
pub extern "C" fn radin_engine_report_transport_success(
    handle: *mut RadinEngine,
    transport: i32,
    now_ms: u64,
) -> i32 {
    let t = match transport_kind(transport) {
        Some(t) => t,
        None => return ERR_BAD_TRANSPORT,
    };
    let mut g = match engine_guard(handle) {
        Some(g) => g,
        None => return ERR_NULL,
    };
    g.report_transport_success(t, now_ms);
    ERR_OK
}

fn transport_kind(v: i32) -> Option<TransportKind> {
    match v {
        0 => Some(TransportKind::Quic),
        1 => Some(TransportKind::Udp),
        2 => Some(TransportKind::TcpTls),
        3 => Some(TransportKind::Http2),
        4 => Some(TransportKind::WebSocketTls),
        5 => Some(TransportKind::Direct),
        _ => None,
    }
}

/// Marshal a JSON snapshot to `out` as a NUL-terminated C string. The string is
/// length-prefixed in the 4 bytes before the pointer so `radin_free_string` can
/// release the exact allocation. The block layout is:
///   [u32 little-endian byte_len inclusive of NUL][bytes…][0]
fn write_json_out(out: *mut *mut c_char, json: String) -> i32 {
    let len_bytes = (json.len() + 1) as u32; // +1 for the NUL terminator
    let mut blob = Vec::with_capacity(4 + len_bytes as usize);
    blob.extend_from_slice(&len_bytes.to_le_bytes());
    blob.extend_from_slice(json.as_bytes());
    blob.push(0);
    // capacity == len, so into_boxed_slice keeps the same single allocation.
    let raw = Box::into_raw(blob.into_boxed_slice());
    unsafe {
        let data_ptr = (raw as *mut u8).add(4);
        *out = data_ptr as *mut c_char;
    }
    ERR_OK
}

/// Free a string allocated by this crate (see `write_json_out` for layout).
#[no_mangle]
pub extern "C" fn radin_free_string(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let data_ptr = ptr as *mut u8;
        let hdr_ptr = data_ptr.sub(4);
        let mut len_bytes = [0u8; 4];
        std::ptr::copy_nonoverlapping(hdr_ptr, len_bytes.as_mut_ptr(), 4);
        let total = 4 + u32::from_le_bytes(len_bytes) as usize;
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            hdr_ptr, total,
        )));
    }
}

/// Marshal a JSON snapshot to `out` (owned by Rust; free with `radin_free_string`).
#[no_mangle]
pub extern "C" fn radin_engine_diagnostic_json(
    handle: *mut RadinEngine,
    now_ms: u64,
    out: *mut *mut c_char,
) -> i32 {
    let result = std::panic::catch_unwind(|| {
        let g = match engine_guard(handle) {
            Some(g) => g,
            None => return ERR_NULL,
        };
        match g.diagnostic_report(now_ms) {
            Ok(rep) => match serde_json::to_string(&rep) {
                Ok(json) => write_json_out(out, json),
                Err(e) => record(handle, "serialize report", e),
            },
            Err(e) => record(handle, "diagnostic", e),
        }
    });
    result.unwrap_or_else(|_| record(handle, "panic", "diagnostic_json panicked"))
}

#[no_mangle]
pub extern "C" fn radin_engine_current_route_json(
    handle: *mut RadinEngine,
    out: *mut *mut c_char,
) -> i32 {
    let g = match engine_guard(handle) {
        Some(g) => g,
        None => return ERR_NULL,
    };
    let json = match g.current.as_ref() {
        Some(c) => match serde_json::to_string(c) {
            Ok(j) => j,
            Err(e) => return record(handle, "serialize route", e),
        },
        None => "null".to_string(),
    };
    write_json_out(out, json)
}

#[no_mangle]
pub extern "C" fn radin_engine_fail_safe(handle: *mut RadinEngine) -> i32 {
    let g = match engine_guard(handle) {
        Some(g) => g,
        None => return ERR_NULL,
    };
    transport_int(g.fail_safe_action())
}

#[no_mangle]
pub extern "C" fn radin_engine_may_forward_direct(handle: *mut RadinEngine) -> i32 {
    let g = match engine_guard(handle) {
        Some(g) => g,
        None => return -1,
    };
    if g.may_forward_direct() {
        1
    } else {
        0
    }
}

fn transport_int(t: TransportKind) -> i32 {
    match t {
        TransportKind::Quic => 0,
        TransportKind::Udp => 1,
        TransportKind::TcpTls => 2,
        TransportKind::Http2 => 3,
        TransportKind::WebSocketTls => 4,
        TransportKind::Direct => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_json() -> String {
        serde_json::to_string(&EngineConfig {
            supported_transports: vec![
                TransportKind::Quic,
                TransportKind::Udp,
                TransportKind::TcpTls,
            ],
            ..EngineConfig::default()
        })
        .unwrap()
    }

    fn edge_json() -> String {
        let edges = vec![radin_core::model::EdgeInfo {
            id: "Edge-SG-01".into(),
            region: "sgp".into(),
            address: "10.0.0.1".into(),
            supported_transports: vec![
                TransportKind::Quic,
                TransportKind::Udp,
                TransportKind::TcpTls,
            ],
            priority: None,
            expires_at: 1_000_000_000,
            signature_b64: None,
        }];
        serde_json::to_string(&edges).unwrap()
    }

    #[test]
    fn version_string_is_c_string() {
        let v = radin_version();
        let s = unsafe { std::ffi::CStr::from_ptr(v) };
        assert!(s.to_str().unwrap().starts_with("radin/"));
    }

    #[test]
    fn engine_lifecycle_round_trips_through_ffi() {
        let cfg = config_json();
        let handle = radin_engine_new(cfg.as_ptr() as *const c_char, cfg.len());
        assert!(!handle.is_null(), "engine constructs from JSON");

        let edges = edge_json();
        assert_eq!(
            radin_engine_set_edges(handle, edges.as_ptr() as *const c_char, edges.len(), 0),
            ERR_OK
        );
        assert_eq!(radin_engine_set_network(handle, 0, 0), ERR_OK); // Wifi
        assert_eq!(radin_engine_set_vpn(handle, 0, 0), ERR_OK); // Disconnected

        let id = b"Edge-SG-01\0";
        assert_eq!(
            radin_engine_observe(
                handle,
                id.as_ptr() as *const c_char,
                id.len() - 1,
                18.0,
                2.0,
                0,
                10
            ),
            ERR_OK
        );

        let mut out: *mut c_char = std::ptr::null_mut();
        assert_eq!(radin_engine_diagnostic_json(handle, 100, &mut out), ERR_OK);
        assert!(!out.is_null());
        let json = {
            let bytes = unsafe { std::slice::from_raw_parts(out as *const u8, 4096) };
            let len = bytes.iter().position(|b| *b == 0).unwrap_or(0);
            String::from_utf8_lossy(&bytes[..len]).into_owned()
        };
        radin_free_string(out);
        assert!(
            json.contains("selected_edge"),
            "report JSON includes route info: {json}"
        );

        unsafe { radin_engine_free(handle) };
    }

    #[test]
    fn refuse_null_handle_gracefully() {
        let mut out: *mut c_char = std::ptr::null_mut();
        assert_eq!(
            radin_engine_diagnostic_json(std::ptr::null_mut(), 0, &mut out),
            ERR_NULL
        );
    }
}
