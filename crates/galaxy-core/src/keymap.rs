//! Keyboard shortcuts (M2-10): VSCode-like defaults plus the user's
//! `keybindings.json`, in VSCode's format.
//!
//! A user entry `{ "key": "cmd+shift+d", "command": "id" }` adds a binding
//! and takes the key away from any other command; `"command": "-id"`
//! removes a default (all of the command's keys when `key` is empty).
//! A command can have several keys; the first one is shown in menus, and a
//! user's binding comes before the defaults.
//!
//! Not supported yet, reported as problems and skipped: `when` clauses,
//! `args`, and chords such as `cmd+k cmd+s`.

use crate::commands::CommandRegistry;
use crate::jsonc::{self, Value};

/// One keystroke: a key name plus modifiers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub cmd: bool,
    /// Lowercase: a single character (`p`, `-`, `[`) or a name from
    /// [`NAMED_KEYS`].
    pub name: String,
}

/// Keys spelled as words, in VSCode's spelling.
pub const NAMED_KEYS: &[&str] = &[
    "tab",
    "enter",
    "escape",
    "space",
    "backspace",
    "delete",
    "up",
    "down",
    "left",
    "right",
    "home",
    "end",
    "pageup",
    "pagedown",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
];

impl Key {
    /// Parses `cmd+shift+p` style text. Modifier names follow VSCode
    /// (`cmd`/`meta`, `ctrl`, `alt`/`option`, `shift`).
    pub fn parse(text: &str) -> Result<Key, String> {
        let text = text.trim().to_lowercase();
        if text.is_empty() {
            return Err("the key is empty".into());
        }
        if text.contains(char::is_whitespace) {
            return Err(
                "chords (two-step keys such as \"cmd+k cmd+s\") are not supported yet".into(),
            );
        }
        // "cmd++" binds the + key.
        let (mods, name) = match text.strip_suffix("++") {
            Some(m) => (m, "+".to_string()),
            None => match text.rsplit_once('+') {
                Some((m, k)) => (m, k.to_string()),
                None => ("", text.clone()),
            },
        };
        let mut key = Key {
            ctrl: false,
            shift: false,
            alt: false,
            cmd: false,
            name: normalize_name(&name)?,
        };
        for m in mods.split('+').filter(|m| !m.is_empty()) {
            let flag = match m {
                "cmd" | "meta" | "command" | "win" => &mut key.cmd,
                "ctrl" | "control" => &mut key.ctrl,
                "alt" | "option" | "opt" => &mut key.alt,
                "shift" => &mut key.shift,
                _ => return Err(format!("unknown modifier \"{m}\"")),
            };
            if *flag {
                return Err(format!("\"{m}\" is given twice"));
            }
            *flag = true;
        }
        let printable = name_is_printable(&key.name);
        if printable && !(key.cmd || key.ctrl || key.alt) {
            return Err(
                "a letter or symbol key needs cmd, ctrl or alt, or it would block typing".into(),
            );
        }
        let function_key = key.name.starts_with('f') && key.name.len() > 1;
        if !(printable || function_key || key.cmd || key.ctrl || key.alt || key.shift) {
            return Err(format!(
                "\"{}\" alone is needed for typing and moving; add a modifier",
                key.name
            ));
        }
        Ok(key)
    }

    /// Canonical text, modifiers in VSCode's order: `ctrl+shift+alt+cmd+p`.
    /// The Swift side builds the same text from key events.
    pub fn id(&self) -> String {
        let mut s = String::new();
        for (on, m) in [
            (self.ctrl, "ctrl+"),
            (self.shift, "shift+"),
            (self.alt, "alt+"),
            (self.cmd, "cmd+"),
        ] {
            if on {
                s.push_str(m);
            }
        }
        s.push_str(&self.name);
        s
    }
}

fn name_is_printable(name: &str) -> bool {
    name.chars().count() == 1
}

fn normalize_name(name: &str) -> Result<String, String> {
    let name = match name {
        "esc" => "escape",
        "return" => "enter",
        "pgup" => "pageup",
        "pgdn" => "pagedown",
        "uparrow" => "up",
        "downarrow" => "down",
        "leftarrow" => "left",
        "rightarrow" => "right",
        other => other,
    };
    if NAMED_KEYS.contains(&name) {
        return Ok(name.to_string());
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_graphic() => Ok(c.to_ascii_lowercase().to_string()),
        _ if name.starts_with('[') => Err(format!(
            "scan-code keys such as \"{name}\" are not supported; use the character instead"
        )),
        _ => Err(format!("unknown key \"{name}\"")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub command: String,
    pub key: Key,
}

/// Default shortcuts: VSCode's macOS defaults for the commands we have.
pub const DEFAULTS: &[(&str, &str)] = &[
    ("cmd+n", "workbench.action.files.newUntitledFile"),
    ("cmd+o", "workbench.action.files.openFileFolder"),
    ("cmd+shift+o", "workbench.action.gotoSymbol"),
    ("cmd+t", "workbench.action.gotoWorkspaceSymbol"),
    ("cmd+s", "workbench.action.files.save"),
    ("cmd+shift+s", "workbench.action.files.saveAs"),
    ("cmd+w", "workbench.action.closeActiveEditor"),
    ("cmd+shift+w", "workbench.action.closeWindow"),
    ("cmd+z", "undo"),
    ("cmd+shift+z", "redo"),
    ("cmd+x", "editor.action.clipboardCutAction"),
    ("cmd+c", "editor.action.clipboardCopyAction"),
    ("cmd+v", "editor.action.clipboardPasteAction"),
    ("cmd+a", "editor.action.selectAll"),
    ("cmd+/", "editor.action.commentLine"),
    ("cmd+d", "editor.action.addSelectionToNextFindMatch"),
    ("cmd+shift+l", "editor.action.selectHighlights"),
    ("alt+cmd+up", "editor.action.insertCursorAbove"),
    ("alt+cmd+down", "editor.action.insertCursorBelow"),
    ("cmd+f", "actions.find"),
    ("alt+cmd+f", "editor.action.startFindReplaceAction"),
    ("cmd+g", "editor.action.nextMatchFindAction"),
    ("cmd+shift+g", "editor.action.previousMatchFindAction"),
    ("cmd+e", "actions.findWithSelection"),
    ("cmd+p", "workbench.action.quickOpen"),
    ("cmd+shift+f", "workbench.action.findInFiles"),
    ("ctrl+g", "workbench.action.gotoLine"),
    ("ctrl+-", "workbench.action.navigateBack"),
    ("ctrl+shift+-", "workbench.action.navigateForward"),
    ("cmd+shift+p", "workbench.action.showCommands"),
    ("cmd+b", "workbench.action.toggleSidebarVisibility"),
    ("cmd+shift+e", "workbench.view.explorer"),
    ("cmd+shift+]", "workbench.action.nextEditor"),
    ("cmd+shift+[", "workbench.action.previousEditor"),
    (
        "ctrl+tab",
        "workbench.action.openPreviousRecentlyUsedEditor",
    ),
    ("cmd+q", "workbench.action.quit"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    /// For each command, its first binding here is its primary one.
    bindings: Vec<Binding>,
}

impl Keymap {
    pub fn defaults() -> Self {
        Self {
            bindings: DEFAULTS
                .iter()
                .map(|&(key, command)| Binding {
                    command: command.into(),
                    key: Key::parse(key).expect("default keys parse"),
                })
                .collect(),
        }
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Defaults with the user's file applied. Problems (one line each)
    /// describe what was skipped; a file that does not parse leaves the
    /// defaults as they are.
    pub fn with_user_file(text: &str, commands: &CommandRegistry) -> (Self, Vec<String>) {
        let mut map = Self::defaults();
        let mut problems = Vec::new();
        let entries = match jsonc::parse(text) {
            Ok(Value::Array(entries)) => entries,
            Ok(other) => {
                problems.push(format!(
                    "keybindings.json must be an array of {{ \"key\", \"command\" }} entries, not {}",
                    other.kind()
                ));
                return (map, problems);
            }
            Err(e) => {
                problems.push(format!("keybindings.json {e}; using the defaults"));
                return (map, problems);
            }
        };
        let known = |id: &str| commands.commands().iter().any(|c| c.id == id);
        for (i, entry) in entries.iter().enumerate() {
            let key_text = entry.get("key").and_then(Value::as_str).unwrap_or("");
            let label = if key_text.is_empty() {
                format!("entry {}", i + 1)
            } else {
                format!("entry {} ({key_text})", i + 1)
            };
            if let Err(why) = map.apply_entry(entry, key_text, &known) {
                problems.push(format!("{label}: {why}"));
            }
        }
        (map, problems)
    }

    fn apply_entry(
        &mut self,
        entry: &Value,
        key_text: &str,
        known: &dyn Fn(&str) -> bool,
    ) -> Result<(), String> {
        if !matches!(entry, Value::Object(_)) {
            return Err(format!("expected an object, found {}", entry.kind()));
        }
        let command = match entry.get("command") {
            Some(Value::String(c)) if !c.trim().is_empty() => c.trim(),
            Some(Value::String(_)) | None => return Err("\"command\" is missing".into()),
            Some(other) => {
                return Err(format!(
                    "\"command\" must be a string, not {}",
                    other.kind()
                ));
            }
        };
        if entry.get("when").is_some() {
            return Err("\"when\" clauses are not supported yet; skipped".into());
        }
        if entry.get("args").is_some() {
            return Err("\"args\" are not supported yet; skipped".into());
        }
        if let Some(removed) = command.strip_prefix('-') {
            if !known(removed) {
                return Err(format!("unknown command \"{removed}\""));
            }
            if key_text.trim().is_empty() {
                self.bindings.retain(|b| b.command != removed);
            } else {
                let key = Key::parse(key_text)?;
                self.bindings
                    .retain(|b| !(b.command == removed && b.key == key));
            }
            return Ok(());
        }
        if !known(command) {
            return Err(format!("unknown command \"{command}\""));
        }
        if !matches!(entry.get("key"), Some(Value::String(_))) {
            return Err("\"key\" is missing".into());
        }
        let key = Key::parse(key_text)?;
        // One key, one command: the user's choice wins.
        self.bindings.retain(|b| b.key != key);
        self.bindings.insert(
            0,
            Binding {
                command: command.into(),
                key,
            },
        );
        Ok(())
    }
}

/// Starting content for a new `keybindings.json`: how the file works, then
/// every default binding commented out, with the command's name.
pub fn user_file_template(commands: &CommandRegistry) -> String {
    let mut out = String::from(
        "// Galaxy keyboard shortcuts, in the same format as VSCode's keybindings.json.\n\
         // Entries here override the defaults. Save the file to apply it.\n\
         //\n\
         //   { \"key\": \"cmd+shift+d\", \"command\": \"editor.action.addSelectionToNextFindMatch\" }\n\
         //   { \"key\": \"cmd+b\", \"command\": \"-workbench.action.toggleSidebarVisibility\" }  (removes a default)\n\
         //\n\
         // Modifiers: cmd, ctrl, alt, shift. \"when\" clauses and chords are not supported yet.\n\
         //\n\
         // Defaults:\n",
    );
    for (key, id) in DEFAULTS {
        let name = commands
            .commands()
            .iter()
            .find(|c| c.id == *id)
            .map(|c| format!("{}: {}", c.category, c.title))
            .unwrap_or_default();
        out.push_str(&format!(
            "//   {{ \"key\": \"{key}\", \"command\": \"{id}\" }}  {name}\n"
        ));
    }
    out.push_str("[\n]\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands;

    fn key(s: &str) -> Key {
        Key::parse(s).unwrap()
    }

    fn keys_of(map: &Keymap, command: &str) -> Vec<String> {
        map.bindings()
            .iter()
            .filter(|b| b.command == command)
            .map(|b| b.key.id())
            .collect()
    }

    #[test]
    fn parses_keys_in_vscode_spelling() {
        assert_eq!(key("Cmd+Shift+P").id(), "shift+cmd+p");
        assert_eq!(key("shift+cmd+p"), key("cmd+shift+p"));
        assert_eq!(key("ctrl+-").id(), "ctrl+-");
        assert_eq!(key("ctrl+shift+-").id(), "ctrl+shift+-");
        assert_eq!(key("cmd++").name, "+");
        assert_eq!(key("meta+option+UpArrow").id(), "alt+cmd+up");
        assert_eq!(key("f5").id(), "f5", "named keys may stand alone");
        assert_eq!(key("ctrl+esc").name, "escape");
        assert_eq!(key("shift+tab").id(), "shift+tab");

        for (bad, why) in [
            ("", "empty"),
            ("cmd+k cmd+s", "chords"),
            ("hyper+a", "modifier"),
            ("cmd+cmd+a", "twice"),
            ("cmd+foo", "unknown key"),
            ("a", "block typing"),
            ("shift+a", "block typing"),
            ("cmd+[Minus]", "scan-code"),
            ("escape", "add a modifier"),
            ("enter", "add a modifier"),
        ] {
            let err = Key::parse(bad).unwrap_err();
            assert!(err.contains(why), "{bad:?}: {err}");
        }
    }

    #[test]
    fn every_default_is_a_known_command_with_one_key() {
        let registry = commands::builtin();
        let map = Keymap::defaults();
        let mut seen = std::collections::HashSet::new();
        for b in map.bindings() {
            assert!(
                registry.commands().iter().any(|c| c.id == b.command),
                "{}",
                b.command
            );
            assert!(seen.insert(b.key.clone()), "{} is bound twice", b.key.id());
        }
    }

    #[test]
    fn user_entries_add_take_over_and_remove_keys() {
        let registry = commands::builtin();
        let (map, problems) = Keymap::with_user_file(
            r#"[
                // Add a second key; it becomes the one menus show.
                { "key": "cmd+shift+d", "command": "editor.action.addSelectionToNextFindMatch" },
                // Take cmd+p from Go to File.
                { "key": "cmd+p", "command": "workbench.action.showCommands" },
                // Remove one default, and all keys of another.
                { "key": "cmd+b", "command": "-workbench.action.toggleSidebarVisibility" },
                { "command": "-workbench.action.quit" },
            ]"#,
            &registry,
        );
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(
            keys_of(&map, "editor.action.addSelectionToNextFindMatch"),
            ["shift+cmd+d", "cmd+d"]
        );
        assert_eq!(
            keys_of(&map, "workbench.action.showCommands"),
            ["cmd+p", "shift+cmd+p"]
        );
        assert!(keys_of(&map, "workbench.action.quickOpen").is_empty());
        assert!(keys_of(&map, "workbench.action.toggleSidebarVisibility").is_empty());
        assert!(keys_of(&map, "workbench.action.quit").is_empty());
    }

    #[test]
    fn problems_name_the_entry_and_skip_only_it() {
        let registry = commands::builtin();
        let (map, problems) = Keymap::with_user_file(
            r#"[
                { "key": "cmd+k", "command": "no.such.command" },
                { "key": "cmd+k", "command": "actions.find", "when": "editorTextFocus" },
                { "key": "cmd+k cmd+s", "command": "actions.find" },
                { "key": "cmd+k", "command": "actions.find", "args": {} },
                { "key": "cmd+k" },
                "cmd+k",
                { "key": "cmd+j", "command": "actions.find" },
            ]"#,
            &registry,
        );
        assert_eq!(problems.len(), 6, "{problems:#?}");
        assert!(
            problems[0].starts_with("entry 1 (cmd+k): unknown command"),
            "{}",
            problems[0]
        );
        assert!(problems[1].contains("\"when\""));
        assert!(problems[2].contains("chords"));
        assert!(problems[3].contains("\"args\""));
        assert!(problems[4].contains("\"command\" is missing"));
        assert!(problems[5].starts_with("entry 6: expected an object"));
        assert_eq!(
            keys_of(&map, "actions.find"),
            ["cmd+j", "cmd+f"],
            "the valid entry applies"
        );
    }

    #[test]
    fn a_broken_file_keeps_the_defaults() {
        let registry = commands::builtin();
        let (map, problems) = Keymap::with_user_file("[ { \"key\": \"cmd+k\" ", &registry);
        assert_eq!(map, Keymap::defaults());
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].starts_with("keybindings.json line 1"),
            "{}",
            problems[0]
        );
        let (_, problems) = Keymap::with_user_file("{}", &registry);
        assert!(problems[0].contains("must be an array"));
    }

    #[test]
    fn the_template_is_a_valid_empty_file_listing_every_default() {
        let registry = commands::builtin();
        let template = user_file_template(&registry);
        let (map, problems) = Keymap::with_user_file(&template, &registry);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(map, Keymap::defaults());
        for (key, id) in DEFAULTS {
            assert!(
                template.contains(&format!("\"key\": \"{key}\", \"command\": \"{id}\"")),
                "{id}"
            );
        }
        assert!(template.contains("Go: Go to File…"));
    }
}
