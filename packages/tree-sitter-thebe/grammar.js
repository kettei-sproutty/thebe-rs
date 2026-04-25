module.exports = grammar({
  name: "thebe",

  extras: ($) => [/\s+/],

  rules: {
    source_file: ($) =>
      repeat(
        choice(
          $.script_setup_element,
          $.script_ts_element,
          $.script_element,
          $.style_element,
          $.template_binding,
          $.start_tag,
          $.end_tag,
          $.self_closing_tag,
          $.text,
        ),
      ),

    template_binding: ($) => seq("{{", field("path", $.binding_path), "}}"),

    binding_path: ($) => seq($.identifier, repeat(seq(".", $.identifier))),

    identifier: () => /[A-Za-z_][A-Za-z0-9_]*/,

    script_setup_element: ($) =>
      prec(
        1,
        seq($.script_setup_start_tag, optional($.raw_text), $.script_end_tag),
      ),

    script_ts_element: ($) =>
      prec(
        1,
        seq($.script_ts_start_tag, optional($.raw_text), $.script_end_tag),
      ),

    script_element: ($) =>
      prec(
        1,
        seq($.script_start_tag, optional($.raw_text), $.script_end_tag),
      ),

    script_setup_start_tag: ($) =>
      seq(
        "<",
        field("name", alias("script", $.html_tag_name)),
        field("attribute", $.setup_attribute),
        ">",
      ),

    script_ts_start_tag: ($) =>
      seq(
        "<",
        field("name", alias("script", $.html_tag_name)),
        field("attribute", $.lang_ts_attribute),
        ">",
      ),

    script_start_tag: ($) => seq("<", field("name", alias("script", $.html_tag_name)), ">"),

    script_end_tag: ($) => seq("</", field("name", alias("script", $.html_tag_name)), ">"),

    style_element: ($) =>
      prec(
        1,
        seq($.style_start_tag, optional($.raw_text), $.style_end_tag),
      ),

    style_start_tag: ($) => seq("<", field("name", alias("style", $.html_tag_name)), ">"),

    style_end_tag: ($) => seq("</", field("name", alias("style", $.html_tag_name)), ">"),

    start_tag: ($) =>
      seq(
        "<",
        field("name", choice($.component_tag_name, $.html_tag_name)),
        repeat(field("attribute", $.attribute)),
        ">",
      ),

    end_tag: ($) => seq("</", field("name", choice($.component_tag_name, $.html_tag_name)), ">"),

    self_closing_tag: ($) =>
      seq(
        "<",
        field("name", choice($.component_tag_name, $.html_tag_name)),
        repeat(field("attribute", $.attribute)),
        "/>",
      ),

    component_tag_name: () => /[A-Z][A-Za-z0-9_]*/,
    html_tag_name: () => /[a-z][A-Za-z0-9-]*/,

    attribute: ($) =>
      seq(
        field("name", $.attribute_name),
        optional(
          seq(
            "=",
            field(
              "value",
              choice(
                $.double_quoted_attribute_value,
                $.single_quoted_attribute_value,
                $.unquoted_attribute_value,
              ),
            ),
          ),
        ),
      ),

    attribute_name: () => /:?[A-Za-z_][A-Za-z0-9_:-]*/,

    setup_attribute: ($) => seq(field("name", alias("setup", $.attribute_name))),

    lang_ts_attribute: ($) =>
      seq(
        field("name", alias("lang", $.attribute_name)),
        "=",
        field(
          "value",
          choice(
            alias('"ts"', $.double_quoted_attribute_value),
            alias("'ts'", $.single_quoted_attribute_value),
          ),
        ),
      ),

    double_quoted_attribute_value: () => token(seq('"', repeat(/[^"\\]/), '"')),
    single_quoted_attribute_value: () => token(seq("'", repeat(/[^'\\]/), "'")),
    unquoted_attribute_value: () => /[^\s/>"'=]+/,

    raw_text: ($) => repeat1($.block_text),

    block_text: () => token(choice(/[^<]+/, "<")),

    text: () => token(choice(/[^<{]+/, "{")),
  },
});
