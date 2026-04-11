use crate::error::ParseError;

/// A byte range within a source file.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SourceSpan {
  /// Inclusive start byte offset.
  pub start: usize,
  /// Exclusive end byte offset.
  pub end: usize,
}

impl SourceSpan {
  #[must_use]
  pub fn offset(self, delta: usize) -> Self {
    Self {
      start: self.start + delta,
      end: self.end + delta,
    }
  }
}

/// The four blocks extracted from a `.trs` Single File Component.
#[derive(Debug, Default)]
pub struct SfcBlocks {
  /// Content of `<head>` — route/layout head contribution HTML.
  pub head: Option<String>,
  /// Source span of the trimmed `<head>` content.
  pub head_span: Option<SourceSpan>,
  /// Content of `<script setup>` — server route module (Rust).
  pub script_setup: Option<String>,
  /// Source span of the trimmed `<script setup>` content.
  pub script_setup_span: Option<SourceSpan>,
  /// Content of `<script lang="ts">` — browser-side TypeScript.
  pub script_ts: Option<String>,
  /// Source span of the trimmed `<script lang="ts">` content.
  pub script_ts_span: Option<SourceSpan>,
  /// Content of `<style>` — scoped CSS.
  pub style: Option<String>,
  /// Source span of the trimmed `<style>` content.
  pub style_span: Option<SourceSpan>,
  /// Everything else — the HTML template.
  pub template: String,
  /// Source spans of template segments in the original file.
  pub template_spans: Vec<SourceSpan>,
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum BlockKind {
  Head,
  ScriptSetup,
  ScriptTs,
  Style,
}

impl BlockKind {
  fn label(self) -> &'static str {
    match self {
      Self::Head => "head",
      Self::ScriptSetup => "script setup",
      Self::ScriptTs => r#"script lang="ts""#,
      Self::Style => "style",
    }
  }

  fn close_tag(self) -> &'static str {
    match self {
      Self::Head => "</head>",
      Self::ScriptSetup | Self::ScriptTs => "</script>",
      Self::Style => "</style>",
    }
  }
}

struct OpeningTag<'a> {
  name: &'a str,
  attrs: Vec<Attribute<'a>>,
  end: usize,
}

struct Attribute<'a> {
  name: &'a str,
  value: Option<&'a str>,
}

struct ExtractedBlock<'a> {
  kind: BlockKind,
  content: &'a str,
  after: &'a str,
  content_span: SourceSpan,
}

/// Parse a `.trs` file into its constituent blocks.
///
/// Uses an HTML5-aware approach: `<script>` and `<style>` are treated as
/// raw-text elements (their content is scanned until the matching close tag,
/// not re-parsed as HTML).
///
/// # Errors
///
/// Returns [`ParseError`] when a recognised block is missing its closing tag
/// or the same block kind appears more than once.
pub fn parse_sfc(input: &str) -> Result<SfcBlocks, ParseError> {
  let mut blocks = SfcBlocks::default();
  let mut template_parts: Vec<&str> = Vec::new();
  let mut remaining = input;

  while !remaining.is_empty() {
    let base_offset = input.len() - remaining.len();
    // Find the next `<` — potential start of a block we care about.
    let Some(lt_pos) = remaining.find('<') else {
      template_parts.push(remaining);
      blocks.template_spans.push(SourceSpan {
        start: base_offset,
        end: base_offset + remaining.len(),
      });
      break;
    };

    let before = &remaining[..lt_pos];
    let from_lt = &remaining[lt_pos..];

    if let Some(extracted) = try_extract_block(from_lt)? {
      if !before.is_empty() {
        template_parts.push(before);
        blocks.template_spans.push(SourceSpan {
          start: base_offset,
          end: base_offset + lt_pos,
        });
      }
      let content_span = extracted.content_span.offset(base_offset + lt_pos);
      match extracted.kind {
        BlockKind::Head => {
          if blocks.head.is_some() {
            return Err(ParseError::DuplicateBlock(
              extracted.kind.label().to_owned(),
            ));
          }
          let (content, span) = trim_block_content(extracted.content, content_span);
          blocks.head = Some(content);
          blocks.head_span = Some(span);
        }
        BlockKind::ScriptSetup => {
          if blocks.script_setup.is_some() {
            return Err(ParseError::DuplicateBlock(
              extracted.kind.label().to_owned(),
            ));
          }
          let (content, span) = trim_block_content(extracted.content, content_span);
          blocks.script_setup = Some(content);
          blocks.script_setup_span = Some(span);
        }
        BlockKind::ScriptTs => {
          if blocks.script_ts.is_some() {
            return Err(ParseError::DuplicateBlock(
              extracted.kind.label().to_owned(),
            ));
          }
          let (content, span) = trim_block_content(extracted.content, content_span);
          blocks.script_ts = Some(content);
          blocks.script_ts_span = Some(span);
        }
        BlockKind::Style => {
          if blocks.style.is_some() {
            return Err(ParseError::DuplicateBlock(
              extracted.kind.label().to_owned(),
            ));
          }
          let (content, span) = trim_block_content(extracted.content, content_span);
          blocks.style = Some(content);
          blocks.style_span = Some(span);
        }
      }
      remaining = extracted.after;
    } else {
      // Not a recognised block — include `<` in template and advance.
      template_parts.push(&remaining[..=lt_pos]);
      blocks.template_spans.push(SourceSpan {
        start: base_offset,
        end: base_offset + lt_pos + 1,
      });
      remaining = &remaining[lt_pos + 1..];
    }
  }

  template_parts
    .concat()
    .trim()
    .clone_into(&mut blocks.template);
  Ok(blocks)
}

/// Attempt to consume one of the three recognised blocks starting at `input`
/// (which must begin with `<`).
///
/// Returns `Some((kind, inner_content, rest_of_input))` on success, or `None`
/// if the current position is not the start of a recognised block.
fn try_extract_block(input: &str) -> Result<Option<ExtractedBlock<'_>>, ParseError> {
  let Some(tag) = parse_opening_tag(input) else {
    return Ok(None);
  };

  let kind = classify_opening_tag(&tag);

  let Some(kind) = kind else {
    return Ok(None);
  };

  let content_start = tag.end + 1;
  let content_and_rest = &input[content_start..];
  let close_tag = kind.close_tag();

  // `<script>` and `<style>` are raw-text elements: search literally for
  // the close tag (case-sensitive lowercase, per our authoring convention).
  let Some(close_pos) = content_and_rest.find(close_tag) else {
    // Also try the uppercase variant as a courtesy.
    let close_tag_upper = close_tag.to_uppercase();
    let Some(close_pos) = content_and_rest.find(close_tag_upper.as_str()) else {
      return Err(ParseError::UnclosedBlock(kind.label().to_owned()));
    };
    let content = &content_and_rest[..close_pos];
    let after = &content_and_rest[close_pos + close_tag_upper.len()..];
    return Ok(Some(ExtractedBlock {
      kind,
      content,
      after,
      content_span: SourceSpan {
        start: content_start,
        end: content_start + close_pos,
      },
    }));
  };

  let content = &content_and_rest[..close_pos];
  let after = &content_and_rest[close_pos + close_tag.len()..];
  Ok(Some(ExtractedBlock {
    kind,
    content,
    after,
    content_span: SourceSpan {
      start: content_start,
      end: content_start + close_pos,
    },
  }))
}

fn classify_opening_tag(tag: &OpeningTag<'_>) -> Option<BlockKind> {
  if tag.name.eq_ignore_ascii_case("head") {
    return Some(BlockKind::Head);
  }
  if tag.name.eq_ignore_ascii_case("style") {
    return Some(BlockKind::Style);
  }
  if !tag.name.eq_ignore_ascii_case("script") {
    return None;
  }

  for attr in &tag.attrs {
    if attr.name.eq_ignore_ascii_case("setup") {
      return Some(BlockKind::ScriptSetup);
    }
    if attr.name.eq_ignore_ascii_case("lang")
      && attr
        .value
        .is_some_and(|value| value.eq_ignore_ascii_case("ts"))
    {
      return Some(BlockKind::ScriptTs);
    }
  }

  None
}

fn parse_opening_tag(input: &str) -> Option<OpeningTag<'_>> {
  let bytes = input.as_bytes();
  if bytes.first().copied()? != b'<' {
    return None;
  }
  if bytes.get(1).is_some_and(|byte| *byte == b'/') {
    return None;
  }

  let mut idx = 1usize;
  while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
    idx += 1;
  }

  let name_start = idx;
  while idx < bytes.len() && is_tag_name_byte(bytes[idx]) {
    idx += 1;
  }
  if idx == name_start {
    return None;
  }
  let name_end = idx;

  let mut attrs = Vec::new();
  loop {
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
      idx += 1;
    }
    if idx >= bytes.len() {
      return None;
    }
    match bytes[idx] {
      b'>' => {
        return Some(OpeningTag {
          name: &input[name_start..name_end],
          attrs,
          end: idx,
        });
      }
      b'/' if bytes.get(idx + 1).is_some_and(|byte| *byte == b'>') => {
        return Some(OpeningTag {
          name: &input[name_start..name_end],
          attrs,
          end: idx + 1,
        });
      }
      _ => {}
    }

    let attr_start = idx;
    while idx < bytes.len()
      && !bytes[idx].is_ascii_whitespace()
      && bytes[idx] != b'='
      && bytes[idx] != b'>'
      && bytes[idx] != b'/'
    {
      idx += 1;
    }
    if idx == attr_start {
      return None;
    }

    let name = &input[attr_start..idx];
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
      idx += 1;
    }

    let value = if bytes.get(idx).is_some_and(|byte| *byte == b'=') {
      idx += 1;
      while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
      }
      if idx >= bytes.len() {
        return None;
      }

      if matches!(bytes[idx], b'"' | b'\'') {
        let quote = bytes[idx];
        idx += 1;
        let value_start = idx;
        while idx < bytes.len() && bytes[idx] != quote {
          idx += 1;
        }
        if idx >= bytes.len() {
          return None;
        }
        let value = &input[value_start..idx];
        idx += 1;
        Some(value)
      } else {
        let value_start = idx;
        while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() && bytes[idx] != b'>' {
          idx += 1;
        }
        Some(&input[value_start..idx])
      }
    } else {
      None
    };

    attrs.push(Attribute { name, value });
  }
}

fn is_tag_name_byte(byte: u8) -> bool {
  byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-')
}

fn trim_block_content(s: &str, span: SourceSpan) -> (String, SourceSpan) {
  let trimmed = s.trim().to_owned();
  let leading_trim = s.len() - s.trim_start().len();
  if trimmed.is_empty() {
    let offset = span.start + leading_trim;
    return (
      trimmed,
      SourceSpan {
        start: offset,
        end: offset,
      },
    );
  }

  let trailing_trim = s.len() - s.trim_end().len();
  (
    trimmed,
    SourceSpan {
      start: span.start + leading_trim,
      end: span.end - trailing_trim,
    },
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_sfc_extracts_all_blocks() {
    let input = r#"<script setup>
struct Props { title: String }
#[thebe::get]
pub fn handler() -> Props { Props { title: "hi".into() } }
</script>

<script lang="ts">
let x = 1;
</script>

<head>
<title>Thebe</title>
</head>

<h1>{{ title }}</h1>

<style>
h1 { color: red; }
</style>"#;

    let blocks = parse_sfc(input).unwrap();
    assert!(blocks.head.is_some());
    assert!(blocks.script_setup.is_some());
    assert!(blocks.script_ts.is_some());
    assert!(blocks.style.is_some());
    assert!(blocks.template.contains("{{ title }}"));
    assert_eq!(
      &input[blocks.script_setup_span.unwrap().start..blocks.script_setup_span.unwrap().end],
      blocks.script_setup.as_deref().unwrap()
    );
  }

  #[test]
  fn parse_sfc_template_only() {
    let input = "<h1>Hello</h1>";
    let blocks = parse_sfc(input).unwrap();
    assert_eq!(blocks.template, "<h1>Hello</h1>");
    assert!(blocks.script_setup.is_none());
    let reconstructed = blocks
      .template_spans
      .iter()
      .map(|span| &input[span.start..span.end])
      .collect::<String>();
    assert_eq!(reconstructed, input);
  }

  #[test]
  fn parse_sfc_returns_error_on_unclosed_block() {
    let input = "<script setup>fn handler() {}";
    assert!(parse_sfc(input).is_err());
  }

  #[test]
  fn parse_sfc_returns_error_on_duplicate_block() {
    let input = "<script setup>fn a() {}</script>\n<script setup>fn b() {}</script>";
    assert!(parse_sfc(input).is_err());
  }

  #[test]
  fn parse_sfc_extracts_head_block() {
    let input = "<head><title>Example</title></head>\n<h1>Hello</h1>";
    let blocks = parse_sfc(input).unwrap();

    assert_eq!(blocks.head.as_deref(), Some("<title>Example</title>"));
    assert_eq!(blocks.template, "<h1>Hello</h1>");
  }

  #[test]
  fn parse_sfc_handles_gt_inside_attribute_values() {
    let input = r#"<script setup data-note="> still inside attr">
pub fn handler() {}
</script>
<h1>Hello</h1>"#;
    let blocks = parse_sfc(input).unwrap();
    assert!(blocks.script_setup.is_some());
    assert_eq!(blocks.template, "<h1>Hello</h1>");
  }

  #[test]
  fn parse_sfc_does_not_misclassify_script_from_attribute_values() {
    let input = r#"<script data-kind="setup" data-lang="lang=ts">alert('hi');</script>
<p>Hello</p>"#;
    let blocks = parse_sfc(input).unwrap();
    assert!(blocks.script_setup.is_none());
    assert!(blocks.script_ts.is_none());
    assert!(
      blocks
        .template
        .contains("<script data-kind=\"setup\" data-lang=\"lang=ts\">")
    );
  }

  #[test]
  fn parse_sfc_tracks_template_segment_source_spans() {
    let input =
      "<script setup>pub fn handler() {}</script>\n<div>{{ title }}</div>\n<style>.x {}</style>";
    let blocks = parse_sfc(input).unwrap();
    let template_source = blocks
      .template_spans
      .iter()
      .map(|span| &input[span.start..span.end])
      .collect::<String>();

    assert!(template_source.contains("{{ title }}"));
    assert_eq!(blocks.template, "<div>{{ title }}</div>");
  }
}
