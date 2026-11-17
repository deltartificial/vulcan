//! Text rules that rustfmt, dprint and clippy do not cover.
//!
//! - **Line width**: at most [`LINE_WIDTH_MAX`] columns in every tracked text file, with no
//!   exception. rustfmt leaves long strings and comments alone, and Markdown has no limit.
//! - **Comments are sentences**: a space after the comment marker, a capital (or code) first, a
//!   full stop, a colon, a question or an exclamation mark last. This covers `//` comments, doc
//!   comments (paragraph by paragraph, list item by list item, code blocks and headings aside) and
//!   the `#` comments of TOML and YAML files. A comment at the end of a code line may be a phrase.

use crate::report::Violation;

/// The hard limit on line width, in characters.
pub(crate) const LINE_WIDTH_MAX: u32 = 100;

/// How a file marks its comments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Syntax {
    /// `//` comments and `///` or `//!` doc comments.
    Rust,
    /// `#` comments, as in TOML and YAML.
    Hash,
    /// No comments to check, as in Markdown and JSON.
    Plain,
}

impl Syntax {
    /// Returns the syntax of a file with the extension `extension`, or nothing if the file is not
    /// a checked text file.
    pub(crate) fn from_extension(extension: &str) -> Option<Self> {
        let syntax = match extension {
            "rs" => Self::Rust,
            "toml" | "yml" | "yaml" => Self::Hash,
            "md" | "json" => Self::Plain,
            _ => return None,
        };
        assert!(!extension.is_empty(), "a known extension has a name");
        if syntax == Self::Rust {
            assert!(extension == "rs", "only Rust files have Rust comments");
        }
        Some(syntax)
    }
}

/// One text file: where it is and what it holds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TextFile<'text> {
    /// The path, relative to the repository root.
    pub(crate) path: &'text str,
    /// The whole content.
    pub(crate) text: &'text str,
}

/// Returns the violations of every rule of this module in one file.
pub(crate) fn check_text(file: &TextFile<'_>, syntax: Syntax) -> Vec<Violation> {
    let TextFile { path, text } = *file;
    let mut violations = check_text_line_widths(file);
    let blocks = match syntax {
        Syntax::Rust => {
            let mut blocks = check_text_blocks(text, CommentKind::Line);
            blocks.extend(check_text_blocks(text, CommentKind::Doc));
            blocks
        }
        Syntax::Hash => check_text_blocks(text, CommentKind::Hash),
        Syntax::Plain => Vec::new(),
    };
    assert!(blocks.len() <= text.lines().count(), "a block takes at least one line");
    for block in &blocks {
        violations.extend(check_text_block(path, block));
    }
    assert!(violations.iter().all(|violation| violation.path == path), "one file at a time");
    violations
}

/// Reports every line wider than [`LINE_WIDTH_MAX`] characters.
fn check_text_line_widths(file: &TextFile<'_>) -> Vec<Violation> {
    let TextFile { path, text } = *file;
    let violations: Vec<Violation> = text
        .lines()
        .enumerate()
        .filter_map(|(line_index, line)| {
            let width = line.chars().count();
            let too_wide = width > usize::try_from(LINE_WIDTH_MAX).map_or(usize::MAX, |max| max);
            too_wide.then(|| {
                let message =
                    format!("line is {width} columns wide, the limit is {LINE_WIDTH_MAX}");
                Violation::new(path, line_index, message)
            })
        })
        .collect();
    assert!(violations.len() <= text.lines().count(), "one violation per line at most");
    if let Some(last) = violations.last() {
        assert!(last.line_number > 0, "a reported line exists");
    }
    violations
}

/// The comment markers a block is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommentKind {
    /// `//`, not followed by `/` or `!`.
    Line,
    /// `///` or `//!`.
    Doc,
    /// `#`, not followed by `!` or `[`.
    Hash,
}

/// One sentence of a comment block: the offset of its first line in the block, and its lines.
type Sentence<'text> = (usize, Vec<&'text str>);

/// A run of consecutive comment lines of one kind at one indentation.
#[derive(Debug)]
struct CommentBlock<'text> {
    /// The kind of every line of the block.
    kind: CommentKind,
    /// The 0-based line index of the first line.
    line_index_first: usize,
    /// The text after the comment marker, one entry per line.
    lines: Vec<&'text str>,
}

/// Returns the text after the comment marker of `line`, if `line` is a comment of `kind`.
fn check_text_blocks_comment(line: &str, kind: CommentKind) -> Option<&str> {
    assert!(!line.contains('\n'), "one line at a time");
    let trimmed = line.trim_start();
    let text = match kind {
        CommentKind::Line => {
            let text = trimmed.strip_prefix("//")?;
            let doc = text.starts_with(['/', '!']);
            if doc { None } else { Some(text) }
        }
        CommentKind::Doc => trimmed.strip_prefix("///").or_else(|| trimmed.strip_prefix("//!")),
        CommentKind::Hash => {
            let text = trimmed.strip_prefix('#')?;
            let directive = text.starts_with(['!', '[']);
            if directive { None } else { Some(text) }
        }
    };
    if let Some(comment) = text {
        assert!(comment.len() < line.len(), "the marker is cut off");
    }
    text
}

/// Returns the comment blocks of `kind` in `text`.
fn check_text_blocks(text: &str, kind: CommentKind) -> Vec<CommentBlock<'_>> {
    let mut blocks: Vec<CommentBlock<'_>> = Vec::new();
    let mut indentation_previous = None;
    let mut in_multiline_string = false;
    for (line_index, line) in text.lines().enumerate() {
        // A `#` inside a TOML multi-line string is text, not a comment.
        let delimiters = line.matches("\"\"\"").count() + line.matches("\'\'\'").count();
        let inside = in_multiline_string;
        if delimiters.is_multiple_of(2) {
            // Balanced on this line: the state carries over unchanged.
        } else {
            in_multiline_string = !in_multiline_string;
        }
        let found = if inside { None } else { check_text_blocks_comment(line, kind) };
        let Some(comment) = found else {
            indentation_previous = None;
            continue;
        };
        let indentation = line.len() - line.trim_start().len();
        let continues = indentation_previous == Some(indentation);
        match blocks.last_mut() {
            Some(block) if continues => block.lines.push(comment),
            Some(_) | None => {
                blocks.push(CommentBlock {
                    kind,
                    line_index_first: line_index,
                    lines: vec![comment],
                });
            }
        }
        indentation_previous = Some(indentation);
    }
    assert!(blocks.iter().all(|block| !block.lines.is_empty()), "a block has a line");
    assert!(blocks.iter().all(|block| block.kind == kind), "a block has one kind");
    blocks
}

/// Reports a missing space after the marker, and paragraphs that do not read as sentences.
fn check_text_block(path: &str, block: &CommentBlock<'_>) -> Vec<Violation> {
    assert!(!block.lines.is_empty(), "a block has a line");
    let mut violations = Vec::new();
    for (offset, line) in block.lines.iter().enumerate() {
        let cramped = line.chars().next().is_some_and(|first| first != ' ');
        if cramped {
            let message = "put a space after the comment marker".to_owned();
            violations.push(Violation::new(path, block.line_index_first + offset, message));
        }
    }
    for (offset, sentence) in check_text_block_sentences(block) {
        if let Some(message) = check_text_block_sentence_problem(&sentence) {
            violations.push(Violation::new(
                path,
                block.line_index_first + offset,
                message.to_owned(),
            ));
        }
    }
    assert!(violations.len() <= block.lines.len() * 2, "two problems per line at most");
    violations
}

/// Returns the sentences of a block, each with the offset of its first line: the whole block for
/// plain comments, and for doc comments each paragraph and list item outside code blocks,
/// headings and tables.
fn check_text_block_sentences<'text>(block: &CommentBlock<'text>) -> Vec<Sentence<'text>> {
    let mut sentences: Vec<Sentence<'text>> = Vec::new();
    let mut in_code = false;
    let mut open = false;
    for (offset, line) in block.lines.iter().enumerate() {
        let text = line.trim();
        let fence = text.starts_with("```");
        let separator = text.chars().all(|character| !character.is_alphanumeric());
        let structural = if text.starts_with(['#', '|']) { true } else { text.starts_with("[`") };
        let structure = if in_code { true } else { if fence { true } else { structural } };
        let skipped = match block.kind {
            CommentKind::Doc => structure,
            CommentKind::Line | CommentKind::Hash => false,
        };
        if fence {
            in_code = !in_code;
        }
        let outside = if skipped { true } else { separator };
        if outside {
            open = false;
        } else {
            let item = ["- ", "* "].iter().any(|marker| text.starts_with(marker));
            let starts = match block.kind {
                CommentKind::Doc => item,
                CommentKind::Line | CommentKind::Hash => false,
            };
            let continues = if open { !starts } else { false };
            match sentences.last_mut() {
                Some((_, lines)) if continues => lines.push(text),
                Some(_) | None => sentences.push((offset, vec![text])),
            }
            open = true;
        }
    }
    assert!(sentences.len() <= block.lines.len(), "a sentence takes at least one line");
    let ordered =
        sentences.iter().zip(sentences.iter().skip(1)).all(|(first, second)| first.0 < second.0);
    assert!(ordered, "sentences come in the order of their lines");
    sentences
}

/// Returns what keeps `lines` from reading as a sentence.
fn check_text_block_sentence_problem(lines: &[&str]) -> Option<&'static str> {
    assert!(!lines.is_empty(), "a sentence has a line");
    let first = lines.first()?;
    let last = lines.last()?;
    let opening_text =
        first.strip_prefix("- ").or_else(|| first.strip_prefix("* ")).map_or(*first, |item| item);
    let opening = opening_text.chars().next()?;
    let opens_well = if opening.is_uppercase() {
        true
    } else {
        if opening.is_ascii_digit() { true } else { "`\"'([*<".contains(opening) }
    };
    let closes_well = last.ends_with(['.', ':', '?', '!']);
    let problem = if opens_well {
        if closes_well {
            None
        } else {
            Some("end the sentence with a full stop, or a colon before code")
        }
    } else {
        Some("start the sentence with a capital letter")
    };
    if problem.is_none() {
        assert!(closes_well, "a sentence without problem ends well");
    }
    problem
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns the messages of every violation in a Rust file.
    fn rust(text: &str) -> Vec<String> {
        check_text(&TextFile { path: "a.rs", text }, Syntax::Rust)
            .into_iter()
            .map(|violation| violation.message)
            .collect()
    }

    /// Accepts a line of exactly the limit and flags one character more; width counts characters,
    /// not bytes.
    #[test]
    fn line_width_limit_is_inclusive() {
        let limit = usize::try_from(LINE_WIDTH_MAX).map_or(0, |width| width);
        let widths = |text: &str| check_text_line_widths(&TextFile { path: "a.rs", text });
        assert!(widths(&"x".repeat(limit)).is_empty());
        let violations = widths(&format!("ok\n{}", "x".repeat(limit + 1)));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].line_number, 2);
        assert!(widths(&"\u{e9}".repeat(limit)).is_empty());
    }

    /// Accepts sentences and trailing phrases, and flags a lower-case start, a missing full stop
    /// and a missing space, once each.
    #[test]
    fn comments_must_be_sentences() {
        let valid =
            "// One sentence.\n// Spanning two lines, then code:\nlet x = 1; // trailing note";
        assert!(rust(valid).is_empty());
        assert_eq!(rust("// lower case start.").len(), 1);
        assert_eq!(rust("// No full stop").len(), 1);
        assert_eq!(rust("//No space.").len(), 1);
    }

    /// Checks doc comments paragraph by paragraph and item by item, skipping code blocks and
    /// headings.
    #[test]
    fn doc_comments_are_checked_by_paragraph() {
        let valid = concat!(
            "/// First paragraph.\n///\n/// # Panics\n///\n/// - An item.\n/// - Another one,\n",
            "///   on two lines.\n///\n/// ```\n/// let code = 1\n/// ```\n",
        );
        assert!(rust(valid).is_empty());
        assert_eq!(rust("/// Missing the stop\n").len(), 1);
        assert_eq!(rust("/// - A good item.\n/// - a bad one.\n").len(), 1);
    }

    /// Checks `#` comments of TOML and YAML, skipping separator lines and trailing comments.
    #[test]
    fn hash_comments_are_checked() {
        let valid = "# ======\n# A section.\nkey = 1 # a trailing note\n";
        let toml = TextFile { path: "a.toml", text: valid };
        assert!(check_text(&toml, Syntax::Hash).is_empty());
        let yaml = TextFile { path: "a.yml", text: "# no capital.\n" };
        assert_eq!(check_text(&yaml, Syntax::Hash).len(), 1);
        assert!(Syntax::from_extension("lock").is_none(), "lock files are not checked");
    }

    /// Starts a new block when the indentation changes, so each block is judged on its own.
    #[test]
    fn indentation_change_starts_a_new_block() {
        let file = TextFile { path: "a.rs", text: "// Outer.\n    // inner without a capital." };
        let violations = check_text(&file, Syntax::Rust);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].line_number, 2);
    }
}
