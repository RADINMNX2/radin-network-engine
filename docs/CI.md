# CI — radin-network-engine

Every push to `main` and every pull request runs the workflow in
`.github/workflows/ci.yml` on `ubuntu-latest` (no OS matrix).

## What runs

| Job | Toolchain | Command | Purpose |
|-----|-----------|---------|---------|
| `check` | stable | `cargo fmt --check` | Formatting gate |
| `check` | stable | `cargo clippy --workspace --all-features --all-targets --locked -- -D warnings` | Lint gate: zero warnings allowed |
| `check` | stable | `cargo test --workspace --all-features --locked` | Full test suite (144 tests at baseline) |
| `msrv` | 1.85.0 | `cargo +1.85.0 build -p radin-core --locked` | Minimum-supported-Rust guard |

## MSRV strategy

The workspace declares `rust-version = 1.85`. The `msrv` job builds
**radin-core only, with default features**, because:

- core is the platform-independent decision engine whose MSRV claim must
  actually hold;
- the optional heavy features (`sqlite` via bundled rusqlite, `zstd` via
  zstd-sys) require native C builds; their full coverage runs on stable where
  that is guaranteed. Forcing them on the MSRV toolchain risks
  runner-environment noise instead of a compatibility signal;
- `cargo +1.85.0` explicitly overrides the `rust-toolchain.toml` pin
  (rustup always honors an explicit `+toolchain` argument).

## Scaffold exclusion

`android/`, `app/` and `proto/` are build-required scaffolds (see
`docs/SPEC_TRACEABILITY.md`): they need Android SDK/NDK, Flutter SDK, and
`protoc` respectively — none of which exist on CI runners. They are not Cargo
workspace members, so no CI step compiles them.

## Run the CI gates locally (exact commands)

    cargo fmt --check
    cargo clippy --workspace --all-features --all-targets --locked -- -D warnings
    cargo test --workspace --all-features --locked
    cargo +1.85.0 build -p radin-core --locked   # only if 1.85 is installed

## Honesty

Job and step names report exactly the commands they run; no step suppresses
errors (`|| true` and `continue-on-error` are absent). A green run means the
four commands above passed on the runner — nothing more, nothing less.