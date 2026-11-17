//! A rule broken at one place, as the checks report it.

use core::fmt;

/// One rule broken at one line of one file.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Violation {
    /// The file, relative to the repository root.
    pub(crate) path: String,
    /// The line, counted from one as editors do.
    pub(crate) line_number: u32,
    /// What is wrong, and what to do instead.
    pub(crate) message: String,
}

impl Violation {
    /// Creates a violation at the 0-based `line_index`, which is reported 1-based.
    pub(crate) fn new(path: &str, line_index: usize, message: String) -> Self {
        assert!(!path.is_empty(), "a violation names its file");
        assert!(!message.is_empty(), "a violation says what is wrong");
        let line_number = u32::try_from(line_index + 1).map_or(u32::MAX, |number| number);
        assert!(line_number > 0, "line numbers count from one");
        Self { path: path.to_owned(), line_number, message }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        assert!(self.line_number > 0, "line numbers count from one");
        write!(formatter, "{}:{}: {}", self.path, self.line_number, self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Turns the first line index into line number one, the way an editor shows it.
    #[test]
    fn line_numbers_count_from_one() {
        let violation = Violation::new("a.rs", 0, "wrong".to_owned());
        assert_eq!(violation.line_number, 1);
        assert_eq!(violation.to_string(), "a.rs:1: wrong");
    }

    /// Refuses a violation that does not say what is wrong.
    #[test]
    #[should_panic(expected = "says what is wrong")]
    fn violations_need_a_message() {
        let _violation = Violation::new("a.rs", 0, String::new());
    }
}
