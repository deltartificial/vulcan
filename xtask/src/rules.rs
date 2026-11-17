//! Rules on Rust code that clippy has no lint for.
//!
//! - **Assertion density**: the code averages at least [`ASSERTION_DENSITY_MIN`] assertions per
//!   function, so functions do not operate on data they have not checked.
//! - **No recursion**: a function never calls itself, so every execution that should be bounded is
//!   bounded.
//! - **Bounded loops**: no `while` and no `loop`. A `for` loop over a range or a collection has a
//!   fixed upper bound.
//! - **Positive conditions**: no `if !`.
//! - **Simple conditions**: an `if` tests one condition, not `a && b` or `a || b`, and an assertion
//!   checks one condition, not `a && b`.
//! - **Described tests**: every test opens with a doc comment that says what it checks and how.
//! - **Helpers named after their caller**: a private function called from one function only is
//!   named after it, so its name shows the call history.
//! - **Named arguments**: a function takes at most one argument of each primitive type, and no
//!   `Option` argument; arguments that could be mixed up go in a struct with named fields.
//! - **Main first**: a binary opens with its `main` function.

use crate::{
    report::Violation,
    source::{
        Container, Function, Source, Visibility, code_part, declares_function, indentation,
        is_identifier_character, strip_literals,
    },
};

/// The minimum average number of assertions per function of the checked code.
pub(crate) const ASSERTION_DENSITY_MIN: u32 = 2;

/// Assertion macros, and calls of assertion helpers such as `self.assert_invariants()`.
const ASSERTION_PATTERNS: &[&str] = &[
    "assert!(",
    "assert_eq!(",
    "assert_ne!(",
    "static_assert_size!(",
    ".assert_",
    "unreachable!(",
];

/// Primitive types of which a function takes at most one argument.
const PRIMITIVE_TYPES: &[&str] = &[
    "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32",
    "u64", "u128", "usize",
];

/// Functions and assertions counted in the checked code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Density {
    /// Functions with a body.
    pub(crate) function_count: u32,
    /// Assertions anywhere in the code, compile-time ones included.
    pub(crate) assertion_count: u32,
}

impl Density {
    /// No function and no assertion.
    pub(crate) const ZERO: Self = Self { function_count: 0, assertion_count: 0 };

    /// Returns the sum of two counts.
    pub(crate) fn add(self, other: Self) -> Self {
        let sum = Self {
            function_count: self.function_count + other.function_count,
            assertion_count: self.assertion_count + other.assertion_count,
        };
        assert!(sum.function_count >= self.function_count, "a sum never shrinks");
        assert!(sum.assertion_count >= self.assertion_count, "a sum never shrinks");
        sum
    }

    /// Returns `true` if the code averages at least [`ASSERTION_DENSITY_MIN`] assertions per
    /// function.
    pub(crate) fn is_dense_enough(self) -> bool {
        let assertion_count_min = u64::from(self.function_count) * u64::from(ASSERTION_DENSITY_MIN);
        let dense = u64::from(self.assertion_count) >= assertion_count_min;
        if self.function_count == 0 {
            assert!(dense, "code without functions is dense enough");
        } else {
            if dense {
                assert!(self.assertion_count > 0, "dense code has assertions");
            }
        }
        dense
    }

    /// Returns the average number of assertions per function, in hundredths.
    pub(crate) fn hundredths_per_function(self) -> u64 {
        let hundredths = u64::from(self.assertion_count) * 100;
        let average =
            hundredths.checked_div(u64::from(self.function_count)).map_or(0, |value| value);
        assert!(average <= hundredths, "an average is at most the total");
        if self.function_count == 0 {
            assert!(average == 0, "no function, no average");
        }
        average
    }
}

/// Counts the functions and assertions of the code before the test module.
pub(crate) fn count_density(source: &Source<'_>) -> Density {
    let assertion_count: usize =
        source.code().iter().map(|line| count_density_assertions(line)).sum();
    let function_count = source.functions.iter().filter(|function| function.has_body).count();
    assert!(function_count <= source.functions.len(), "a subset of the functions");
    assert!(assertion_count <= source.code().len() * 8, "a line holds few assertions");
    Density {
        function_count: count_from_usize(function_count),
        assertion_count: count_from_usize(assertion_count),
    }
}

/// Returns the number of assertions on one line.
fn count_density_assertions(line: &str) -> usize {
    assert!(!line.contains('\n'), "one line at a time");
    let code = code_part(line);
    let count = ASSERTION_PATTERNS.iter().map(|pattern| code.matches(pattern).count()).sum();
    assert!(count <= code.len(), "an assertion takes more than one byte");
    count
}

/// Converts a count of lines or functions, which a source file keeps far below `u32::MAX`.
pub(crate) fn count_from_usize(count: usize) -> u32 {
    let converted = u32::try_from(count).map_or(u32::MAX, |value| value);
    assert!(converted < u32::MAX, "a source file has fewer than `u32::MAX` lines");
    assert!(u64::from(converted) == u64::try_from(count).map_or(0, |value| value), "no loss");
    converted
}

/// Returns the violations of every rule of this module in one file.
pub(crate) fn check_rules(path: &str, source: &Source<'_>) -> Vec<Violation> {
    assert!(!path.is_empty(), "a file has a path");
    let mut violations = Vec::new();
    violations.extend(check_rules_recursion(path, source));
    violations.extend(check_rules_lines(path, source));
    violations.extend(check_rules_test_descriptions(path, source));
    violations.extend(check_rules_helper_names(path, source));
    violations.extend(check_rules_parameters(path, source));
    violations.extend(check_rules_main_first(path, source));
    assert!(violations.iter().all(|violation| violation.path == path), "one file at a time");
    violations
}

/// Reports functions that call themselves.
fn check_rules_recursion(path: &str, source: &Source<'_>) -> Vec<Violation> {
    let mut violations = Vec::new();
    for function in source.functions.iter().filter(|function| function.has_body) {
        let calls_itself =
            function.body.iter().any(|line| check_rules_recursion_calls(function, line));
        if calls_itself {
            violations.push(Violation::new(
                path,
                function.line_index_first,
                format!("`{}` calls itself: use a loop over an explicit stack", function.name),
            ));
        }
    }
    assert!(violations.len() <= source.functions.len(), "one violation per function at most");
    assert!(violations.iter().all(|violation| violation.path == path), "one file at a time");
    violations
}

/// Returns `true` if `line` calls `function` by name.
fn check_rules_recursion_calls(function: &Function<'_>, line: &str) -> bool {
    assert!(function.has_body, "only a body can call");
    let code = code_part(line);
    let calls = match function.container {
        Container::Module => {
            let call = format!("{}(", function.name);
            occurrences(code, &call).any(|offset| {
                let before = code.get(..offset).map_or("", |prefix| prefix);
                let qualified = before.ends_with('.') || before.ends_with("::");
                !qualified
            })
        }
        Container::Inherent | Container::Trait => {
            let method = format!("self.{}(", function.name);
            let associated = format!("Self::{}(", function.name);
            code.contains(&method) || code.contains(&associated)
        }
    };
    if calls {
        assert!(code.contains(function.name), "a call names the function");
    }
    calls
}

/// Returns the offsets of `needle` in `haystack` where it starts a whole identifier.
fn occurrences<'haystack>(
    haystack: &'haystack str,
    needle: &'haystack str,
) -> impl Iterator<Item = usize> + 'haystack {
    assert!(!needle.is_empty(), "an empty needle matches everywhere");
    let first = needle.chars().next().is_some_and(is_identifier_character);
    assert!(first, "a needle starts with an identifier");
    haystack.match_indices(needle).map(|(offset, _)| offset).filter(move |&offset| {
        let before = haystack.get(..offset).and_then(|prefix| prefix.chars().next_back());
        before.is_none_or(|character| !is_identifier_character(character))
    })
}

/// Reports `while`, `loop` and `if !` on every code line, tests included.
fn check_rules_lines(path: &str, source: &Source<'_>) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (line_index, line) in source.lines.iter().enumerate() {
        let code = strip_literals(code_part(line));
        let words = check_rules_lines_words(&code);
        let unbounded = words.contains(&"while") || words.contains(&"loop");
        if unbounded {
            violations.push(Violation::new(
                path,
                line_index,
                "unbounded loop: iterate with `for` over a range or a collection".to_owned(),
            ));
        }
        if code.contains("if !") {
            violations.push(Violation::new(
                path,
                line_index,
                "negated condition: state it positively, or swap the branches".to_owned(),
            ));
        }
        if let Some(message) = check_rules_lines_compound(&code) {
            violations.push(Violation::new(path, line_index, message.to_owned()));
        }
    }
    assert!(violations.len() <= source.lines.len() * 3, "three rules per line at most");
    assert!(violations.iter().all(|violation| violation.path == path), "one file at a time");
    violations
}

/// Returns why `code` breaks the rule on compound conditions, if it does: an `if` that joins
/// conditions with `&&` or `||`, or an assertion that joins them with `&&`.
fn check_rules_lines_compound(code: &str) -> Option<&'static str> {
    assert!(!code.contains('\n'), "one line at a time");
    let joined = code.contains(" && ");
    let alternative = code.contains(" || ");
    let words = check_rules_lines_words(code);
    let condition = words.contains(&"if");
    let assertion = code.contains("assert!(");
    let compound = joined || alternative;
    let message = if condition {
        compound.then_some("compound condition: split it into nested `if`s")
    } else {
        if assertion {
            joined.then_some("compound assertion: one condition per assertion")
        } else {
            None
        }
    };
    if message.is_some() {
        assert!(joined || alternative, "only a joined condition is compound");
    }
    message
}

/// Returns the identifiers and keywords of `code`.
fn check_rules_lines_words(code: &str) -> Vec<&str> {
    let words: Vec<&str> = code
        .split(|character: char| !is_identifier_character(character))
        .filter(|word| !word.is_empty())
        .collect();
    assert!(words.len() <= code.len(), "a word takes at least one byte");
    assert!(words.iter().all(|word| word.chars().all(is_identifier_character)), "whole words");
    words
}

/// Reports tests without a doc comment that describes them.
fn check_rules_test_descriptions(path: &str, source: &Source<'_>) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (line_index, line) in source.lines.iter().enumerate() {
        if line.trim() == "#[test]" {
            let described = source.lines[..line_index]
                .iter()
                .rev()
                .find(|above| !above.trim_start().starts_with("#["))
                .is_some_and(|above| above.trim_start().starts_with("///"));
            if described {
                assert!(line_index > 0, "a described test has a line above it");
            } else {
                violations.push(Violation::new(
                    path,
                    line_index,
                    "describe the test: a doc comment with its goal and method".to_owned(),
                ));
            }
        }
    }
    assert!(violations.len() <= source.lines.len(), "one violation per line at most");
    violations
}

/// Reports private helpers called from a single function and not named after it.
fn check_rules_helper_names(path: &str, source: &Source<'_>) -> Vec<Violation> {
    let mut violations = Vec::new();
    let helpers = source.functions.iter().filter(|function| {
        let private = function.visibility == Visibility::Private;
        private && function.container != Container::Trait && function.name != "main"
    });
    for helper in helpers {
        if let Some(caller) = check_rules_helper_names_caller(source, helper) {
            let prefix = format!("{}_", caller.name);
            // A helper of a method may lead with the type's name too: `source_new_blocks`.
            let qualified = caller.container == Container::Inherent &&
                helper.name.contains(&format!("_{prefix}"));
            let prefixed = helper.name.starts_with(&prefix);
            let named = prefixed || qualified;
            if named {
                assert!(helper.name.len() > prefix.len(), "a helper adds to its caller's name");
            } else {
                assert!(caller.name != helper.name, "a helper is not its own caller");
                violations.push(Violation::new(
                    path,
                    helper.line_index_first,
                    format!(
                        "`{}` is only called by `{}`: name it `{prefix}..`",
                        helper.name, caller.name
                    ),
                ));
            }
        }
    }
    violations
}

/// Returns the one function that names `helper`, if every use of `helper` is in that function.
fn check_rules_helper_names_caller<'source, 'text>(
    source: &'source Source<'text>,
    helper: &Function<'text>,
) -> Option<&'source Function<'text>> {
    assert!(helper.visibility == Visibility::Private, "only a private helper has one caller");
    let mut caller: Option<&Function<'text>> = None;
    for (line_index, line) in source.code().iter().enumerate() {
        let declaration = line_index == helper.line_index_first;
        let uses = occurrences(code_part(line), helper.name).any(|offset| {
            let after = code_part(line).get(offset + helper.name.len()..).map_or("", |rest| rest);
            after.chars().next().is_none_or(|character| !is_identifier_character(character))
        });
        let call = if declaration { false } else { uses };
        if call {
            let around = source.function_around(line_index)?;
            let same = caller.is_none_or(|known| known.line_index_first == around.line_index_first);
            if same {
                caller = Some(around);
            } else {
                return None;
            }
        }
    }
    let caller_found = caller?;
    assert!(caller_found.has_body, "a caller has a body");
    let is_helper = caller_found.line_index_first == helper.line_index_first;
    if is_helper { None } else { Some(caller_found) }
}

/// Reports functions with two arguments of one primitive type, or an `Option` argument.
fn check_rules_parameters(path: &str, source: &Source<'_>) -> Vec<Violation> {
    let mut violations = Vec::new();
    let checked = source.functions.iter().filter(|function| function.container != Container::Trait);
    for function in checked {
        let types = check_rules_parameters_types(&function.signature);
        assert!(types.len() <= function.signature.len(), "a type takes at least one byte");
        let repeated = PRIMITIVE_TYPES.iter().find(|primitive| {
            let count = types.iter().filter(|parameter| parameter == primitive).count();
            count >= 2
        });
        if let Some(primitive) = repeated {
            violations.push(Violation::new(
                path,
                function.line_index_first,
                format!("`{}` takes two `{primitive}`: pass them in a struct", function.name),
            ));
        }
        if types.iter().any(|parameter| parameter.starts_with("Option<")) {
            violations.push(Violation::new(
                path,
                function.line_index_first,
                format!("`{}` takes an `Option`: name it in a struct field", function.name),
            ));
        }
    }
    assert!(violations.len() <= source.functions.len() * 2, "two violations per function");
    violations
}

/// Returns the types of the parameters in `signature`, `self` excluded.
fn check_rules_parameters_types(signature: &str) -> Vec<&str> {
    let Some(parameters) = check_rules_parameters_types_list(signature) else {
        return Vec::new();
    };
    let types: Vec<&str> = check_rules_parameters_types_split(parameters)
        .into_iter()
        .filter_map(|parameter| parameter.split_once(':').map(|(_, kind)| kind.trim()))
        .collect();
    assert!(types.len() <= parameters.len() + 1, "a parameter takes at least one byte");
    assert!(types.iter().all(|kind| !kind.is_empty()), "every parameter has a type");
    types
}

/// Returns the text between the parentheses of the parameter list of `signature`.
fn check_rules_parameters_types_list(signature: &str) -> Option<&str> {
    assert!(!signature.is_empty(), "a signature has text");
    let (_, after_name) = signature.split_once("fn ")?;
    let mut angle_depth = 0_u32;
    let mut paren_depth = 0_u32;
    let mut start = None;
    let mut previous = ' ';
    for (offset, character) in after_name.char_indices() {
        match character {
            '<' => angle_depth += 1,
            '>' if previous != '-' => angle_depth = angle_depth.saturating_sub(1),
            '(' if angle_depth == 0 => {
                paren_depth += 1;
                if paren_depth == 1 {
                    start = Some(offset + 1);
                }
            }
            ')' if angle_depth == 0 => {
                paren_depth = paren_depth.saturating_sub(1);
                if paren_depth == 0 {
                    let list = after_name.get(start?..offset)?;
                    assert!(list.len() < after_name.len(), "the list is inside the signature");
                    return Some(list);
                }
            }
            _ => {}
        }
        previous = character;
    }
    None
}

/// Splits `list` at the commas outside brackets.
fn check_rules_parameters_types_split(list: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_u32;
    let mut start = 0_usize;
    let mut previous = ' ';
    for (offset, character) in list.char_indices() {
        match character {
            '(' | '[' | '<' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            '>' if previous != '-' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(list.get(start..offset).map_or("", str::trim));
                start = offset + 1;
            }
            _ => {}
        }
        previous = character;
    }
    parts.push(list.get(start..).map_or("", str::trim));
    let nonempty: Vec<&str> = parts.into_iter().filter(|part| !part.is_empty()).collect();
    assert!(nonempty.len() <= list.len(), "a part takes at least one byte");
    assert!(nonempty.iter().all(|part| part.trim() == *part), "parts are trimmed");
    nonempty
}

/// Reports a binary whose first item is not its `main` function.
fn check_rules_main_first(path: &str, source: &Source<'_>) -> Vec<Violation> {
    assert!(source.code_line_count <= source.lines.len(), "the code is a prefix of the file");
    let has_main = source
        .functions
        .iter()
        .any(|function| function.name == "main" && function.container == Container::Module);
    if has_main {
        let first_item = source.code().iter().enumerate().find(|&(_, line)| {
            let top_level = indentation(line) == 0 && !line.trim().is_empty();
            let preamble = ["//", "#", "use ", "mod ", "pub mod ", "pub use ", "}", ")", "    "]
                .iter()
                .any(|prefix| line.starts_with(prefix));
            top_level && !preamble
        });
        if let Some((line_index, line)) = first_item {
            let main_first = declares_function(line) && line.contains("fn main(");
            if main_first {
                assert!(line_index < source.code_line_count, "`main` is in the code");
            } else {
                return vec![Violation::new(
                    path,
                    line_index,
                    "put `main` first: it is where a reader starts".to_owned(),
                )];
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns the messages of every rule violation in `text`.
    fn messages(text: &str) -> Vec<String> {
        check_rules("test.rs", &Source::new(text))
            .into_iter()
            .map(|violation| violation.message)
            .collect()
    }

    /// Counts functions with a body and assertions outside the test module, and compares the
    /// average with the minimum on both sides of it.
    #[test]
    fn density_counts_functions_and_assertions_outside_tests() {
        let text = concat!(
            "fn short() -> u8 {\n    1\n}\n",
            "fn long(value: u8) -> u8 {\n    assert!(value > 0, \"x\");\n    value\n}\n",
            "fn helper(&self) {\n    self.assert_invariants();\n}\n",
            "#[cfg(test)]\n",
            "fn ignored() {\n    assert!(true);\n    assert!(true);\n}\n",
        );
        let density = count_density(&Source::new(text));
        assert_eq!(density, Density { function_count: 3, assertion_count: 2 });
        assert!(!density.is_dense_enough(), "two assertions for three functions");
        assert_eq!(density.hundredths_per_function(), 66);
        let dense = density.add(Density { function_count: 0, assertion_count: 4 });
        assert!(dense.is_dense_enough(), "six assertions for three functions");
        assert!(Density::ZERO.is_dense_enough(), "no function, nothing to check");
    }

    /// Flags a free function and a method that call themselves, and leaves alone a method that
    /// calls a function of the same name on another receiver.
    #[test]
    fn recursion_is_reported_for_free_functions_and_methods() {
        let free = messages("fn walk(depth: u8) {\n    walk(depth);\n}\n");
        assert!(free.iter().any(|message| message.contains("calls itself")));
        let method =
            messages("impl Tree {\n    fn walk(&self) {\n        self.walk();\n    }\n}\n");
        assert!(method.iter().any(|message| message.contains("calls itself")));
        let other = messages(
            "impl Tree {\n    fn len(&self) -> u32 {\n        self.items.len()\n    }\n}\n",
        );
        assert!(other.iter().all(|message| !message.contains("calls itself")));
    }

    /// Flags `while`, `loop` and `if !`, but not the same words inside strings or
    /// comments.
    #[test]
    fn unbounded_loops_and_negations_are_reported() {
        assert_eq!(messages("fn spin() {\n    loop {}\n}\n").len(), 1);
        assert_eq!(messages("fn wait() {\n    while ready() {}\n}\n").len(), 1);
        assert_eq!(messages("fn check() {\n    if !ready() {}\n}\n").len(), 1);
        assert!(messages("fn text() {\n    say(\"while\"); // loop\n}\n").is_empty());
    }

    /// Flags an `if` that joins conditions and an assertion that joins them with `&&`, and accepts
    /// a named compound condition and an assertion with an alternative.
    #[test]
    fn compound_conditions_are_reported() {
        assert_eq!(messages("fn check() {\n    if ready && open {}\n}\n").len(), 1);
        assert_eq!(messages("fn check() {\n    if ready || open {}\n}\n").len(), 1);
        assert_eq!(messages("fn check() {\n    assert!(ready && open, \"x\");\n}\n").len(), 1);
        assert!(messages("fn check() {\n    let both = ready && open;\n}\n").is_empty());
        assert!(messages("fn check() {\n    assert!(ready || open, \"x\");\n}\n").is_empty());
    }

    /// Accepts a test with a doc comment above its attributes and flags one without.
    #[test]
    fn tests_need_a_description() {
        let described = "/// Checks one thing.\n#[test]\n#[should_panic]\nfn checks() {}\n";
        assert!(messages(described).is_empty());
        let bare = "// A plain comment.\n#[test]\nfn checks() {}\n";
        assert_eq!(messages(bare).len(), 1);
    }

    /// Flags a helper called only by `parse` but not named after it, and accepts the renamed
    /// helper and a helper shared by two callers.
    #[test]
    fn helpers_are_named_after_their_only_caller() {
        let single = "fn parse() {\n    digits();\n}\nfn digits() {}\n";
        assert!(messages(single).iter().any(|message| message.contains("name it `parse_..`")));
        let renamed = "fn parse() {\n    parse_digits();\n}\nfn parse_digits() {}\n";
        assert!(messages(renamed).is_empty());
        let shared =
            "fn parse() {\n    digits();\n}\nfn print() {\n    digits();\n}\nfn digits() {}\n";
        assert!(messages(shared).is_empty());
    }

    /// Flags two arguments of one primitive type and an `Option` argument, and accepts two
    /// arguments of different types.
    #[test]
    fn parameters_that_can_be_mixed_up_are_reported() {
        assert_eq!(messages("pub fn span(low: u32, high: u32) {}\n").len(), 1);
        assert_eq!(messages("pub fn pick(choice: Option<u8>) {}\n").len(), 1);
        assert!(messages("pub fn fill<F: Fn(u8) -> u8>(count: u32, fill: F) {}\n").is_empty());
    }

    /// Flags a binary that declares a constant before `main`, and accepts one that opens with it.
    #[test]
    fn main_comes_first() {
        let late = "use std::io;\nconst LIMIT: u8 = 1;\nfn main() {}\n";
        assert_eq!(messages(late).len(), 1);
        let first = "use std::io;\nfn main() {}\nconst LIMIT: u8 = 1;\n";
        assert!(messages(first).is_empty());
    }
}
