# RADIN Test Matrix (spec 38)

The chaos grammar in `crates/radin-chaos` defines how packets behave, and
`tests/test_matrix.rs` validates parameters across the spec matrix. These are
the axes every profile sweeps:

## Loss axis

| Ratio | Meaning                         |
|-------|---------------------------------|
| 0.0   | loss-free path                  |
| 0.005 | 0.5% — negligible               |
| 0.01  | 1% — healthy-ish                |
| 0.03  | 3% — hysteresis degradation     |
| 0.05  | 5% — degraded                   |
| 0.10  | 10% — likely switch (heavy loss)|

## Jitter axis

`1 / 5 / 20 / 50` ms (single-sample jitter sampled from `[-j, +j]`).

## Latency axis

`30 / 70 / 120 / 200 / 300` ms base RTT.

## Failure modes (profiles)

- `ResetEveryPackets(n)` — the endpoint dies/renews every `n` packets
  (outage simulation).
- `Intermittent { down_ratio }` — down/up oscillation (e.g. `down_ratio 0.5`).
- Combined with latency/jitter/loss for `with_latency`, `with_jitter`.

## What `test_matrix.rs` asserts

For every axis value (and the presets above):

- `fates_for(profile, n, seed, tick)` produces exactly `n` packet fates with a
  deterministic seed (repeatable runs).
- `ResetEveryPackets(50)` renews at packet 50, 100, … (`.connection_down`
  boundary honored).
- `Intermittent` with `down_ratio 0.5` marks ~half the window down.
- `synthetic_marker_is_never_false` — chaos karma is always `MeasuredSource::Synthetic`
  (the honesty marker is never repurposed).

## Chaos → engine → scenarios

`crates/radin-core/tests/failover_scenarios.rs` feeds chaos-shaped packet fates
into `SimEdge` observations (per-edge ProbeWindows with EWMA stats + loss) and
drives `RouteEngine`. The scenario suite pins the behavioral contract:

1. fast path stays stable under healthy conditions,
2. sustained degradation switches routes,
3. total outage marches the chain to Direct under `AllowDirectFallback`,
4. StrictVpnOnly never leaks direct (`may_forward_direct` gate),
5. recovery re-selects the best edge and health climbs (sustained),
6. circuit breakers quarantine a failing transport (Open → HalfOpen),
7. loss/jitter spikes enter the event log as structured events,
8. telemetry is honest end-to-end (real samples only),
9. restrictive networks degrade the best edge but never crash the engine.

## Evidence

The workspace ships with 144 passing tests across all crates/features and a
zero-warning build. Run:

    cargo test --workspace --all-features