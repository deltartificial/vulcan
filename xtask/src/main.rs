//! Project automation: `cargo xtask <task>`.
//!
//! Every build, lint and test entry point of the workspace lives here, in Rust, instead of in a
//! `Makefile` or shell scripts. One toolchain runs it on every platform, the compiler checks it,
//! and adding a task is adding a row to `TASKS`.

mod commits;
mod report;
mod rules;
mod source;
mod style;

use crate::{report::Violation, rules::Density, source::Source, style::Syntax};
use std::{
    env, fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    let task_name = env::args().nth(1);
    let outcome = match task_name.as_deref() {
        None | Some("help" | "--help") => main_print_help().map_err(Failure::Io),
        Some(name) => main_run_task(name),
    };
    let code = match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            // Reporting is best effort: the exit code already carries the failure.
            let _reported = writeln!(io::stderr(), "error: {}", main_describe(&failure));
            ExitCode::FAILURE
        }
    };
    assert!(!TASKS.is_empty(), "there is something to run");
    assert!(TASKS.iter().all(|task| !task.name.is_empty()), "every task has a name");
    code
}

/// A program run with fixed arguments and environment.
struct Invocation {
    /// The executable, looked up on `PATH`.
    program: &'static str,
    /// The arguments, in order.
    arguments: &'static [&'static str],
    /// Variables added to the inherited environment.
    environment: &'static [(&'static str, &'static str)],
}

/// What a task does.
enum Action {
    /// Runs one program.
    Run(Invocation),
    /// Runs other tasks in order and stops at the first failure. They must not be sequences
    /// themselves, which keeps the runner flat.
    Sequence(&'static [&'static str]),
    /// Runs the text and code rules of [`style`] and [`rules`].
    Style,
    /// Fails if a test-only crate is reachable from the normal dependency graph.
    NoTestDependencies,
    /// Builds every fuzz target and runs each for [`FUZZ_SECONDS`].
    Fuzz,
    /// Checks the commit messages of the branch with [`commits`].
    Commits,
    /// Copies the compatibility test vectors between the checkout and a temporary directory.
    Copy(Copy),
}

/// Which way [`Action::Copy`] goes.
#[derive(Clone, Copy)]
enum Direction {
    /// From the checkout to the temporary directory.
    Save,
    /// From the temporary directory to the checkout.
    Restore,
}

/// The settings of [`Action::Copy`].
#[derive(Clone, Copy)]
struct Copy {
    /// Which way the files go.
    direction: Direction,
}

/// A named entry point.
struct Task {
    /// The name typed after `cargo xtask`.
    name: &'static str,
    /// One sentence, shown by `cargo xtask help`.
    summary: &'static str,
    /// What the task does.
    action: Action,
}

/// Why a task failed.
#[derive(Debug)]
enum Failure {
    /// The task name is not in [`TASKS`].
    UnknownTask(String),
    /// A program could not be started, most often because it is not installed.
    Spawn {
        /// The program that did not start.
        program: &'static str,
        /// Why it did not start.
        error: io::Error,
    },
    /// A program ran and reported failure.
    Status {
        /// The program that failed.
        program: &'static str,
    },
    /// The checks found violations.
    Violations {
        /// How many.
        count: u32,
    },
    /// A test-only crate is a normal dependency.
    TestDependency(&'static str),
    /// A directory holds more entries than [`COPY_ENTRY_COUNT_MAX`].
    TooManyEntries,
    /// Reading files or writing the report failed.
    Io(io::Error),
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Builds a [`Action::Run`].
const fn run(
    program: &'static str,
    arguments: &'static [&'static str],
    environment: &'static [(&'static str, &'static str)],
) -> Action {
    assert!(!program.is_empty(), "a program has a name");
    assert!(arguments.len() < 32, "a command line stays short enough to read");
    Action::Run(Invocation { program, arguments, environment })
}

/// Rust flags of the native-CPU builds.
const NATIVE: &[(&str, &str)] = &[("RUSTFLAGS", "--codegen target-cpu=native")];

/// Rust flags of the nightly fast paths.
const NIGHTLY_CFG: &[(&str, &str)] = &[("RUSTFLAGS", "--cfg vulcan_nightly --deny warnings")];

/// Rustdoc flags of the documentation build.
const DOCS_FLAGS: &[(&str, &str)] = &[(
    "RUSTDOCFLAGS",
    "--cfg docsrs --show-type-layout --generate-link-to-definition --enable-index-page \
     -Zunstable-options --deny warnings --deny rustdoc::all",
)];

/// Every task, in the order `help` lists them.
const TASKS: &[Task] = &[
    Task {
        name: "build",
        summary: "Build the workspace.",
        action: run("cargo", &["build", "--workspace"], &[]),
    },
    Task {
        name: "build-release",
        summary: "Build in release mode.",
        action: run("cargo", &["build", "--workspace", "--profile", "release"], &[]),
    },
    Task {
        name: "profiling",
        summary: "Build with profiling symbols for the native CPU.",
        action: run("cargo", &["build", "--profile", "profiling"], NATIVE),
    },
    Task {
        name: "maxperf",
        summary: "Build with maximum optimization for the native CPU.",
        action: run("cargo", &["build", "--profile", "maxperf"], NATIVE),
    },
    Task {
        name: "maxperf-nightly",
        summary: "`maxperf` plus the nightly fast paths.",
        action: run(
            "cargo",
            &["+nightly", "build", "--profile", "maxperf"],
            &[("RUSTFLAGS", "--codegen target-cpu=native --cfg vulcan_nightly")],
        ),
    },
    Task {
        name: "fmt",
        summary: "Format Rust code (nightly rustfmt).",
        action: run("cargo", &["+nightly", "fmt", "--all"], &[]),
    },
    Task {
        name: "fmt-check",
        summary: "Check Rust formatting.",
        action: run("cargo", &["+nightly", "fmt", "--all", "--check"], &[]),
    },
    Task {
        name: "clippy",
        summary: "Run clippy on every target, warnings denied.",
        action: run(
            "cargo",
            &[
                "+nightly",
                "clippy",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--no-deps",
                "--",
                "--deny",
                "warnings",
            ],
            &[],
        ),
    },
    Task {
        name: "clippy-fix",
        summary: "Apply clippy's automatic fixes.",
        action: run(
            "cargo",
            &[
                "+nightly",
                "clippy",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--fix",
                "--allow-staged",
                "--allow-dirty",
                "--",
                "--deny",
                "warnings",
            ],
            &[],
        ),
    },
    Task {
        name: "clippy-nightly-cfg",
        summary: "Run clippy with the nightly fast paths.",
        action: run(
            "cargo",
            &["+nightly", "clippy", "--workspace", "--all-targets", "--all-features", "--no-deps"],
            NIGHTLY_CFG,
        ),
    },
    Task { name: "typos", summary: "Check spelling.", action: run("typos", &[], &[]) },
    Task {
        name: "toml",
        summary: "Format TOML, Markdown, JSON and YAML.",
        action: run("dprint", &["fmt"], &[]),
    },
    Task {
        name: "toml-check",
        summary: "Check TOML, Markdown, JSON and YAML formatting.",
        action: run("dprint", &["check"], &[]),
    },
    Task {
        name: "style",
        summary: "Check line width, comments, assertion density and the code rules.",
        action: Action::Style,
    },
    Task {
        name: "lint",
        summary: "Format, then run clippy, typos, dprint and the style checks.",
        action: Action::Sequence(&["fmt", "clippy", "typos", "toml", "style"]),
    },
    Task {
        name: "udeps",
        summary: "Find unused dependencies.",
        action: run("cargo", &["+nightly", "udeps", "--workspace", "--all-features"], &[]),
    },
    Task {
        name: "hack",
        summary: "Check the feature powerset.",
        action: run(
            "cargo",
            &["hack", "check", "--workspace", "--feature-powerset", "--depth", "2"],
            &[],
        ),
    },
    Task {
        name: "sort-check",
        summary: "Check that `Cargo.toml` tables are sorted.",
        action: run("cargo", &["sort", "--check", "--workspace"], &[]),
    },
    Task {
        name: "zepter",
        summary: "Check feature propagation.",
        action: run("zepter", &["run", "check"], &[]),
    },
    Task {
        name: "no-test-deps",
        summary: "Ensure test-only crates stay out of the normal graph.",
        action: Action::NoTestDependencies,
    },
    Task {
        name: "test-unit",
        summary: "Run unit and integration tests with nextest.",
        action: run(
            "cargo",
            &["nextest", "run", "--workspace", "--all-features", "--no-fail-fast"],
            &[],
        ),
    },
    Task {
        name: "test-doc",
        summary: "Run doctests.",
        action: run("cargo", &["test", "--doc", "--workspace", "--all-features"], &[]),
    },
    Task {
        name: "test",
        summary: "Run all tests.",
        action: Action::Sequence(&["test-unit", "test-doc"]),
    },
    Task {
        name: "coverage",
        summary: "Write an HTML coverage report (needs cargo-llvm-cov).",
        action: run("cargo", &["+nightly", "llvm-cov", "nextest", "--workspace", "--html"], &[]),
    },
    Task {
        name: "wasm",
        summary: "Check the build for wasm32-wasip1.",
        action: run("cargo", &["check", "--workspace", "--target", "wasm32-wasip1"], &[]),
    },
    Task {
        name: "deny",
        summary: "Check advisories, bans, licenses and sources.",
        action: run("cargo", &["deny", "--all-features", "check", "all"], &[]),
    },
    Task {
        name: "audit",
        summary: "Check dependencies for known vulnerabilities.",
        action: run("cargo", &["audit"], &[]),
    },
    Task {
        name: "docs",
        summary: "Build the documentation, warnings denied.",
        action: run(
            "cargo",
            &[
                "+nightly",
                "doc",
                "--workspace",
                "--all-features",
                "--no-deps",
                "--document-private-items",
            ],
            DOCS_FLAGS,
        ),
    },
    Task {
        name: "miri",
        summary: "Run the tests under Miri, which detects undefined behavior.",
        action: run("cargo", &["+nightly", "miri", "test", "--workspace"], &[]),
    },
    Task {
        name: "fuzz",
        summary: "Build every fuzz target and run each one briefly.",
        action: Action::Fuzz,
    },
    Task {
        name: "commits",
        summary: "Check the messages of the commits since `origin/main`.",
        action: Action::Commits,
    },
    Task {
        name: "compat-save",
        summary: "Save `test-vectors` outside the checkout, before switching branches.",
        action: Action::Copy(Copy { direction: Direction::Save }),
    },
    Task {
        name: "compat-restore",
        summary: "Restore the saved `test-vectors` into the checkout.",
        action: Action::Copy(Copy { direction: Direction::Restore }),
    },
    Task {
        name: "bench",
        summary: "Run the benchmarks.",
        action: run("cargo", &["bench", "--workspace"], &[]),
    },
    Task {
        name: "pr",
        summary: "Run every check a pull request needs.",
        action: Action::Sequence(&[
            "deny",
            "fmt",
            "clippy",
            "typos",
            "toml-check",
            "style",
            "udeps",
            "hack",
            "zepter",
            "no-test-deps",
            "test-unit",
            "test-doc",
            "docs",
            "commits",
        ]),
    },
    Task {
        name: "fix",
        summary: "Apply clippy fixes, then format.",
        action: Action::Sequence(&["clippy-fix", "fmt"]),
    },
    Task {
        name: "clean",
        summary: "Remove build artifacts.",
        action: run("cargo", &["clean"], &[]),
    },
];

/// Crates that may only appear as dev-dependencies.
const TEST_ONLY_CRATES: &[&str] = &["arbitrary", "proptest", "quickcheck"];

/// How long each fuzz target runs in [`Action::Fuzz`], in seconds: a smoke run, not a campaign.
const FUZZ_SECONDS: &str = "-max_total_time=15";

/// The directory of the fuzz targets.
const FUZZ_DIRECTORY: &str = "fuzz";

/// The upper bound on the number of fuzz targets.
const FUZZ_TARGET_COUNT_MAX: usize = 256;

/// The branch whose commits are already checked.
const COMMITS_BASE: &str = "origin/main";

/// The upper bound on the number of commits of one branch.
const COMMIT_COUNT_MAX: usize = 1024;

/// The directory of the compatibility test vectors in the checkout.
const VECTORS_DIRECTORY: &str = "test-vectors";

/// The upper bound on the files and directories that [`Action::Copy`] visits.
const COPY_ENTRY_COUNT_MAX: usize = 100_000;

/// The directories whose code counts towards the assertion density.
const DENSITY_DIRECTORIES: &[&str] = &["vulcan/src/", "xtask/src/"];

/// Returns the task called `name`.
fn main_run_task_find(name: &str) -> Result<&'static Task, Failure> {
    assert!(!name.is_empty(), "a task has a name");
    let task = TASKS.iter().find(|task| task.name == name);
    if let Some(found) = task {
        assert_eq!(found.name, name, "the task found has the name asked for");
    }
    task.ok_or_else(|| Failure::UnknownTask(name.to_owned()))
}

/// Runs the task `name`; a sequence runs its steps, which are leaf tasks.
fn main_run_task(name: &str) -> Result<(), Failure> {
    assert!(!name.is_empty(), "a task has a name");
    let task = main_run_task_find(name)?;
    match &task.action {
        Action::Sequence(steps) => {
            assert!(!steps.is_empty(), "a sequence has steps");
            for step in *steps {
                main_run_task_leaf(main_run_task_find(step)?)?;
            }
            Ok(())
        }
        Action::Run(_) |
        Action::Style |
        Action::NoTestDependencies |
        Action::Fuzz |
        Action::Commits |
        Action::Copy(_) => main_run_task_leaf(task),
    }
}

/// Runs a task that is not a sequence.
fn main_run_task_leaf(task: &Task) -> Result<(), Failure> {
    assert!(!task.name.is_empty(), "a task has a name");
    match &task.action {
        Action::Run(invocation) => main_run_task_leaf_invocation(invocation),
        Action::Style => main_run_task_leaf_style(),
        Action::NoTestDependencies => main_run_task_leaf_test_dependencies(),
        Action::Fuzz => main_run_task_leaf_fuzz(),
        Action::Commits => main_run_task_leaf_commits(),
        Action::Copy(copy) => main_run_task_leaf_copy(*copy),
        Action::Sequence(_) => unreachable!("sequence `{}` used as a step", task.name),
    }
}

/// Runs one program and waits for it.
fn main_run_task_leaf_invocation(invocation: &Invocation) -> Result<(), Failure> {
    let program = invocation.program;
    let arguments_named = invocation.arguments.iter().all(|argument| !argument.is_empty());
    assert!(arguments_named, "no empty argument");
    assert!(!program.is_empty(), "a program has a name");
    writeln!(io::stderr(), "$ {program} {}", invocation.arguments.join(" "))?;
    let status = Command::new(program)
        .args(invocation.arguments)
        .envs(invocation.environment.iter().copied())
        .status()
        .map_err(|error| Failure::Spawn { program, error })?;
    if status.success() { Ok(()) } else { Err(Failure::Status { program }) }
}

/// Runs a program and returns its standard output.
fn run_output(program: &'static str, arguments: &[&str]) -> Result<String, Failure> {
    assert!(!program.is_empty(), "a program has a name");
    assert!(arguments.iter().all(|argument| !argument.is_empty()), "no empty argument");
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| Failure::Spawn { program, error })?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(text.len() <= output.stdout.len() * 3, "lossy decoding at most triples bytes");
        Ok(text)
    } else {
        Err(Failure::Status { program })
    }
}

/// Runs every text and code rule on the files tracked by git.
fn main_run_task_leaf_style() -> Result<(), Failure> {
    let listing = run_output("git", &["ls-files", "--cached", "--others", "--exclude-standard"])?;
    assert!(!listing.is_empty(), "git tracks files");
    let mut violations = Vec::new();
    let mut density = Density::ZERO;
    for path in listing.lines() {
        let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
        if let Some(syntax) = Syntax::from_extension(extension) {
            let report = main_run_task_leaf_style_file(path, syntax)?;
            violations.extend(report.violations);
            density = density.add(report.density);
        }
    }
    assert!(density.function_count > 0, "the density directories hold code");
    main_run_task_leaf_style_report(&violations, density)
}

/// The violations of one file, and its density.
struct FileReport {
    /// Every rule broken in the file.
    violations: Vec<Violation>,
    /// The functions and assertions of the file, or zero outside [`DENSITY_DIRECTORIES`].
    density: Density,
}

/// Checks one file, and counts its density if it is in [`DENSITY_DIRECTORIES`].
fn main_run_task_leaf_style_file(path: &str, syntax: Syntax) -> Result<FileReport, Failure> {
    let text = fs::read_to_string(path)?;
    let mut violations = style::check_text(path, &text, syntax);
    let density = match syntax {
        Syntax::Rust => {
            let source = Source::new(&text);
            violations.extend(rules::check_rules(path, &source));
            let counted = DENSITY_DIRECTORIES.iter().any(|directory| path.starts_with(directory));
            if counted { rules::count_density(&source) } else { Density::ZERO }
        }
        Syntax::Hash | Syntax::Plain => Density::ZERO,
    };
    assert!(violations.iter().all(|violation| violation.path == path), "one file at a time");
    if density.is_dense_enough() {
        assert!(density.function_count <= density.assertion_count, "dense code asserts");
    } else {
        // Not a violation by itself, since the rule is on the average, but it shows where to work.
        writeln!(
            io::stderr(),
            "{path}: {} assertions for {} functions",
            density.assertion_count,
            density.function_count
        )?;
    }
    Ok(FileReport { violations, density })
}

/// Prints the violations and the density, and fails if either breaks a rule.
fn main_run_task_leaf_style_report(
    violations: &[Violation],
    density: Density,
) -> Result<(), Failure> {
    assert!(violations.iter().all(|violation| violation.line_number > 0), "lines count from one");
    let mut stderr = io::stderr().lock();
    for violation in violations {
        writeln!(stderr, "{violation}")?;
    }
    let hundredths = density.hundredths_per_function();
    let whole = hundredths.checked_div(100).map_or(0, |value| value);
    let fraction = hundredths.checked_rem(100).map_or(0, |value| value);
    if density.function_count > 0 {
        writeln!(
            stderr,
            "assertions: {} for {} functions ({whole}.{fraction:02} each, minimum {})",
            density.assertion_count,
            density.function_count,
            rules::ASSERTION_DENSITY_MIN,
        )?;
    } else {
        // Nothing was counted, as for commit messages: no density to report.
    }
    let dense_enough = density.is_dense_enough();
    let count = rules::count_from_usize(violations.len()) + u32::from(!dense_enough);
    if count == 0 {
        assert!(violations.is_empty(), "no violation is reported");
        Ok(())
    } else {
        Err(Failure::Violations { count })
    }
}

/// Fails if a crate of [`TEST_ONLY_CRATES`] is a normal or build dependency.
fn main_run_task_leaf_test_dependencies() -> Result<(), Failure> {
    assert!(!TEST_ONLY_CRATES.is_empty(), "there is something to look for");
    let arguments = [
        "tree",
        "--workspace",
        "--edges",
        "normal,build",
        "--no-default-features",
        "--prefix",
        "none",
    ];
    let tree = run_output("cargo", &arguments)?;
    let found = TEST_ONLY_CRATES
        .iter()
        .find(|name| tree.lines().any(|line| line.split_whitespace().next() == Some(**name)));
    assert!(!tree.is_empty(), "the workspace has at least one crate");
    found.map_or(Ok(()), |name| Err(Failure::TestDependency(name)))
}

/// Builds every fuzz target, then runs each one for [`FUZZ_SECONDS`].
fn main_run_task_leaf_fuzz() -> Result<(), Failure> {
    let listing = run_output("cargo", &["+nightly", "fuzz", "list", "--fuzz-dir", FUZZ_DIRECTORY])?;
    let targets: Vec<&str> =
        listing.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    assert!(!targets.is_empty(), "the fuzz directory has targets");
    assert!(targets.len() <= FUZZ_TARGET_COUNT_MAX, "the fuzz targets fit the bound");
    for target in targets {
        let arguments =
            ["+nightly", "fuzz", "run", "--fuzz-dir", FUZZ_DIRECTORY, target, "--", FUZZ_SECONDS];
        writeln!(io::stderr(), "$ cargo {}", arguments.join(" "))?;
        let status = Command::new("cargo")
            .args(arguments)
            .status()
            .map_err(|error| Failure::Spawn { program: "cargo", error })?;
        if status.success() {
            assert!(status.code() == Some(0_i32), "a success exits with zero");
        } else {
            return Err(Failure::Status { program: "cargo fuzz" });
        }
    }
    Ok(())
}

/// Checks the message of every commit between [`COMMITS_BASE`] and `HEAD`.
fn main_run_task_leaf_commits() -> Result<(), Failure> {
    let range = format!("{COMMITS_BASE}..HEAD");
    let listing = run_output("git", &["rev-list", "--no-merges", &range])?;
    let hashes: Vec<&str> = listing.lines().collect();
    assert!(hashes.len() <= COMMIT_COUNT_MAX, "the branch fits the bound");
    let full = hashes.iter().all(|hash| hash.len() == 40 || hash.len() == 64);
    assert!(full, "`rev-list` prints full hashes");
    let mut violations = Vec::new();
    for hash in hashes {
        let message = run_output("git", &["log", "--max-count=1", "--format=%B", hash])?;
        violations.extend(commits::check_message(hash, &message));
    }
    main_run_task_leaf_style_report(&violations, Density::ZERO)
}

/// Copies [`VECTORS_DIRECTORY`] to or from the temporary directory, if it exists.
fn main_run_task_leaf_copy(copy: Copy) -> Result<(), Failure> {
    let saved = env::temp_dir().join("vulcan-compat-vectors");
    let checkout = PathBuf::from(VECTORS_DIRECTORY);
    let (source, target) = match copy.direction {
        Direction::Save => (checkout, saved),
        Direction::Restore => (saved, checkout),
    };
    assert!(source != target, "a copy goes somewhere else");
    if source.is_dir() {
        main_run_task_leaf_copy_tree(&source, &target)?;
        assert!(target.is_dir(), "the copy created its target");
    } else {
        writeln!(io::stderr(), "nothing to copy: `{}` does not exist", source.display())?;
    }
    Ok(())
}

/// Copies the directory tree at `source` to `target`, with an explicit stack instead of
/// recursion, visiting at most [`COPY_ENTRY_COUNT_MAX`] entries.
fn main_run_task_leaf_copy_tree(source: &Path, target: &Path) -> Result<(), Failure> {
    assert!(source.is_dir(), "the source is a directory");
    assert!(source != target, "a tree is not copied onto itself");
    let mut pending = vec![(source.to_path_buf(), target.to_path_buf())];
    for _ in 0..COPY_ENTRY_COUNT_MAX {
        let Some((source_directory, target_directory)) = pending.pop() else {
            return Ok(());
        };
        fs::create_dir_all(&target_directory)?;
        for entry_read in fs::read_dir(&source_directory)? {
            let entry = entry_read?;
            let target_path = target_directory.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                pending.push((entry.path(), target_path));
            } else {
                let _bytes = fs::copy(entry.path(), target_path)?;
            }
        }
    }
    Err(Failure::TooManyEntries)
}

/// Returns the message that explains `failure`.
fn main_describe(failure: &Failure) -> String {
    let message = match failure {
        Failure::UnknownTask(name) => format!("unknown task `{name}`; run `cargo xtask help`"),
        Failure::Spawn { program, error } => {
            format!("could not run `{program}` ({error}); is it installed?")
        }
        Failure::Status { program } => format!("`{program}` failed"),
        Failure::Violations { count } => format!("{count} violations"),
        Failure::TestDependency(name) => {
            format!("test-only crate `{name}` is reachable from the normal dependency graph")
        }
        Failure::TooManyEntries => {
            format!("more than {COPY_ENTRY_COUNT_MAX} entries to copy")
        }
        Failure::Io(error) => format!("I/O error: {error}"),
    };
    assert!(!message.is_empty(), "every failure is explained");
    let continues = message.chars().next().is_some_and(|first| !first.is_uppercase());
    assert!(continues, "the message continues the `error: ` prefix");
    message
}

/// Lists every task with its summary.
fn main_print_help() -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    assert!(!TASKS.is_empty(), "there are tasks to list");
    writeln!(stderr, "Usage: cargo xtask <task>\n\nTasks:")?;
    for task in TASKS {
        assert!(task.summary.ends_with('.'), "a summary is a sentence");
        writeln!(stderr, "  {:<20} {}", task.name, task.summary)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compares every pair of tasks, so no name is defined twice.
    #[test]
    fn task_names_are_unique() {
        for (position, task) in TASKS.iter().enumerate() {
            let duplicate = TASKS[position + 1..].iter().any(|other| other.name == task.name);
            assert!(!duplicate, "task `{}` is defined twice", task.name);
        }
    }

    /// Returns the steps of a sequence, or nothing for a leaf task.
    const fn sequence_steps(task: &Task) -> &'static [&'static str] {
        if let Action::Sequence(steps) = task.action { steps } else { &[] }
    }

    /// Resolves every step of every sequence, which must be an existing task that is not itself a
    /// sequence.
    #[test]
    fn sequences_name_existing_leaf_tasks() {
        let pairs = TASKS
            .iter()
            .flat_map(|task| sequence_steps(task).iter().map(move |step| (task.name, *step)));
        for (sequence, step) in pairs {
            let found = main_run_task_find(step);
            assert!(found.is_ok(), "`{sequence}` names unknown task `{step}`");
            let nested = found.is_ok_and(|task| !sequence_steps(task).is_empty());
            assert!(!nested, "`{sequence}` nests sequence `{step}`");
        }
    }

    /// Runs a task name that does not exist and expects the dedicated failure.
    #[test]
    fn unknown_tasks_are_reported() {
        let failure = main_run_task("no-such-task").err();
        assert!(matches!(failure, Some(Failure::UnknownTask(name)) if name == "no-such-task"));
    }

    /// Checks that every summary is a sentence, since `help` prints them as such.
    #[test]
    fn summaries_are_sentences() {
        for task in TASKS {
            assert!(task.summary.ends_with('.'), "summary of `{}` ends with a period", task.name);
        }
    }

    /// Copies a two-level tree with the explicit stack and finds every file at its new place.
    #[test]
    #[cfg_attr(miri, ignore = "Miri isolates the test from the file system")]
    #[expect(clippy::absolute_paths, reason = "one use, in a test")]
    fn copy_tree_copies_nested_directories() {
        let root = env::temp_dir().join(format!("xtask-copy-{}", std::process::id()));
        let source = root.join("source");
        let target = root.join("target");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("top.bin"), b"top").unwrap();
        fs::write(source.join("nested").join("deep.bin"), b"deep").unwrap();
        main_run_task_leaf_copy_tree(&source, &target).unwrap();
        assert_eq!(fs::read(target.join("nested").join("deep.bin")).unwrap(), b"deep");
        assert_eq!(fs::read(target.join("top.bin")).unwrap(), b"top");
        fs::remove_dir_all(root).unwrap();
    }
}
