module.exports = grammar({
  name: "thebe",

  extras: ($) => [/\s+/],

  rules: {
    source_file: ($) =>
      repeat(
        choice(
          $.head_open,
          $.head_close,
          $.script_setup_open,
          $.script_open,
          $.script_ts_open,
          $.script_close,
          $.style_open,
          $.style_close,
          $.template_binding,
          $.tag,
          $.text,
        ),
      ),

    head_open: () => "<head>",
    head_close: () => "</head>",
    script_setup_open: () => "<script setup>",
    script_open: () => "<script>",
    script_ts_open: () => choice('<script lang="ts">', "<script lang='ts'>"),
    script_close: () => "</script>",
    style_open: () => "<style>",
    style_close: () => "</style>",

    template_binding: ($) => seq("{{", field("path", $.binding_path), "}}"),

    binding_path: ($) => seq($.identifier, repeat(seq(".", $.identifier))),

    identifier: () => /[A-Za-z_][A-Za-z0-9_]*/,
    tag: () => token(/<\/?[A-Za-z][^>]*>/),
    text: () => token(/[^<{]+/),
  },
});
