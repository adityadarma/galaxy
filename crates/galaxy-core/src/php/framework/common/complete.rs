//! Completion and hover helpers shared by every framework: name lookup in
//! an index, completion lists, hover text and jump targets.

use std::path::Path;
use std::sync::Arc;

use super::files::{Item, Loc};
use crate::php::engine::{Completion, ItemKind, PhpEngine, Target, fuzzy, rank};
use crate::php::parse::{ClassInfo, MemberKind};

const MAX_ITEMS: usize = 200;

/// What a name resolves to.
pub(in crate::php) struct Found {
    pub(in crate::php) detail: String,
    pub(in crate::php) loc: Option<Loc>,
    pub(in crate::php) class: Option<String>,
}

pub(in crate::php) fn found(item: &Item) -> Found {
    Found {
        detail: item.detail.clone(),
        loc: item.loc.clone(),
        class: item.class.clone(),
    }
}

/// Completions for the items whose name fits `typed`.
pub(in crate::php) fn named(
    typed: &str,
    items: &[Item],
    kind: ItemKind,
    from: u32,
) -> Vec<Completion> {
    let mut out: Vec<Completion> = items
        .iter()
        .filter(|item| fuzzy(typed, &item.name))
        .map(|item| Completion {
            label: item.name.clone(),
            detail: (!item.detail.is_empty()).then(|| item.detail.clone()),
            insert_text: item.name.clone(),
            is_snippet: false,
            kind,
            sort: rank(typed, &item.name, '0'),
            extra: Vec::new(),
            replace_from: Some(from),
        })
        .collect();
    out.sort_by(|a, b| a.sort.cmp(&b.sort));
    out.dedup_by(|a, b| a.label == b.label);
    out.truncate(MAX_ITEMS);
    out
}

pub(in crate::php) fn pick<'a>(items: &'a [Item], name: &str) -> Option<&'a Item> {
    items
        .iter()
        .filter(|item| item.name == name)
        .max_by_key(|item| item.class.is_some())
}

pub(in crate::php) fn target_of(loc: &Loc) -> Target {
    Target {
        path: loc.path.clone(),
        line: loc.line,
        column: loc.column,
    }
}

pub(in crate::php) fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Markdown for a hover: what it is, its value and where it is defined.
pub(in crate::php) fn describe(
    what: &str,
    name: &str,
    hit: &Found,
    plain: bool,
    root: &Path,
) -> String {
    let mut out = format!("**{what}** `{name}`");
    if !hit.detail.is_empty() {
        if plain {
            out.push_str(&format!("\n\n{}", hit.detail));
        } else {
            out.push_str(&format!("\n\n`{}`", hit.detail.replace('`', "'")));
        }
    }
    if let Some(class) = &hit.class {
        out.push_str(&format!("\n\n`{class}`"));
    }
    if let Some(loc) = &hit.loc {
        let file = relative(root, &loc.path);
        if loc.line == 0 && loc.column == 0 {
            out.push_str(&format!("\n\nDefined in `{file}`"));
        } else {
            out.push_str(&format!("\n\nDefined in `{file}:{}`", loc.line + 1));
        }
    }
    out
}

impl PhpEngine {
    /// Public methods of a controller written in the project itself.
    pub(in crate::php) fn fully_known(&mut self, class: &Arc<ClassInfo>) -> bool {
        for owner in self.lineage(class) {
            let parents: Vec<String> = owner
                .extends
                .iter()
                .chain(&owner.traits)
                .chain(&owner.implements)
                .cloned()
                .collect();
            if parents.iter().any(|p| self.class(p).is_none()) {
                return false;
            }
        }
        true
    }
    pub(in crate::php) fn action_methods(
        &mut self,
        class: &Arc<ClassInfo>,
        vendor: bool,
    ) -> Vec<Item> {
        let mut out: Vec<Item> = Vec::new();
        for owner in self.lineage(class).into_iter().take(8) {
            if !vendor && owner.path.to_string_lossy().contains("/vendor/") {
                continue;
            }
            for member in &owner.members {
                if member.kind == MemberKind::Method
                    && member.visibility == crate::php::parse::Visibility::Public
                    && !member.is_static
                    && !member.name.starts_with("__")
                    && !out.iter().any(|i| i.name == member.name)
                {
                    out.push(Item {
                        name: member.name.clone(),
                        detail: member.detail.clone(),
                        loc: Some(Loc {
                            path: owner.path.clone(),
                            line: member.line,
                            column: member.column,
                        }),
                        class: None,
                    });
                }
            }
        }
        out
    }
}
