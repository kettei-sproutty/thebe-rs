mod error;
mod generator;
mod template;

pub use error::CodegenError;
pub use generator::{
  ComponentMacro, RouteEntry, RouteHandlerInfo, TemplateSymbolDefinition, default_app_html,
  generate_component, generate_route, generate_routes_file, props_symbol_definitions,
  route_handler_info, route_state_type, route_template_symbol_definitions, route_template_symbols,
  validate_app_html,
};
pub use template::{
  TemplateBindingOccurrence, inject_attr_bindings, list_template_binding_occurrences,
  list_template_bindings,
};
