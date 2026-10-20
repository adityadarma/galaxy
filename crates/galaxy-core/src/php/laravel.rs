//! Laravel knowledge for the built-in PHP engine: the strings of a project
//! that name config keys, routes, views and the rest, and what they point
//! to. Everything is read from the project's files; PHP never runs.

pub mod index;
pub mod site;

pub use index::{Index, Item, Loc, Table};
pub use site::{Kind, Site, in_string, site_at, sites};
