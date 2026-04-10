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

/// Parse a `.trs` file into its constituent blocks.
///
/// Uses an HTML5-aware approach: `<script>` and `<style>` are treated as
/// raw-text elements (their content is scanned until the matching close tag,
/// not re-parsed as HTML).
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

        match try_extract_block(from_lt)? {
            Some((kind, content, after)) => {
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
            }
            None => {
                // Not a recognised block — include `<` in template and advance.
                template_parts.push(&remaining[..lt_pos + 1]);
                remaining = &remaining[lt_pos + 1..];
            }
        }
    }

    blocks.template = template_parts.concat().trim().to_owned();
    Ok(blocks)
}

/// Attempt to consume one of the three recognised blocks starting at `input`
/// (which must begin with `<`).
///
/// Returns `Some((kind, inner_content, rest_of_input))` on success, or `None`
/// if the current position is not the start of a recognised block.
fn try_extract_block<'a>(
    input: &'a str,
) -> Result<Option<(BlockKind, &'a str, &'a str)>, ParseError> {
    // Identify the block kind by matching the opening tag prefix.
    // All comparisons are on ASCII characters, so a byte-level lower-case
    // check is sufficient and avoids allocating a lowercase copy of the full
    // input.
    let kind = if ascii_starts_with_ignore_case(input, "<script") {
        // Need to inspect the attributes to distinguish the two script kinds.
        classify_script_tag(input)
    } else if ascii_starts_with_ignore_case(input, "<style") {
        // Make sure the next char after "<style" is `>` or whitespace, not
        // an unrelated tag name like `<stylesheet>`.
        let after_keyword = &input["<style".len()..];
        if after_keyword
            .starts_with(|c: char| c == '>' || c.is_ascii_whitespace())
        {
            Some(BlockKind::Style)
        } else {
            None
        }
    } else {
        None
    };

    let Some(kind) = kind else {
        return Ok(None);
    };

    // Find the `>` that closes the opening tag.
    let Some(open_end) = input.find('>') else {
        return Err(ParseError::UnclosedBlock(kind.label().to_owned()));
    };

    let content_start = open_end + 1;
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

/// Determine whether the `<script ...>` opening tag at `input` is a setup
/// block, a TypeScript block, or something else (in which case `None` is
/// returned so the tag passes through to the template).
fn classify_script_tag(input: &str) -> Option<BlockKind> {
    // Collect everything up to the first `>` as the attribute string.
    let gt_pos = input.find('>')?;
    let attrs_region = &input[..gt_pos];

    // Check for the `setup` attribute (bare or with value).
    if attrs_region.contains("setup") {
        return Some(BlockKind::ScriptSetup);
    }
    // Check for `lang="ts"` or `lang='ts'`.
    if attrs_region.contains(r#"lang="ts""#) || attrs_region.contains("lang='ts'") {
        return Some(BlockKind::ScriptTs);
    }
    // Plain `<script>` — not a Thebe block we extract.
    None
}

/// Returns `true` if `haystack` starts with `needle`, ignoring ASCII case.
fn ascii_starts_with_ignore_case(haystack: &str, needle: &str) -> bool {
    haystack.len() >= needle.len()
        && haystack[..needle.len()].eq_ignore_ascii_case(needle)
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
}
