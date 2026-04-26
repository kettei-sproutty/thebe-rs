mod error;
mod generator;
mod template;

pub use error::CodegenError;
pub use generator::{
  ComponentMacro, DevRouteArtifact, RouteEntry, RouteHandlerInfo, TemplateSymbolDefinition,
  default_app_html, dev_route_artifact_path, generate_component, generate_route,
  generate_routes_file, inline_rust_view_source, props_symbol_definitions,
  props_typescript_source,
  route_handler_info, route_state_type, route_template_symbol_definitions, route_template_symbols,
  validate_app_html,
};
pub use template::{
  TemplateBindingOccurrence, inject_attr_bindings, list_template_binding_occurrences,
  list_template_bindings, list_used_component_names,
};
