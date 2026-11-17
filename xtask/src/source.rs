//! A line-based reading of Rust source files: enough structure for the repository rules, without
//! a parser dependency.
//!
//! `rustfmt` formats every file, so the shapes this module relies on hold: a function opens with
//! `fn` at the start of a line and closes with a `}` at the same indentation, and so does an
//! `impl` or `trait` block.

/// Function declaration prefixes, after the indentation.
const FUNCTION_PREFIXES: &[&str] = &[
    "fn ",
    "const fn ",
    "unsafe fn ",
    "pub fn ",
    "pub const fn ",
    "pub unsafe fn ",
    "pub(crate) fn ",
    "pub(crate) const fn ",
    "pub(super) fn ",
    "pub(super) const fn ",
];

/// Block prefixes, after the indentation, whose functions belong to a type or a trait.
const BLOCK_PREFIXES: &[&str] =
    &["impl", "unsafe impl", "trait ", "pub trait ", "pub(crate) trait "];

/// The first line of the test module, after which every line is test code.
const TEST_MODULE_MARKER: &str = "#[cfg(test)]";

/// A Rust file split into lines, with its functions and `impl`/`trait` blocks.
#[derive(Debug)]
pub(crate) struct Source<'text> {
    /// Every line of the file.
    pub(crate) lines: Vec<&'text str>,
    /// The number of lines before the test module; the rest is test code.
    pub(crate) code_line_count: usize,
    /// The functions of the code before the test module, in file order.
    pub(crate) functions: Vec<Function<'text>>,
}

/// One function: its name, where it sits, and how it is declared.
#[derive(Debug, Clone)]
pub(crate) struct Function<'text> {
    /// The identifier after `fn`.
    pub(crate) name: &'text str,
    /// The line of the `fn` keyword.
    pub(crate) line_index_first: usize,
    /// The line that holds the body's closing brace, or the `;` of a declaration.
    pub(crate) line_index_last: usize,
    /// The declaration up to its opening brace, lines joined with spaces.
    pub(crate) signature: String,
    /// The lines strictly between the opening and the closing brace.
    pub(crate) body: Vec<&'text str>,
    /// How far the function can be seen from.
    pub(crate) visibility: Visibility,
    /// Where the function is declared.
    pub(crate) container: Container,
    /// Whether the declaration has a body, unlike a trait method signature.
    pub(crate) has_body: bool,
}

/// How far a function can be seen from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Visibility {
    /// No `pub`: callable from its own module only.
    Private,
    /// Any form of `pub`.
    Public,
}

/// Where a function is declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Container {
    /// At module level.
    Module,
    /// In an inherent `impl` block.
    Inherent,
    /// In a trait definition or a trait `impl`, whose names the trait decides.
    Trait,
}

/// An `impl` or `trait` block: its line range and kind.
#[derive(Debug, Clone, Copy)]
struct Block {
    /// The line of the `impl` or `trait` keyword.
    line_index_first: usize,
    /// The line of the closing brace.
    line_index_last: usize,
    /// [`Container::Inherent`] or [`Container::Trait`].
    container: Container,
}

impl<'text> Source<'text> {
    /// Reads `text`.
    pub(crate) fn new(text: &'text str) -> Self {
        let lines: Vec<&str> = text.lines().collect();
        let code_line_count = lines
            .iter()
            .position(|line| line.trim_start().starts_with(TEST_MODULE_MARKER))
            .map_or(lines.len(), |position| position);
        assert!(code_line_count <= lines.len(), "the code is a prefix of the file");
        let code = &lines[..code_line_count];
        let blocks = source_new_blocks(code);
        let functions = source_new_functions(code, &blocks);
        assert!(functions.len() <= code_line_count, "a function takes at least one line");
        let ordered = functions
            .iter()
            .zip(functions.iter().skip(1))
            .all(|(first, second)| first.line_index_first < second.line_index_first);
        assert!(ordered, "functions come in file order");
        Self { lines, code_line_count, functions }
    }

    /// Returns the lines before the test module.
    pub(crate) fn code(&self) -> &[&'text str] {
        assert!(self.code_line_count <= self.lines.len(), "the code is a prefix of the file");
        let inside =
            self.functions.iter().all(|function| function.line_index_last < self.code_line_count);
        assert!(inside, "every function is in the code");
        &self.lines[..self.code_line_count]
    }

    /// Returns the innermost function whose lines include `line_index`.
    pub(crate) fn function_around(&self, line_index: usize) -> Option<&Function<'text>> {
        assert!(line_index < self.lines.len(), "the line is in the file");
        let found = self.functions.iter().rev().find(|function| {
            function.line_index_first <= line_index && line_index <= function.line_index_last
        });
        if let Some(function) = found {
            assert!(function.line_index_first <= line_index, "the function starts at or before");
            assert!(line_index <= function.line_index_last, "the function ends at or after");
        }
        found
    }
}

/// Returns the indentation of `line`, in bytes.
pub(crate) fn indentation(line: &str) -> usize {
    let width = line.len() - line.trim_start().len();
    assert!(width <= line.len(), "the indentation is part of the line");
    let blank = line.get(..width).is_some_and(|prefix| prefix.trim().is_empty());
    assert!(blank, "the indentation is whitespace");
    width
}

/// Returns the code of `line`: the part before a `//` comment, outside string literals.
pub(crate) fn code_part(line: &str) -> &str {
    assert!(!line.contains('\n'), "one line at a time");
    let mut in_string = false;
    let mut previous = ' ';
    for (offset, character) in line.char_indices() {
        let quote = character == '"' && previous != '\\';
        let comment = character == '/' && previous == '/' && !in_string;
        if quote {
            in_string = !in_string;
        }
        if comment {
            let code = line.get(..offset - 1).map_or(line, |code| code);
            assert!(code.len() < line.len(), "the comment is cut off");
            return code;
        }
        previous = character;
    }
    line
}

/// Returns `code` with the contents of its string and character literals removed, so a keyword
/// inside a literal is not taken for code.
pub(crate) fn strip_literals(code: &str) -> String {
    assert!(!code.contains('\n'), "one line at a time");
    let mut stripped = String::with_capacity(code.len());
    let mut in_string = false;
    let mut escaped = false;
    for character in code.chars() {
        if in_string {
            let closes = character == '"' && !escaped;
            escaped = character == '\\' && !escaped;
            if closes {
                in_string = false;
                stripped.push(character);
            }
        } else {
            in_string = character == '"';
            stripped.push(character);
        }
    }
    assert!(stripped.len() <= code.len(), "stripping only removes characters");
    stripped
}

/// Returns `true` if `line` declares a function.
pub(crate) fn declares_function(line: &str) -> bool {
    assert!(!line.contains('\n'), "one line at a time");
    let declaration = line.trim_start();
    let declares = FUNCTION_PREFIXES.iter().any(|prefix| declaration.starts_with(prefix));
    if declares {
        assert!(line.contains("fn "), "a declaration has the `fn` keyword");
    }
    declares
}

/// Returns `true` if `character` can be part of an identifier.
pub(crate) const fn is_identifier_character(character: char) -> bool {
    let identifier = character.is_ascii_alphanumeric() || character == '_';
    if identifier {
        assert!(character.is_ascii(), "identifiers of this workspace are ASCII");
    }
    identifier
}

/// Returns the line that closes the brace opened at `line_index_first`, at the same indentation.
fn source_new_closing(lines: &[&str], line_index_first: usize) -> Option<usize> {
    assert!(line_index_first < lines.len(), "the opening line is in the file");
    let closing_width = indentation(lines[line_index_first]);
    let found = (line_index_first..lines.len()).find(|&line_index| {
        let line = lines[line_index];
        line.trim_end().ends_with('}') && indentation(line) == closing_width && {
            let trimmed = line.trim();
            trimmed == "}" || trimmed == "};" || line_index == line_index_first
        }
    });
    if let Some(line_index_last) = found {
        assert!(line_index_last >= line_index_first, "a block closes after it opens");
    }
    found
}

/// Returns the `impl` and `trait` blocks of `lines`.
fn source_new_blocks(lines: &[&str]) -> Vec<Block> {
    let mut blocks = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        let declaration = line.trim_start();
        let opens_block = BLOCK_PREFIXES.iter().any(|prefix| declaration.starts_with(prefix));
        // A header with a `where` clause opens its brace on a later line, at the same indentation.
        let declared_only = declaration.trim_end().ends_with(';');
        let opens = opens_block && !declared_only;
        if opens {
            let is_trait = declaration.contains(" for ") || declaration.contains("trait ");
            let container = if is_trait { Container::Trait } else { Container::Inherent };
            if let Some(line_index_last) = source_new_closing(lines, line_index) {
                blocks.push(Block { line_index_first: line_index, line_index_last, container });
            }
        }
    }
    assert!(blocks.len() <= lines.len(), "a block takes at least one line");
    let spans = blocks.iter().all(|block| block.line_index_first <= block.line_index_last);
    assert!(spans, "a block ends where it starts or later");
    blocks
}

/// Returns the functions of `lines`, each with the container that `blocks` gives it.
fn source_new_functions<'text>(lines: &[&'text str], blocks: &[Block]) -> Vec<Function<'text>> {
    let mut functions = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        let function = declares_function(line)
            .then(|| source_new_functions_one(lines, line_index, blocks))
            .flatten();
        functions.extend(function);
    }
    assert!(functions.len() <= lines.len(), "a function takes at least one line");
    assert!(functions.iter().all(|function| !function.name.is_empty()), "functions have names");
    functions
}

/// Reads the function declared at `lines[line_index_first]`.
fn source_new_functions_one<'text>(
    lines: &[&'text str],
    line_index_first: usize,
    blocks: &[Block],
) -> Option<Function<'text>> {
    assert!(declares_function(lines[line_index_first]), "a function starts here");
    let declaration = lines[line_index_first].trim_start();
    let name = source_new_functions_one_name(declaration)?;
    // The signature may span several lines (parameters, `where` clauses) before its `{`.
    let line_index_opening = (line_index_first..lines.len()).find(|&line_index| {
        let code = code_part(lines[line_index]).trim_end();
        code.ends_with('{') || code.ends_with(';') || code.ends_with('}')
    })?;
    let opening = code_part(lines[line_index_opening]).trim_end();
    let signature_lines = &lines[line_index_first..=line_index_opening];
    let signature = signature_lines.iter().map(|line| line.trim()).collect::<Vec<_>>().join(" ");
    let has_body = !opening.ends_with(';');
    let line_index_last = if opening.ends_with('{') {
        source_new_closing(lines, line_index_first)?
    } else {
        line_index_opening
    };
    let body = if line_index_last == line_index_opening {
        // A one-line function: its body is the text after the first brace.
        opening.split_once('{').map_or_else(Vec::new, |(_, inline)| vec![inline])
    } else {
        lines.get(line_index_opening + 1..line_index_last).map_or_else(Vec::new, Vec::from)
    };
    let visibility =
        if declaration.starts_with("pub") { Visibility::Public } else { Visibility::Private };
    let container = blocks
        .iter()
        .rev()
        .find(|block| {
            block.line_index_first < line_index_first && line_index_last <= block.line_index_last
        })
        .map_or(Container::Module, |block| block.container);
    assert!(line_index_first <= line_index_last, "a function ends where it starts or later");
    Some(Function {
        name,
        line_index_first,
        line_index_last,
        signature,
        body,
        visibility,
        container,
        has_body,
    })
}

/// Returns the identifier after `fn ` in `declaration`.
fn source_new_functions_one_name(declaration: &str) -> Option<&str> {
    let (_, after) = declaration.split_once("fn ")?;
    let name_length = after.find(|character| !is_identifier_character(character))?;
    let name = after.get(..name_length)?;
    assert!(name.chars().all(is_identifier_character), "a name is one identifier");
    assert!(declaration.contains(name), "the name is part of the declaration");
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads a file with a free function, a multi-line signature, an inherent method, a trait
    /// method and a test module, and checks the extent and container of each function.
    #[test]
    fn functions_have_extent_and_container() {
        let text = concat!(
            "fn free() -> u8 {\n    1\n}\n",
            "pub fn long(\n    value: u8,\n) -> u8\nwhere\n    u8: Copy,\n{\n    value\n}\n",
            "impl<T> Thing<T>\nwhere\n    T: Copy,\n{\n    fn method(&self) {}\n}\n",
            "impl Display for Thing {\n    fn fmt(&self) {\n        body();\n    }\n}\n",
            "#[cfg(test)]\nfn ignored() {}\n",
        );
        let source = Source::new(text);
        let names: Vec<&str> = source.functions.iter().map(|function| function.name).collect();
        assert_eq!(names, ["free", "long", "method", "fmt"]);
        assert_eq!(source.functions[0].body, ["    1"]);
        assert_eq!(source.functions[1].visibility, Visibility::Public);
        assert_eq!(source.functions[1].body, ["    value"]);
        assert_eq!(source.functions[2].container, Container::Inherent);
        assert_eq!(source.functions[3].container, Container::Trait);
        assert_eq!(source.code().len(), 22);
    }

    /// Cuts comments off code lines, but not a `//` inside a string literal.
    #[test]
    fn code_part_ignores_comments_outside_strings() {
        assert_eq!(code_part("let value = 1; // note"), "let value = 1; ");
        assert_eq!(
            code_part("let url = \"https://example.com\";"),
            "let url = \"https://example.com\";"
        );
        assert_eq!(code_part("// only a comment"), "");
    }
}
