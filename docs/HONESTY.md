# HONESTY — The RADIN contract

> "Always true blue." No number leaves this project that was not actually
> measured. Synthetic data exists only to *train and test*; it can never be
> a production report.

## 1. Sources

`MeasuredSource::{Real, Synthetic}` is the single marker every measurement
carries (`radin-core/src/metrics.rs`). A value is `Real` if and only if it was
observed on a real wire by the data plane; everything else is `Synthetic`.

## 2. Enforced boundaries (the checks are code, not vibes)

| Boundary                        | Rejection point                              | Error                          |
|---------------------------------|----------------------------------------------|--------------------------------|
| Production metrics buffer       | `TelemetryBuffer::push` → `ensure_real`      | `Error::InvalidInput`          |
| Route scoring                   | `scoring::score_all` synthetic-flag check    | `Error::InvalidInput`          |
| Diagnostic report (production)  | `DiagnosticBuilder::build` → `ensure_real`   | `Error::InvalidInput`          |
| Transport selection             | `transport::select_transport`                | synthetic refused              |

The `benchmark-synthetic` feature is never wired to a production sink; the
chaos crate (`radin-chaos`) is **test-only** and its outputs are dead-ended in
tests and the CLI demo — the demo stamps every line as synthetic.

## 3. What the engine may NOT do

- Never report a route/edge as healthy from anything but measured samples.
- Never guess a score, a grade, an RTT, a loss ratio, or a jitter from memory.
- Never export a `DiagnosticReport` whose `source` is `Synthetic`.
- Never present better numbers than it measured (spec 41) — drift or expiry
  forces re-measure, not interpolation.

## 4. Statistical honesty

- Loss is a ratio of real failures to real probes (`failures / total`), so it
  remembers history and approaches reality slowly — this is deliberate and
  conservative (an edge that just suffered a loss storm does not bounce back
  instantly).
- The CLI/chaos demos label measurement source in their output header so a
  harness number is never mistaken for production telemetry.

## 5. The report itself

`DiagnosticReport::render_plaintext` prints exactly the fields the engine can
account for, and always ends with:

    (No packet payloads, credentials, or user data included.)

Payloads never enter the report, logs, or telemetry — ever. Spec 41 audit
procedure: grep the tree for `MeasuredSource::Synthetic` usages; each must sit
behind `#[cfg(test)]`, the `benchmark-synthetic` feature, `radin-chaos`, or the
CLI demo with its visible synthetic stamp.