//! Helpers every framework shares: PHP source scanners, file and config
//! reading, and completion and hover building blocks. Frameworks import
//! from here and never from each other.

pub(in crate::php) mod complete;
pub(in crate::php) mod files;
pub(in crate::php) mod scan;
