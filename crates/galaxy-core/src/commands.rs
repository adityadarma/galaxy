//! Command registry and command palette search (M2-09).
//!
//! Every user-facing action is registered once with a stable id
//! (`workbench.action.quickOpen`, VSCode-style), a category and a title.
//! The palette shows `Category: Title`; the keymap (M2-10) will bind keys
//! to the same ids.
//!
//! The registry only knows what commands exist. Running one, and deciding
//! whether it is enabled right now, is the UI's job: the palette passes
//! the ids that are currently available to [`CommandRegistry::search`].

use std::collections::{HashMap, HashSet};

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::file_finder::highlights;

/// Recently run commands remembered for the palette.
pub const MAX_RECENT: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub id: String,
    /// Grouping shown before the title, e.g. `File`, `View`. May be empty.
    pub category: String,
    pub title: String,
    /// Extra words that should find it, not shown (e.g. "close" for
    /// "Hide Sidebar").
    pub keywords: Vec<String>,
}

impl Command {
    /// `Category: Title`, or just the title.
    pub fn label(&self) -> String {
        if self.category.is_empty() {
            self.title.clone()
        } else {
            format!("{}: {}", self.category, self.title)
        }
    }
}

/// One palette row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMatch {
    pub id: String,
    pub label: String,
    /// Matched characters as `(start, length)` UTF-16 ranges in `label`.
    pub highlights: Vec<(usize, usize)>,
    pub is_recent: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RegistryError {
    EmptyId,
    Duplicate(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::EmptyId => write!(f, "command id is empty"),
            RegistryError::Duplicate(id) => write!(f, "command {id:?} is already registered"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// All commands, in registration order.
pub struct CommandRegistry {
    commands: Vec<Command>,
    by_id: HashMap<String, usize>,
    /// Most recent first.
    recent: Vec<String>,
    matcher: Matcher,
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
            by_id: HashMap::new(),
            recent: Vec::new(),
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    pub fn register(&mut self, command: Command) -> Result<(), RegistryError> {
        if command.id.trim().is_empty() {
            return Err(RegistryError::EmptyId);
        }
        if self.by_id.contains_key(&command.id) {
            return Err(RegistryError::Duplicate(command.id));
        }
        self.by_id.insert(command.id.clone(), self.commands.len());
        self.commands.push(command);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Command> {
        self.by_id.get(id).map(|&i| &self.commands[i])
    }

    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// Notes that `id` was run, so it is listed first next time. Unknown
    /// ids are ignored.
    pub fn note_used(&mut self, id: &str) {
        if !self.by_id.contains_key(id) {
            return;
        }
        self.recent.retain(|r| r != id);
        self.recent.insert(0, id.to_owned());
        self.recent.truncate(MAX_RECENT);
    }

    pub fn recent(&self) -> &[String] {
        &self.recent
    }

    /// Commands matching `query` among `available` ids (all if `None`).
    ///
    /// Empty query: recently used commands first, then the rest
    /// alphabetically by label. Otherwise fuzzy matched on the label, or
    /// failing that on the keywords; recent commands get a bonus, and
    /// ties go to the shorter label, then alphabetical order.
    pub fn search(
        &mut self,
        query: &str,
        available: Option<&HashSet<String>>,
    ) -> Vec<CommandMatch> {
        let recent_rank: HashMap<&str, usize> = self
            .recent
            .iter()
            .enumerate()
            .map(|(rank, id)| (id.as_str(), rank))
            .collect();
        let candidates = self
            .commands
            .iter()
            .filter(|c| available.is_none_or(|ids| ids.contains(&c.id)));

        let query = query.trim();
        if query.is_empty() {
            let mut rows: Vec<(Option<usize>, String, &Command)> = candidates
                .map(|c| (recent_rank.get(c.id.as_str()).copied(), c.label(), c))
                .collect();
            rows.sort_by(|a, b| match (a.0, b.0) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.1.to_lowercase().cmp(&b.1.to_lowercase()),
            });
            return rows
                .into_iter()
                .map(|(rank, label, c)| CommandMatch {
                    id: c.id.clone(),
                    label,
                    highlights: Vec::new(),
                    is_recent: rank.is_some(),
                })
                .collect();
        }

        let pattern = Pattern::new(
            query,
            CaseMatching::Smart,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, String, &Command, bool)> = Vec::new();
        for c in candidates {
            let label = c.label();
            let on_label = pattern.score(Utf32Str::new(&label, &mut buf), &mut self.matcher);
            let (base, on_label) = match on_label {
                Some(s) => (s, true),
                None => {
                    // Keywords only find a command; they rank below
                    // a match on what the user can see.
                    let all = format!("{label} {}", c.keywords.join(" "));
                    match pattern.score(Utf32Str::new(&all, &mut buf), &mut self.matcher) {
                        Some(s) => (s / 2, false),
                        None => continue,
                    }
                }
            };
            let bonus = recent_rank
                .get(c.id.as_str())
                .map_or(0, |&rank| 30u32.saturating_sub(rank as u32));
            scored.push((base + bonus, label, c, on_label));
        }
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.len().cmp(&b.1.len()))
                .then_with(|| a.1.cmp(&b.1))
        });
        scored
            .into_iter()
            .map(|(_, label, c, on_label)| CommandMatch {
                id: c.id.clone(),
                highlights: if on_label {
                    highlights(&label, &pattern, &mut self.matcher)
                } else {
                    Vec::new()
                },
                is_recent: recent_rank.contains_key(c.id.as_str()),
                label,
            })
            .collect()
    }
}

/// `(id, category, title, keywords)` of every built-in command. Ids follow
/// VSCode where it has the same command, so keymaps carry over (M2-10).
const BUILTIN: &[(&str, &str, &str, &[&str])] = &[
    (
        "workbench.action.files.newUntitledFile",
        "File",
        "New File",
        &["untitled", "buffer"],
    ),
    (
        "workbench.action.files.openFileFolder",
        "File",
        "Open File or Folder…",
        &[],
    ),
    (
        "workbench.action.files.openFolder",
        "File",
        "Open Folder…",
        &["workspace", "project"],
    ),
    ("workbench.action.files.save", "File", "Save", &["write"]),
    (
        "workbench.action.files.saveAs",
        "File",
        "Save As…",
        &["write"],
    ),
    (
        "workbench.action.closeActiveEditor",
        "File",
        "Close Tab",
        &["editor"],
    ),
    ("workbench.action.closeWindow", "File", "Close Window", &[]),
    ("undo", "Edit", "Undo", &[]),
    ("redo", "Edit", "Redo", &[]),
    ("editor.action.clipboardCutAction", "Edit", "Cut", &[]),
    ("editor.action.clipboardCopyAction", "Edit", "Copy", &[]),
    ("editor.action.clipboardPasteAction", "Edit", "Paste", &[]),
    ("editor.action.selectAll", "Selection", "Select All", &[]),
    (
        "editor.action.commentLine",
        "Edit",
        "Toggle Line Comment",
        &["cmd+/"],
    ),
    (
        "workbench.action.gotoSymbol",
        "Go",
        "Go to Symbol…",
        &["cmd+shift+o"],
    ),
    (
        "workbench.action.gotoWorkspaceSymbol",
        "Go",
        "Go to Workspace Symbol…",
        &["cmd+t"],
    ),
    (
        "workbench.action.openUserSettingsJson",
        "Preferences",
        "Open User Settings (JSON)…",
        &["settings"],
    ),
    (
        "workbench.action.openWorkspaceSettingsJson",
        "Preferences",
        "Open Workspace Settings (JSON)…",
        &["settings", "workspace"],
    ),
    (
        "editor.action.addSelectionToNextFindMatch",
        "Selection",
        "Add Next Occurrence",
        &["multi cursor"],
    ),
    (
        "editor.action.selectHighlights",
        "Selection",
        "Select All Occurrences",
        &["multi cursor"],
    ),
    (
        "editor.action.insertCursorAbove",
        "Selection",
        "Add Cursor Above",
        &["multi cursor"],
    ),
    (
        "editor.action.insertCursorBelow",
        "Selection",
        "Add Cursor Below",
        &["multi cursor"],
    ),
    ("actions.find", "Find", "Find…", &["search"]),
    (
        "editor.action.startFindReplaceAction",
        "Find",
        "Find and Replace…",
        &["search"],
    ),
    (
        "editor.action.nextMatchFindAction",
        "Find",
        "Find Next",
        &["search"],
    ),
    (
        "editor.action.previousMatchFindAction",
        "Find",
        "Find Previous",
        &["search"],
    ),
    (
        "actions.findWithSelection",
        "Find",
        "Use Selection for Find",
        &["search"],
    ),
    (
        "workbench.action.quickOpen",
        "Go",
        "Go to File…",
        &["quick open", "fuzzy"],
    ),
    (
        "workbench.action.findInFiles",
        "Search",
        "Search in Workspace…",
        &["workspace", "files", "text"],
    ),
    ("workbench.action.gotoLine", "Go", "Go to Line…", &["jump"]),
    (
        "workbench.action.navigateBack",
        "Go",
        "Back",
        &["history", "previous location"],
    ),
    (
        "workbench.action.navigateForward",
        "Go",
        "Forward",
        &["history", "next location"],
    ),
    (
        "workbench.action.showCommands",
        "View",
        "Command Palette…",
        &["show all commands"],
    ),
    (
        "workbench.action.openGlobalKeybindingsFile",
        "Preferences",
        "Open Keyboard Shortcuts (JSON)",
        &["keybindings", "keymap", "shortcuts"],
    ),
    (
        "workbench.action.toggleSidebarVisibility",
        "View",
        "Toggle Sidebar",
        &["hide", "show", "explorer"],
    ),
    (
        "galaxy.toggleGitGraph",
        "View",
        "Toggle Git Graph",
        &["git", "history", "log", "commits", "branches"],
    ),
    (
        "workbench.action.togglePanel",
        "View",
        "Toggle Terminal",
        &["panel", "bottom", "hide", "show", "console"],
    ),
    (
        "workbench.action.terminal.moveToEditor",
        "Terminal",
        "Move Terminal into Editor Area",
        &["tab", "editor", "shell"],
    ),
    (
        "workbench.action.terminal.moveToTerminalPanel",
        "Terminal",
        "Move Terminal into Panel",
        &["tab", "panel", "bottom", "shell"],
    ),
    (
        "workbench.action.createTerminalEditor",
        "Terminal",
        "Create New Terminal in Editor Area",
        &["tab", "editor", "shell", "new"],
    ),
    (
        "workbench.view.explorer",
        "View",
        "Show Explorer",
        &["files", "sidebar"],
    ),
    (
        "workbench.view.scm",
        "View",
        "Show Source Control",
        &["git", "scm", "changes", "sidebar"],
    ),
    (
        "workbench.view.extensions",
        "View",
        "Show Extensions",
        &["plugins", "language server", "lsp", "install"],
    ),
    (
        "workbench.action.nextEditor",
        "View",
        "Show Next Tab",
        &["editor"],
    ),
    (
        "workbench.action.previousEditor",
        "View",
        "Show Previous Tab",
        &["editor"],
    ),
    (
        "workbench.action.openPreviousRecentlyUsedEditor",
        "View",
        "Switch to Last Tab",
        &["editor", "recent"],
    ),
    (
        "explorer.newFile",
        "Explorer",
        "New File…",
        &["create", "add"],
    ),
    (
        "explorer.newFolder",
        "Explorer",
        "New Folder…",
        &["create", "add", "directory"],
    ),
    (
        "revealFileInOS",
        "Explorer",
        "Reveal in Finder",
        &["show", "open folder"],
    ),
    ("copyFilePath", "Explorer", "Copy Path", &["clipboard"]),
    (
        "copyRelativeFilePath",
        "Explorer",
        "Copy Relative Path",
        &["clipboard"],
    ),
    ("renameFile", "Explorer", "Rename…", &["move", "name"]),
    (
        "deleteFile",
        "Explorer",
        "Delete (Move to Trash)",
        &["remove", "trash"],
    ),
    (
        "workbench.files.action.refreshFilesExplorer",
        "Explorer",
        "Refresh",
        &["reload"],
    ),
    (
        "workbench.files.action.collapseExplorerFolders",
        "Explorer",
        "Collapse Folders",
        &[],
    ),
    (
        "galaxy.showTypingLatency",
        "Developer",
        "Show Typing Latency",
        &["performance", "benchmark"],
    ),
    ("galaxy.about", "Galaxy", "About", &["version"]),
    ("workbench.action.quit", "Galaxy", "Quit", &["exit"]),
];

/// The commands the app ships with.
pub fn builtin() -> CommandRegistry {
    let mut r = CommandRegistry::new();
    for &(id, category, title, keywords) in BUILTIN {
        r.register(Command {
            id: id.into(),
            category: category.into(),
            title: title.into(),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
        })
        .expect("built-in command ids are unique");
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(id: &str, category: &str, title: &str, keywords: &[&str]) -> Command {
        Command {
            id: id.into(),
            category: category.into(),
            title: title.into(),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
        }
    }

    fn registry() -> CommandRegistry {
        let mut r = CommandRegistry::new();
        for c in [
            cmd("file.save", "File", "Save", &[]),
            cmd("file.saveAs", "File", "Save As…", &[]),
            cmd(
                "view.toggleSidebar",
                "View",
                "Toggle Sidebar",
                &["hide", "show", "explorer"],
            ),
            cmd("go.file", "Go", "Go to File…", &["quick open"]),
            cmd("go.line", "Go", "Go to Line…", &[]),
            cmd("about", "", "About Galaxy", &[]),
        ] {
            r.register(c).unwrap();
        }
        r
    }

    fn ids(found: &[CommandMatch]) -> Vec<&str> {
        found.iter().map(|m| m.id.as_str()).collect()
    }

    #[test]
    fn registration_rejects_duplicates_and_empty_ids() {
        let mut r = registry();
        assert_eq!(r.len(), 6);
        assert_eq!(r.get("go.line").unwrap().label(), "Go: Go to Line…");
        assert_eq!(r.get("about").unwrap().label(), "About Galaxy");
        assert!(matches!(
            r.register(cmd("file.save", "File", "Again", &[])),
            Err(RegistryError::Duplicate(_))
        ));
        assert!(matches!(
            r.register(cmd(" ", "", "x", &[])),
            Err(RegistryError::EmptyId)
        ));
    }

    #[test]
    fn empty_query_lists_recent_first_then_alphabetically() {
        let mut r = registry();
        r.note_used("go.line");
        r.note_used("file.save");
        r.note_used("nope");
        let found = r.search("", None);
        assert_eq!(
            ids(&found),
            [
                "file.save",
                "go.line",
                "about",
                "file.saveAs",
                "go.file",
                "view.toggleSidebar"
            ]
        );
        assert!(found[0].is_recent && found[1].is_recent && !found[2].is_recent);
        assert_eq!(r.recent(), ["file.save", "go.line"]);
    }

    #[test]
    fn fuzzy_search_ranks_and_highlights() {
        let mut r = registry();
        let found = r.search("save", None);
        assert_eq!(
            ids(&found),
            ["file.save", "file.saveAs"],
            "shorter label wins a tie"
        );
        // "File: Save": S-a-v-e at UTF-16 6..10.
        assert_eq!(found[0].highlights, [(6, 4)]);

        assert_eq!(ids(&r.search("tgsb", None)), ["view.toggleSidebar"]);
        assert!(r.search("zzzz", None).is_empty());
    }

    #[test]
    fn keywords_find_commands_without_highlights() {
        let mut r = registry();
        let found = r.search("explorer", None);
        assert_eq!(ids(&found), ["view.toggleSidebar"]);
        assert!(found[0].highlights.is_empty());
        assert_eq!(ids(&r.search("quick open", None)), ["go.file"]);
    }

    #[test]
    fn recency_breaks_close_scores_and_availability_filters() {
        let mut r = registry();
        r.note_used("file.saveAs");
        assert_eq!(ids(&r.search("save", None))[0], "file.saveAs");

        let available: HashSet<String> = ["go.file".to_string(), "about".to_string()].into();
        assert_eq!(ids(&r.search("", Some(&available))), ["about", "go.file"]);
        assert_eq!(ids(&r.search("go", Some(&available))), ["go.file"]);
    }

    #[test]
    fn builtin_commands_are_unique_and_searchable() {
        let mut r = builtin();
        assert_eq!(r.len(), BUILTIN.len());
        assert_eq!(
            ids(&r.search("go to file", None))[0],
            "workbench.action.quickOpen"
        );
        assert_eq!(
            ids(&r.search("sidebar", None))[0],
            "workbench.action.toggleSidebarVisibility"
        );
    }

    #[test]
    fn recent_list_is_capped() {
        let mut r = CommandRegistry::new();
        for i in 0..(MAX_RECENT + 5) {
            r.register(cmd(&format!("c{i}"), "", &format!("C {i}"), &[]))
                .unwrap();
            r.note_used(&format!("c{i}"));
        }
        assert_eq!(r.recent().len(), MAX_RECENT);
        assert_eq!(r.recent()[0], format!("c{}", MAX_RECENT + 4));
    }
}
