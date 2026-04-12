mod error;
mod generator;
mod template;

pub use error::CodegenError;
pub use generator::{
  RouteEntry, RouteHandlerInfo, default_app_html, generate_route, generate_routes_file,
  route_handler_info, route_state_type, route_template_symbols, validate_app_html,
};
pub use template::{
  TemplateBindingOccurrence, list_template_binding_occurrences, list_template_bindings,
};
