//! Foreseer Desktop library: protocol v1, config, auth, and controller.

pub mod auth;
pub mod config;
pub mod controller;
pub mod extension;
pub mod protocol;
pub mod session;
pub mod setup;
pub mod supervisor;

pub use config::{
    AppConfig, AppMode, ForeseerUrlError, validate_bootstrap_server_url, validate_foreseer_url,
};
pub use controller::{AppState, Controller, ControllerEvent, RuntimeOps};
pub use protocol::{
    NativeCommandV1, NativeEventV1, PROTOCOL_VERSION, parse_command, serialize_event,
};
