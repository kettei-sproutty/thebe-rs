mod error;
mod generator;
mod template;

pub use error::CodegenError;
pub use generator::{RouteEntry, generate_main, generate_route};
