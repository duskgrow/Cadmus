//! The Agent Skills format (ADR-0006), pure-logic half: SKILL.md frontmatter
//! parsing and validation. Filesystem discovery lives in the `cadmus` crate
//! (the wiring side); the catalog render lives in [`crate::context`].

mod frontmatter;

pub use frontmatter::{FrontmatterIssue, parse_frontmatter, split_frontmatter};
