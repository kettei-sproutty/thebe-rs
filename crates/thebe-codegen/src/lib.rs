mod error;
mod generator;
mod template;

pub use error::CodegenError;
pub use generator::{
	RouteEntry, default_app_html, generate_route, generate_routes_file, validate_app_html,
};
