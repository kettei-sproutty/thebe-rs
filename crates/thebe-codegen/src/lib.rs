mod error;
mod generator;
mod template;

pub use error::CodegenError;
pub use generator::{generate_main, generate_route, RouteEntry};
