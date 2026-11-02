//! Bounded recursion with a depth counter.
//!
//! A recursive descent over untrusted input (a parser, a tree walk) must not overflow the stack.
//! Counting depth is cheaper and more predictable than growing the stack on demand: an increment
//! and a comparison per level, with the error path kept out of line.
//!
//! ```
//! use vulcan::recursion::{Depth, Recurse, RecursionLimitReached};
//!
//! struct Parser {
//!     depth: Depth,
//! }
//!
//! impl Recurse for Parser {
//!     fn depth_mut(&mut self) -> &mut Depth {
//!         &mut self.depth
//!     }
//! }
//!
//! fn nest(p: &mut Parser, levels: u32) -> Result<u32, RecursionLimitReached> {
//!     if levels == 0 {
//!         return Ok(0);
//!     }
//!     p.recurse(|inner| nest(inner, levels - 1))?.map(|depth| depth + 1)
//! }
//!
//! let mut parser = Parser { depth: Depth::new(16) };
//! assert_eq!(nest(&mut parser, 10), Ok(10));
//! assert!(nest(&mut parser, 100).is_err());
//! ```

use std::fmt;

/// The current recursion depth and its limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Depth {
    current: u32,
    limit: u32,
}

impl Depth {
    /// Default limit, as used by Solidity tooling for nested expressions and statements.
    pub const DEFAULT_LIMIT: u32 = 256;

    /// Creates a counter that allows `limit` nested levels.
    #[must_use]
    pub const fn new(limit: u32) -> Self {
        Self { current: 0, limit }
    }

    /// Returns the current depth.
    #[must_use]
    pub const fn current(&self) -> u32 {
        self.current
    }

    /// Returns the limit.
    #[must_use]
    pub const fn limit(&self) -> u32 {
        self.limit
    }
}

impl Default for Depth {
    fn default() -> Self {
        Self::new(Self::DEFAULT_LIMIT)
    }
}

/// The recursion limit was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecursionLimitReached {
    /// The limit that was exceeded.
    pub limit: u32,
}

impl fmt::Display for RecursionLimitReached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "recursion limit of {} reached", self.limit)
    }
}

impl std::error::Error for RecursionLimitReached {}

/// Recursion with a depth limit, for any context that owns a [`Depth`].
pub trait Recurse: Sized {
    /// Returns the depth counter of this context.
    fn depth_mut(&mut self) -> &mut Depth;

    /// Runs `f` one level deeper, or fails once the limit is reached.
    ///
    /// The depth is restored after `f` returns. If `f` panics, the context is in an unknown state
    /// anyway and must not be reused.
    #[inline]
    fn recurse<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> Result<R, RecursionLimitReached> {
        let depth = self.depth_mut();
        if depth.current >= depth.limit {
            return Err(limit_reached(depth.limit));
        }
        depth.current += 1;
        let result = f(self);
        self.depth_mut().current -= 1;
        Ok(result)
    }
}

#[cold]
#[inline(never)]
const fn limit_reached(limit: u32) -> RecursionLimitReached {
    RecursionLimitReached { limit }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Walker(Depth);

    impl Recurse for Walker {
        fn depth_mut(&mut self) -> &mut Depth {
            &mut self.0
        }
    }

    fn descend(w: &mut Walker, n: u32) -> Result<(), RecursionLimitReached> {
        if n == 0 { Ok(()) } else { w.recurse(|inner| descend(inner, n - 1))? }
    }

    #[test]
    fn limit_is_enforced_and_depth_restored() {
        let mut walker = Walker(Depth::new(3));
        assert_eq!(descend(&mut walker, 3), Ok(()));
        assert_eq!(walker.0.current(), 0);
        assert_eq!(descend(&mut walker, 4), Err(RecursionLimitReached { limit: 3 }));
        assert_eq!(walker.0.current(), 0, "depth restored after an error");
        assert_eq!(RecursionLimitReached { limit: 3 }.to_string(), "recursion limit of 3 reached");
    }
}
