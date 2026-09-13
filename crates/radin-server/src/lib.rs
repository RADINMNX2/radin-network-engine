//! RADIN reference edge control server.
//!
//! Stateless, provider-neutral reference implementation of the edge control
//! API (spec 31): edge registry, health, benchmark, config, telemetry.
//! Real deployments run *their own* edge fleet in any provider.

pub mod routes;
pub mod state;

pub use routes::router;
pub use state::{reference_edges, EdgeHealthStatus, ServerState};
