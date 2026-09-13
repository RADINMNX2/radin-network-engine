// main.dart — RADIN mobile UI scaffold (spec 30/40).
//
// Build-required scaffold: must be verified with `flutter analyze` on a
// Flutter-capable machine. Renders only what the Rust engine reports; the UI
// cannot invent numbers. The report is always shown with its measured source
// so a synthetic/harness build is visibly labelled, never mistaken for real.

import 'package:flutter/material.dart';

import 'ffi/radin_ffi.dart' as r;

void main() {
  runApp(const RadinApp());
}

class RadinApp extends StatelessWidget {
  const RadinApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'RADIN',
      theme: ThemeData(colorSchemeSeed: Colors.indigo, useMaterial3: true),
      home: const DiagnosticScreen(),
    );
  }
}

class DiagnosticScreen extends StatefulWidget {
  const DiagnosticScreen({super.key});

  @override
  State<DiagnosticScreen> createState() => _DiagnosticScreenState();
}

class _DiagnosticScreenState extends State<DiagnosticScreen> {
  Map<String, dynamic>? _report;
  String? _error;

  @override
  void initState() {
    super.initState();
    _refresh();
  }

  Future<void> _refresh() async {
    try {
      final lib = r.RadinEngine.libraryName();
      // Config JSON mirrors EngineConfig (see rust ffi tests). In the real
      // app the edge set is provisioned over the control plane.
      final engine = r.RadinEngine(
        nativeLib: lib,
        engineConfigJson: '{"supported_transports":[0,1]}',
      );
      final report = engine.diagnostic(nowMs: DateTime.now().millisecondsSinceEpoch);
      engine.dispose();
      setState(() => _report = report);
    } catch (e) {
      setState(() => _error = e.toString());
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('RADIN diagnostic')),
      floatingActionButton: FloatingActionButton(
        onPressed: _refresh,
        child: const Icon(Icons.refresh),
      ),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          if (_error != null)
            ListTile(
              leading: const Icon(Icons.error_outline),
              title: Text('ffi error: $_error'),
            ),
          if (_report != null) ...[
            Text('Selected edge: ${_report!['selected_edge'] ?? '—'}'),
            Text('Transport: ${_report!['transport'] ?? '—'}'),
            Text('RTT: ${_report!['rtt_ms']?.toStringAsFixed(1) ?? '—'} ms'),
            Text('Jitter: ${_report!['jitter_ms']?.toStringAsFixed(1) ?? '—'} ms'),
            Text('Loss: ${_report!['packet_loss_pct']?.toStringAsFixed(2) ?? '—'} %'),
            Text('Route changes: ${_report!['route_changes'] ?? 0}'),
            Text('Fallbacks: ${_report!['transport_fallbacks'] ?? 0}'),
            Text('Circuit trips: ${_report!['circuit_trips'] ?? 0}'),
            const Divider(),
            Text('Source: ${_report!['source'] ?? 'unknown'}',
                style: const TextStyle(fontStyle: FontStyle.italic)),
          ]
        ],
      ),
    );
  }
}