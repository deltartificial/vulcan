//! Commit message rules.
//!
//! A pull request description is not stored in the repository and does not show in `git blame`,
//! so the commit message carries the explanation. Every commit on the branch has a title of the
//! form `type(scope): description` and a body, separated from the title by a blank line, that
//! says what changed and why.

use crate::report::Violation;

/// The commit types a title may start with.
pub(crate) const TYPES: &[&str] =
    &["feat", "fix", "chore", "test", "bench", "perf", "refactor", "docs", "ci", "revert", "deps"];

/// The longest title, in characters, so it fits in `git log --oneline` and in a review list.
pub(crate) const TITLE_LENGTH_MAX: u32 = 72;

/// The shortest body, in characters: enough for a sentence that says why.
pub(crate) const BODY_LENGTH_MIN: u32 = 20;

/// One commit, as `git` reports it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Commit<'text> {
    /// What names the commit in the report: its hash.
    pub(crate) name: &'text str,
    /// The whole message: the title, a blank line and the body.
    pub(crate) message: &'text str,
}

/// Returns the violations of the message of one commit.
pub(crate) fn check_message(commit: &Commit<'_>) -> Vec<Violation> {
    let Commit { name, message } = *commit;
    assert!(!name.is_empty(), "a commit has a name");
    let mut problems = Vec::new();
    let mut lines = message.lines();
    let title = lines.next().map_or("", str::trim_end);
    if let Some(problem) = check_message_title(title) {
        problems.push(problem);
    }
    let separator = lines.next().map_or("", str::trim);
    let body: String = lines.collect::<Vec<_>>().join("\n");
    let body_length = u32::try_from(body.trim().chars().count()).map_or(u32::MAX, |length| length);
    if separator.is_empty() {
        if body_length < BODY_LENGTH_MIN {
            problems
                .push(format!("add a body of at least {BODY_LENGTH_MIN} characters: what and why"));
        } else {
            assert!(body_length >= BODY_LENGTH_MIN, "the body is long enough");
        }
    } else {
        problems.push("separate the title from the body with a blank line".to_owned());
    }
    assert!(problems.len() <= 3, "a title, a separator and a body problem at most");
    problems.into_iter().map(|problem| Violation::new(name, 0, problem)).collect()
}

/// Returns what is wrong with a title, if anything.
fn check_message_title(title: &str) -> Option<String> {
    let title_length = u32::try_from(title.chars().count()).map_or(u32::MAX, |length| length);
    if title_length > TITLE_LENGTH_MAX {
        return Some(format!("shorten the title to {TITLE_LENGTH_MAX} characters"));
    }
    let Some((prefix, description)) = title.split_once(": ") else {
        return Some("write the title as `type(scope): description`".to_owned());
    };
    let kind = prefix.trim_end_matches('!').split('(').next().map_or("", |kind| kind);
    let scope_closed = prefix.contains('(') == prefix.trim_end_matches('!').ends_with(')');
    let problem = if TYPES.contains(&kind) {
        if scope_closed {
            description.trim().is_empty().then(|| "describe the change after the type".to_owned())
        } else {
            Some("close the scope with `)`".to_owned())
        }
    } else {
        Some(format!("start the title with one of: {}", TYPES.join(", ")))
    };
    if problem.is_none() {
        assert!(title_length > 0, "a valid title is not empty");
    }
    problem
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns the violations of `message`, as the message of a commit called `abc`.
    fn check(message: &str) -> Vec<Violation> {
        check_message(&Commit { name: "abc", message })
    }

    /// Accepts a conventional title with a body, with and without a scope and a breaking mark.
    #[test]
    fn conventional_messages_with_a_body_pass() {
        let body = "\n\nExplains what changed and why it had to.";
        for title in ["feat(sync): add a pool", "fix: handle empty input", "feat!: remove an API"] {
            assert!(check(&format!("{title}{body}")).is_empty(), "{title}");
        }
    }

    /// Flags an unknown type, a missing body, a missing blank line and an unclosed scope.
    #[test]
    fn broken_messages_are_reported() {
        assert_eq!(check("update stuff\n\nA body that is long enough.").len(), 1);
        assert_eq!(check("fix: handle empty input").len(), 1);
        assert_eq!(check("fix: a\nNo blank line before this body.").len(), 1);
        assert_eq!(check("fix(sync: a\n\nA body that is long enough.").len(), 1);
    }
}
