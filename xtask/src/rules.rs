//! Rules on Rust code that clippy has no lint for.
//!
//! - **Assertion density**: the code averages at least [`ASSERTION_DENSITY_MIN`] assertions per
//!   function, so functions do not operate on data they have not checked.
//! - **No blind function**: in the code whose density is counted, a function of several statements
//!   holds at least one assertion. Only a function that is a single expression, a delegation or an
//!   accessor, may hold none: it has no data of its own to check.
//! - **No recursion**: a function never calls itself, directly or through other functions of its
//!   file, so every execution that should be bounded is bounded.
//! - **Bounded loops**: no `while` and no `loop`. A `for` loop over a range or a collection has a
//!   fixed upper bound.
//! - **Positive conditions**: no `if !`.
//! - **Simple conditions**: there is no `&&` and no `||`, in an `if`, in an assertion or anywhere
//!   else, so a condition cannot be compound, in place, over several lines or behind a name.
//!   Several conditions are nested `if`s or a `match` over the cases, which shows every case.
//! - **Described tests**: every test opens with a doc comment that says what it checks and how.
//! - **Helpers named after their caller**: a private function called from one function only is
//!   named after it, so its name shows the call history.
//! - **Named arguments**: a function takes at most one argument of each type, and no `Option`
//!   argument; arguments that could be mixed up go in a struct with named fields.
//! - **Named type parameters**: a type parameter is a word (`Item`), not a letter (`T`).
//! - **Main first**: a binary opens with its `main` function.

use crate::{
    report::Violation,
    source::{
        Container, Function, Source, Visibility, code_part, indentation, is_identifier_character,
        strip_literals,
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

/// Reports functions of several statements that hold no assertion. A function that never
/// returns (`-> !`) is left alone: it is where a failed assertion ends.
pub(crate) fn check_blind_functions(path: &str, source: &Source<'_>) -> Vec<Violation> {
    let mut violations = Vec::new();
    let checked = source
        .functions
        .iter()
        .filter(|function| function.has_body)
        .filter(|function| !function.signature.contains("-> !"));
    for function in checked {
        let statement_count = check_blind_functions_statements(source, function);
        let asserts = function.body.iter().any(|line| count_density_assertions(line) > 0);
        if statement_count > 1 {
            if asserts {
                assert!(!function.body.is_empty(), "an assertion is a line of the body");
            } else {
                violations.push(Violation::new(
                    path,
                    function.line_index_first,
                    format!("`{}` has several statements and no assertion", function.name),
                ));
            }
        }
    }
    assert!(violations.len() <= source.functions.len(), "one violation per function at most");
    violations
}

/// Counts the statements of the body of `function`: the lines at the indentation of the body
/// that open one, which leaves out comments, attributes and the lines that close a block.
fn check_blind_functions_statements(source: &Source<'_>, function: &Function<'_>) -> usize {
    let declaration = source.lines.get(function.line_index_first).map_or("", |line| *line);
    let body_width = indentation(declaration) + 4;
    let statement_count = function
        .body
        .iter()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| indentation(line) == body_width)
        .filter(|line| !line.trim_start().starts_with(['}', ')', ']', '#', '/']))
        .count();
    assert!(statement_count <= function.body.len(), "a statement takes at least one line");
    statement_count
}

/// A possible call from one function of a file to another.
#[derive(Clone, Copy)]
struct Call<'source, 'text> {
    /// The function whose body is searched.
    caller: &'source Function<'text>,
    /// The function whose name is searched for.
    callee: &'source Function<'text>,
}

/// Reports functions that call themselves, directly or through other functions of the file.
fn check_rules_recursion(path: &str, source: &Source<'_>) -> Vec<Violation> {
    let functions = &source.functions;
    // `calls[caller][callee]` holds if the body of `caller` names `callee`.
    let calls: Vec<Vec<bool>> = functions
        .iter()
        .map(|caller| {
            functions
                .iter()
                .map(|callee| check_rules_recursion_calls(Call { caller, callee }))
                .collect()
        })
        .collect();
    let reaches = check_rules_recursion_closure(&calls);
    let mut violations = Vec::new();
    for (index, function) in functions.iter().enumerate() {
        if reaches[index][index] {
            let how =
                if calls[index][index] { "calls itself" } else { "calls itself through others" };
            violations.push(Violation::new(
                path,
                function.line_index_first,
                format!("`{}` {how}: use a loop over an explicit stack", function.name),
            ));
        }
    }
    assert!(violations.len() <= source.functions.len(), "one violation per function at most");
    assert!(violations.iter().all(|violation| violation.path == path), "one file at a time");
    violations
}

/// Returns `true` if the body of `call.caller` calls `call.callee` by name.
fn check_rules_recursion_calls(call: Call<'_, '_>) -> bool {
    let Call { caller, callee } = call;
    assert!(!callee.name.is_empty(), "a function has a name");
    let plain = format!("{}(", callee.name);
    let method = format!("self.{}(", callee.name);
    let associated = format!("Self::{}(", callee.name);
    let qualified = format!("{}::{}(", callee.owner, callee.name);
    let same_owner = caller.owner == callee.owner;
    let calls = caller.body.iter().any(|line| {
        let code = code_part(line);
        match callee.container {
            Container::Module => {
                occurrences(&Search { haystack: code, needle: &plain }).any(|offset| {
                    let before = code.get(..offset).map_or("", |prefix| prefix);
                    // A name after `.` or `::` is a method or a path, not this free function.
                    !before.ends_with(['.', ':'])
                })
            }
            Container::Inherent | Container::Trait => {
                let on_self =
                    [&method, &associated].iter().any(|pattern| code.contains(pattern.as_str()));
                let through_self = if same_owner { on_self } else { false };
                let through_path = if callee.owner.is_empty() {
                    false
                } else {
                    occurrences(&Search { haystack: code, needle: &qualified }).next().is_some()
                };
                if through_self { true } else { through_path }
            }
        }
    });
    if calls {
        assert!(caller.has_body, "only a body can call");
    }
    calls
}

/// Returns, for every pair of functions, whether the first reaches the second through a chain
/// of one or more calls: the transitive closure of `calls`, by Warshall's algorithm.
fn check_rules_recursion_closure(calls: &[Vec<bool>]) -> Vec<Vec<bool>> {
    let function_count = calls.len();
    assert!(calls.iter().all(|row| row.len() == function_count), "a square table");
    let mut reaches = calls.to_vec();
    for through in 0..function_count {
        let onward = reaches[through].clone();
        for row in reaches.iter_mut().filter(|row| row[through]) {
            for (reached, continues) in row.iter_mut().zip(&onward) {
                *reached |= *continues;
            }
        }
    }
    let kept = calls.iter().zip(&reaches).all(|(direct, closed)| {
        direct.iter().zip(closed).all(|(called, reached)| *reached >= *called)
    });
    assert!(kept, "a direct call is a chain of one call");
    reaches
}

/// A text to search for in another.
#[derive(Clone, Copy)]
struct Search<'haystack> {
    /// The text searched.
    haystack: &'haystack str,
    /// The text searched for, which starts with an identifier.
    needle: &'haystack str,
}

/// Returns the offsets of the needle in the haystack where it starts a whole identifier.
fn occurrences<'haystack>(search: &Search<'haystack>) -> impl Iterator<Item = usize> + 'haystack {
    let Search { haystack, needle } = *search;
    assert!(!needle.is_empty(), "an empty needle matches everywhere");
    let first = needle.chars().next().is_some_and(is_identifier_character);
    assert!(first, "a needle starts with an identifier");
    haystack.match_indices(needle).map(|(offset, _)| offset).filter(move |&offset| {
        let before = haystack.get(..offset).and_then(|prefix| prefix.chars().next_back());
        before.is_none_or(|character| !is_identifier_character(character))
    })
}

/// Reports `while`, `loop`, `if !`, compound conditions and one-letter type parameters on every
/// code line, tests included.
fn check_rules_lines(path: &str, source: &Source<'_>) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (line_index, line) in source.lines.iter().enumerate() {
        let code = strip_literals(code_part(line));
        let words = check_rules_lines_words(&code);
        let unbounded = words.iter().any(|word| matches!(*word, "while" | "loop"));
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
        if check_rules_lines_compound(&code) {
            violations.push(Violation::new(
                path,
                line_index,
                "compound condition: no `&&` or `||`, nest `if`s or match on the cases".to_owned(),
            ));
        }
        if check_rules_lines_letter(&code) {
            violations.push(Violation::new(
                path,
                line_index,
                "one-letter type parameter: name it with a word, like `Item`".to_owned(),
            ));
        }
    }
    assert!(violations.len() <= source.lines.len() * 4, "four rules per line at most");
    assert!(violations.iter().all(|violation| violation.path == path), "one file at a time");
    violations
}

/// Returns `true` if `code` joins conditions: with `&&` or `||` between two operands, or by
/// asking a list of conditions whether it holds `true` or `false`.
fn check_rules_lines_compound(code: &str) -> bool {
    assert!(!code.contains('\n'), "one line at a time");
    let joins = |operator: &str| {
        code.match_indices(operator).any(|(offset, _)| {
            let before = code.get(..offset).map_or("", |before| before).trim_end();
            // An operator follows an operand. The `||` of a closure without parameters follows
            // `(`, `,`, `=` or `move`, and the `&&` of a double reference follows `:`, `<` or `(`.
            let after_name = before.chars().next_back().is_some_and(is_identifier_character);
            let after_operand =
                if after_name { true } else { before.ends_with([')', ']', '}', '"', '\'']) };
            let after_keyword = ["move", "return", "as", "in"].iter().any(|keyword| {
                occurrences(&Search { haystack: before, needle: keyword })
                    .any(|start| start + keyword.len() == before.len())
            });
            if after_operand { !after_keyword } else { false }
        })
    };
    let listed = ["contains(&true)", "contains(&false)"].iter().any(|list| code.contains(list));
    let compound = if joins("&&") { true } else { if joins("||") { true } else { listed } };
    if compound {
        assert!(!code.trim().is_empty(), "a compound condition is code");
    }
    compound
}

/// Returns `true` if `code` uses a single capital letter as an identifier, which only a type
/// parameter does: constants and types have longer names.
fn check_rules_lines_letter(code: &str) -> bool {
    assert!(!code.contains('\n'), "one line at a time");
    let characters: Vec<char> = format!(" {code} ").chars().collect();
    // A letter between quotes is a character literal, and one after a quote is a lifetime.
    let joins =
        |character: &char| matches!(*character, 'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '\'');
    let found = characters.windows(3).any(|window| match window {
        [before, 'A'..='Z', after] => [before, after].iter().all(|neighbor| !joins(neighbor)),
        _ => false,
    });
    if found {
        assert!(code.chars().any(|character| character.is_ascii_uppercase()), "a capital letter");
    }
    found
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
    let helpers = source
        .functions
        .iter()
        .filter(|function| function.visibility == Visibility::Private)
        .filter(|function| function.container != Container::Trait)
        .filter(|function| function.name != "main");
    let called_once = helpers.filter_map(|helper| {
        check_rules_helper_names_caller(source, helper).map(|caller| (helper, caller))
    });
    for (helper, caller) in called_once {
        let prefix = format!("{}_", caller.name);
        // A helper of a method may lead with the type's name too: `source_new_blocks`.
        let qualified = match caller.container {
            Container::Inherent => helper.name.contains(&format!("_{prefix}")),
            Container::Module | Container::Trait => false,
        };
        if helper.name.starts_with(&prefix) {
            assert!(helper.name.len() > prefix.len(), "a helper adds to its caller's name");
        } else {
            if qualified {
                assert!(helper.name.len() > prefix.len(), "a helper adds to its caller's name");
            } else {
                assert!(caller.name != helper.name, "a helper is not its own caller");
                let message = format!(
                    "`{}` is only called by `{}`: name it `{prefix}..`",
                    helper.name, caller.name
                );
                violations.push(Violation::new(path, helper.line_index_first, message));
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
        let search = Search { haystack: code_part(line), needle: helper.name };
        let uses = occurrences(&search).any(|offset| {
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

/// Reports functions with two arguments of one type, or an `Option` argument.
fn check_rules_parameters(path: &str, source: &Source<'_>) -> Vec<Violation> {
    let mut violations = Vec::new();
    let checked = source.functions.iter().filter(|function| function.container != Container::Trait);
    for function in checked {
        let types = check_rules_parameters_types(&function.signature);
        assert!(types.len() <= function.signature.len(), "a type takes at least one byte");
        let repeated = types
            .iter()
            .enumerate()
            .find(|&(position, kind)| types.iter().skip(position + 1).any(|other| other == kind));
        if let Some((_, kind)) = repeated {
            violations.push(Violation::new(
                path,
                function.line_index_first,
                format!("`{}` takes two `{kind}`: pass them in a struct", function.name),
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
        .filter(|function| function.container == Container::Module)
        .any(|function| function.name == "main");
    if has_main {
        let top_level = source
            .code()
            .iter()
            .enumerate()
            .filter(|&(_, line)| indentation(line) == 0)
            .filter(|&(_, line)| !line.trim().is_empty());
        let first_item = top_level.into_iter().find(|&(_, line)| {
            let preamble = ["//", "#", "use ", "mod ", "pub mod ", "pub use ", "}", ")", "    "]
                .iter()
                .any(|prefix| line.starts_with(prefix));
            !preamble
        });
        if let Some((line_index, line)) = first_item {
            let main_first = line.starts_with("fn main(");
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

    /// Returns the messages in `text` that contain `fragment`.
    fn messages_about(text: &str, fragment: &str) -> Vec<String> {
        messages(text).into_iter().filter(|message| message.contains(fragment)).collect()
    }

    /// Flags two free functions that call each other and two methods of one type that do, in
    /// separate `impl` blocks, and leaves alone methods of two types that share their names.
    #[test]
    fn recursion_through_other_functions_is_reported() {
        let free = "fn ping() {\n    pong();\n}\nfn pong() {\n    ping();\n}\n";
        assert_eq!(messages_about(free, "calls itself through others").len(), 2);
        let methods = concat!(
            "impl Tree {\n    fn len(&self) -> u32 {\n        self.count()\n    }\n}\n",
            "impl Tree {\n    fn count(&self) -> u32 {\n        self.len()\n    }\n}\n",
        );
        assert_eq!(messages_about(methods, "calls itself through others").len(), 2);
        let types = concat!(
            "impl Tree {\n    fn len(&self) -> u32 {\n        self.count()\n    }\n}\n",
            "impl Leaf {\n    fn count(&self) -> u32 {\n        self.len()\n    }\n}\n",
        );
        assert!(messages_about(types, "calls itself").is_empty(), "two types, no cycle");
        let chain = "fn first() {\n    second();\n}\nfn second() {}\n";
        assert!(messages_about(chain, "calls itself").is_empty(), "a chain is not a cycle");
    }

    /// Flags `&&` and `||` wherever they join conditions (an `if`, an assertion, a value, a
    /// wrapped line) and a list of conditions searched for `true`, and accepts what only looks
    /// like them: a closure without parameters and a reference to a reference.
    #[test]
    fn compound_conditions_are_reported() {
        assert_eq!(messages("fn check() {\n    if ready && open {}\n}\n").len(), 1);
        assert_eq!(messages("fn check() {\n    if ready || open {}\n}\n").len(), 1);
        let wrapped_condition = "fn check() {\n    if ready &&\n        open\n    {}\n}\n";
        assert_eq!(messages(wrapped_condition).len(), 1);
        assert_eq!(messages("fn check() {\n    assert!(ready && open, \"x\");\n}\n").len(), 1);
        assert_eq!(messages("fn check() {\n    assert!(ready || open, \"x\");\n}\n").len(), 1);
        assert_eq!(messages("fn check() {\n    let both = ready && open;\n}\n").len(), 1);
        let listed = "fn check() {\n    let either = [ready, open].contains(&true);\n}\n";
        assert_eq!(messages(listed).len(), 1);
        let closures = "fn check() {\n    run(|| ready());\n    spawn(move || open());\n}\n";
        assert!(messages(closures).is_empty(), "a closure without parameters");
        assert!(messages("fn check(byte: &&u8) {}\n").is_empty(), "a double reference");
        let nested = "fn check() {\n    if ready {\n        if open {}\n    }\n}\n";
        assert!(messages(nested).is_empty(), "nested conditions");
    }

    /// Flags a function of two statements without an assertion, and accepts the same function
    /// once it asserts, a function that is a single expression over several lines, and a
    /// function that never returns.
    #[test]
    fn functions_of_several_statements_assert() {
        let blind = "fn area(side: u32) -> u32 {\n    let area = side * side;\n    area\n}\n";
        let reported = check_blind_functions("test.rs", &Source::new(blind));
        assert_eq!(reported.len(), 1);
        assert!(reported[0].message.contains("several statements and no assertion"));
        let checked = concat!(
            "fn area(side: u32) -> u32 {\n    assert!(side > 0, \"x\");\n",
            "    let area = side * side;\n    area\n}\n",
        );
        assert!(check_blind_functions("test.rs", &Source::new(checked)).is_empty());
        let expression = concat!(
            "fn pick(side: bool) -> u32 {\n    match side {\n        true => 1,\n",
            "        false => 2,\n    }\n}\n",
        );
        assert!(check_blind_functions("test.rs", &Source::new(expression)).is_empty());
        let diverging = "fn stop(code: i32) -> ! {\n    let status = code;\n    exit(status)\n}\n";
        assert!(check_blind_functions("test.rs", &Source::new(diverging)).is_empty());
    }

    /// Flags a one-letter type parameter, and accepts a word and a character literal.
    #[test]
    fn one_letter_type_parameters_are_reported() {
        assert_eq!(messages("pub fn first<T>(items: &[T]) {}\n").len(), 1);
        assert!(messages("pub fn first<Item>(items: &[Item]) {}\n").is_empty());
        assert!(messages("pub fn letter() -> char {\n    'T'\n}\n").is_empty());
        assert!(messages("pub fn borrow<'a>(text: &'a str) {}\n").is_empty());
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

    /// Flags two arguments of one type, primitive or not, and an `Option` argument, and accepts
    /// two arguments of different types.
    #[test]
    fn parameters_that_can_be_mixed_up_are_reported() {
        assert_eq!(messages("pub fn span(low: u32, high: u32) {}\n").len(), 1);
        assert_eq!(messages("pub fn pick(choice: Option<u8>) {}\n").len(), 1);
        assert_eq!(messages("pub fn copy(source: &Path, target: &Path) {}\n").len(), 1);
        let distinct = "pub fn fill<Fill: Fn(u8) -> u8>(count: u32, fill: Fill) {}\n";
        assert!(messages(distinct).is_empty());
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
