// radin_ffi.dart — dart:ffi bindings for the RADIN core (spec 40).
//
// Scaffold only: this workspace has no Flutter SDK, so this file is written
// against the stable dart:ffi ABI and is intended to pass `flutter analyze`
// once a toolchain is present. The engine is a pure Rust state machine; the
// UI never fabricates measurements. All JSON strings crossing the boundary
// are owned by the Rust side and freed via radin_free_string.

import 'dart:convert';
import 'dart:ffi';
import 'dart:io' show Platform;
import 'dart:typed_data';

typedef _new_native = Pointer<NativeEngine> Function(Pointer<Uint8> config, UintPtr len);
typedef _new_dart = Pointer<NativeEngine> Function(Pointer<Uint8>, int);

typedef _free_engine_native = Void Function(Pointer<NativeEngine>);
typedef _free_engine_dart = void Function(Pointer<NativeEngine>);

typedef _last_error_native = UintPtr Function(Pointer<NativeEngine>, Pointer<Uint8>, UintPtr);
typedef _last_error_dart = int Function(Pointer<NativeEngine>, Pointer<Uint8>, int);

typedef _set_edges_native = Int32 Function(Pointer<NativeEngine>, Pointer<Uint8>, UintPtr, Uint64);
typedef _set_edges_dart = int Function(Pointer<NativeEngine>, Pointer<Uint8>, int, int);

typedef _observe_native =
    Int32 Function(Pointer<NativeEngine>, Pointer<Uint8>, UintPtr, Double, Double, Int32, Uint64);
typedef _observe_dart = int Function(Pointer<NativeEngine>, Pointer<Uint8>, int, double, double, int, int);

typedef _diagnostic_native = Int32 Function(Pointer<NativeEngine>, Uint64, Pointer<Pointer<Uint8>>);
typedef _diagnostic_dart = int Function(Pointer<NativeEngine>, int, Pointer<Pointer<Uint8>>);

typedef _free_str_native = Void Function(Pointer<Uint8>);
typedef _free_str_dart = void Function(Pointer<Uint8>);

/// Opaque handle: mirrors `#[repr(C)] pub struct RadinEngine` on the Rust side.
final class NativeEngine extends Opaque {}

const int errOk = 0;
const int errNull = -1;
const int errParse = -2;
const int errBadTransport = -3;
const int errPanic = -4;

/// View a Dart [Uint8List] as a [Pointer] for the duration of the call.
Pointer<Uint8> _view(Uint8List bytes) => bytes.isNotEmpty
    ? bytes.buffer.asUint8List(bytes.offsetInBytes, bytes.length).addressOfBytes
    : nullptr.cast<Uint8>();

/// Reads the length-prefixed C string produced by radin_engine_diagnostic_json.
/// Layout (see write_json_out in radin-ffi): the 4 bytes immediately before the
/// returned pointer hold a little-endian u32 byte-length (including the NUL);
/// the referenced bytes follow in the same allocation.
String _readLengthPrefixed(Pointer<Uint8> dataPtr) {
  final hdr = dataPtr.cast<Uint8>().elementAt(-4);
  final lenBytes = [hdr[0], hdr[1], hdr[2], hdr[3]];
  var total = 0;
  for (var i = 3; i >= 0; i--) {
    total = (total << 8) | lenBytes[i];
  }
  if (total <= 0) return '';
  final raw = dataPtr.toTypedData(bytes: total).asUint8List();
  final end = raw.indexOf(0);
  return utf8.decode(end >= 0 ? raw.sublist(0, end) : raw, allowMalformed: true);
}

/// Reads a fixed-capacity C string buffer (used for radin_engine_last_error).
String _readBounded(Pointer<Uint8> ptr, int cap) {
  final raw = ptr.toTypedData(bytes: cap).asUint8List();
  final end = raw.indexOf(0);
  return utf8.decode(end >= 0 ? raw.sublist(0, end) : raw, allowMalformed: true);
}

/// Thin, panic-safe wrapper over the C ABI. Every method maps an `Int32`
/// status to a thrown [StateError] with the Rust `last_error` text attached.
final class RadinEngine {
  RadinEngine._(this._lib, this._handle);

  factory RadinEngine({required String nativeLib, required String engineConfigJson}) {
    final lib = DynamicLibrary.open(nativeLib);
    final newFn = lib.lookupFunction<_new_native, _new_dart>('radin_engine_new');
    final bytes = Uint8List.fromList(utf8.encode(engineConfigJson));
    final handle = newFn(_view(bytes), bytes.length);
    if (handle.address == 0) {
      throw StateError('radin_engine_new returned null (bad EngineConfig JSON)');
    }
    return RadinEngine._(lib, handle);
  }

  final DynamicLibrary _lib;
  final Pointer<NativeEngine> _handle;

  static String libraryName() {
    if (Platform.isAndroid) return 'libradin_ffi.so';
    if (Platform.isIOS) return 'Radin.xcframework/Radin';
    if (Platform.isWindows) return 'radin_ffi.dll';
    return 'libradin_ffi.so';
  }

  void dispose() {
    _lib
        .lookupFunction<_free_engine_native, _free_engine_dart>('radin_engine_free')
        .call(_handle);
  }

  int setEdges(String edgesJson, {required int nowMs}) {
    final fn = _lib.lookupFunction<_set_edges_native, _set_edges_dart>('radin_engine_set_edges');
    final bytes = Uint8List.fromList(utf8.encode(edgesJson));
    return _check(fn(_handle, _view(bytes), bytes.length, nowMs), 'set_edges');
  }

  int observe({
    required String edgeId,
    required double latencyMs,
    required double jitterMs,
    required bool lost,
    required int nowMs,
  }) {
    final fn = _lib.lookupFunction<_observe_native, _observe_dart>('radin_engine_observe');
    final id = Uint8List.fromList(utf8.encode(edgeId));
    final status =
        fn(_handle, _view(id), id.length, latencyMs, jitterMs, lost ? 1 : 0, nowMs);
    return _check(status, 'observe');
  }

  /// Production diagnostic report as a Dart [Map] (spec 30). An engine built
  /// on synthetic measurements refuses to emit one — that refusal propagates
  /// as a non-OK status which surfaces as a thrown error here.
  Map<String, dynamic> diagnostic({required int nowMs}) {
    final fn = _lib
        .lookupFunction<_diagnostic_native, _diagnostic_dart>('radin_engine_diagnostic_json');
    final out = calloc<Pointer<Uint8>>();
    try {
      final status = _check(fn(_handle, nowMs, out), 'diagnostic');
      if (out.value == nullptr) {
        return {};
      }
      final map = jsonDecode(_readLengthPrefixed(out.value)) as Map<String, dynamic>;
      return map;
    } finally {
      final freeStr = _lib.lookupFunction<_free_str_native, _free_str_dart>('radin_free_string');
      if (out.value != nullptr) freeStr(out.value);
      free(out);
    }
  }

  int _check(int status, String op) {
    if (status == errOk) return status;
    final last = _lib
        .lookupFunction<_last_error_native, _last_error_dart>('radin_engine_last_error');
    final buf = calloc<Uint8>(256);
    try {
      final n = last(_handle, buf, 256);
      final detail = n > 0 ? _readBounded(buf, n.toInt()) : '(no detail)';
      throw StateError('$op failed (status=$status): $detail');
    } finally {
      free(buf);
    }
  }
}