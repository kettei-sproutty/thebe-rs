(template_binding) @string.special

(comment) @comment

(binding_path
  (identifier) @variable)

((html_tag_name) @keyword
  (#match? @keyword "^(head|script|style)$"))

((html_tag_name) @tag
  (#not-match? @tag "^(head|script|style)$"))

(component_tag_name) @tag

((attribute_name) @keyword.control.directive
  (#match? @keyword.control.directive "^:"))

((attribute_name) @function
  (#match? @function "^on[A-Za-z]+$"))

((attribute_name) @attribute
  (#not-match? @attribute "^(?::|on[A-Za-z]+$)"))

[
  (double_quoted_attribute_value)
  (single_quoted_attribute_value)
  (unquoted_attribute_value)
] @string
