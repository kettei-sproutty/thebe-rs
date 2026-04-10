use crate::error::ParseError;

/// The four blocks extracted from a `.trs` Single File Component.
#[derive(Debug, Default)]
pub struct SfcBlocks {
    /// Content of `<script setup>` — server route module (Rust).
    pub script_setup: Option<String>,
    /// Content of `<script lang="ts">` — browser-side TypeScript.
    pub script_ts: Option<String>,
    /// Content of `<style>` — scoped CSS.
    pub style: Option<String>,
    /// Everything else — the HTML template.
    pub template: String,
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum BlockKind {
    ScriptSetup,
    ScriptTs,
    Style,
}

impl BlockKind {
    fn label(self) -> &'static str {
        match self {
            Self::ScriptSetup => "script setup",
            Self::ScriptTs => r#"script lang="ts""#,
            Self::Style => "style",
        }
    }

    fn close_tag(self) -> &'static str {
        match self {
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
        // Find the next `<` — potential start of a block we care about.
        let Some(lt_pos) = remaining.find('<') else {
            template_parts.push(remaining);
            break;
        };

        let before = &remaining[..lt_pos];
        let from_lt = &remaining[lt_pos..];

        if let Some((kind, content, after)) = try_extract_block(from_lt)? {
            template_parts.push(before);
            match kind {
                BlockKind::ScriptSetup => {
                    if blocks.script_setup.is_some() {
                        return Err(ParseError::DuplicateBlock(kind.label().to_owned()));
                    }
                    blocks.script_setup = Some(trim_newlines(content));
                }
                BlockKind::ScriptTs => {
                    if blocks.script_ts.is_some() {
                        return Err(ParseError::DuplicateBlock(kind.label().to_owned()));
                    }
                    blocks.script_ts = Some(trim_newlines(content));
                }
                BlockKind::Style => {
                    if blocks.style.is_some() {
                        return Err(ParseError::DuplicateBlock(kind.label().to_owned()));
                    }
                    blocks.style = Some(trim_newlines(content));
                }
            }
            remaining = after;
        } else {
            // Not a recognised block — include `<` in template and advance.
            template_parts.push(&remaining[..=lt_pos]);
            remaining = &remaining[lt_pos + 1..];
        }
    }

    template_parts.concat().trim().clone_into(&mut blocks.template);
    Ok(blocks)
}

/// Attempt to consume one of the three recognised blocks starting at `input`
/// (which must begin with `<`).
///
/// Returns `Some((kind, inner_content, rest_of_input))` on success, or `None`
/// if the current position is not the start of a recognised block.
fn try_extract_block(
    input: &str,
) -> Result<Option<(BlockKind, &str, &str)>, ParseError> {
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
        return Ok(Some((kind, content, after)));
    };

    let content = &content_and_rest[..close_pos];
    let after = &content_and_rest[close_pos + close_tag.len()..];
    Ok(Some((kind, content, after)))
}

fn classify_opening_tag(tag: &OpeningTag<'_>) -> Option<BlockKind> {
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
            && attr.value.is_some_and(|value| value.eq_ignore_ascii_case("ts"))
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
                while idx < bytes.len()
                    && !bytes[idx].is_ascii_whitespace()
                    && bytes[idx] != b'>'
                {
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

fn trim_newlines(s: &str) -> String {
    s.trim_matches('\n').trim_matches('\r').trim().to_owned()
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

<h1>{{ title }}</h1>

<style>
h1 { color: red; }
</style>"#;

        let blocks = parse_sfc(input).unwrap();
        assert!(blocks.script_setup.is_some());
        assert!(blocks.script_ts.is_some());
        assert!(blocks.style.is_some());
        assert!(blocks.template.contains("{{ title }}"));
    }

    #[test]
    fn parse_sfc_template_only() {
        let input = "<h1>Hello</h1>";
        let blocks = parse_sfc(input).unwrap();
        assert_eq!(blocks.template, "<h1>Hello</h1>");
        assert!(blocks.script_setup.is_none());
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
        assert!(blocks.template.contains("<script data-kind=\"setup\" data-lang=\"lang=ts\">"));
    }
}
