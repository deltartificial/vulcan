//! Vulcan minimal example package.
//!
//! This is a placeholder package to demonstrate the workspace configuration
//! and ensure CI workflows execute successfully.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod alloc;

/// Adds two numbers together.
///
/// # Examples
///
/// ```
/// use vulcan::add;
///
/// assert_eq!(add(2, 3), 5);
/// ```
#[must_use]
pub const fn add(left: u64, right: u64) -> u64 {
    left + right
}

/// Multiplies two numbers.
///
/// # Examples
///
/// ```
/// use vulcan::multiply;
///
/// assert_eq!(multiply(4, 5), 20);
/// ```
#[must_use]
pub const fn multiply(left: u64, right: u64) -> u64 {
    left * right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(2, 2), 4);
        assert_eq!(add(0, 0), 0);
        assert_eq!(add(1, 1), 2);
    }

    #[test]
    fn test_multiply() {
        assert_eq!(multiply(2, 3), 6);
        assert_eq!(multiply(0, 5), 0);
        assert_eq!(multiply(1, 1), 1);
    }
}
