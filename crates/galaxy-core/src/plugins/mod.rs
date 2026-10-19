//! Language server plugins.
//!
//! A plugin is a folder with a `galaxy-plugin.json` manifest. Built-in servers
//! are plugins too: their manifests live in the repo's `plugins/` folder and
//! are embedded at compile time. Installed plugins (see [`install`]) live in
//! the user's plugin directory and override built-ins with the same id.
//!
//! Manifest shape:
//!
//! ```json
//! {
//!   "id": "laravel", "name": "Laravel", "version": "0.0.32",
//!   "languages": ["php", "blade"],
//!   "role": "companion", "primaryFallback": true, "priority": 100,
//!   "activation": { "rootMarkers": ["artisan"] },
//!   "server": {
//!     "command": ["${pluginDir}/vendor/bin/laravel-lsp", "laravel-lsp"],
//!     "args": [],
//!     "initializationOptions": { "phpCommand": ["${which:php}"] }
//!   },
//!   "install": { "steps": [["composer", "require", "..."]], "downloads": [] }
//! }
//! ```
//!
//! `server.command` lists candidates; the first that resolves wins. Placeholders:
//! `${pluginDir}`, `${platform}` and `${which:NAME}` (a program found on
//! `PATH`). A value with an unresolvable placeholder is dropped: a command
//! candidate is skipped, an object key is removed.

pub mod install;
pub mod registry;

use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

use crate::lsp::{ServerConfig, default_server_kind};

pub const MANIFEST_FILE: &str = "galaxy-plugin.json";

type Find<'a> = &'a dyn Fn(&str) -> Option<PathBuf>;

const BUILTIN_MANIFESTS: &[&str] = &[
    include_str!("../../../../plugins/rust-analyzer/galaxy-plugin.json"),
    include_str!("../../../../plugins/typescript-language-server/galaxy-plugin.json"),
    include_str!("../../../../plugins/pyright/galaxy-plugin.json"),
    include_str!("../../../../plugins/gopls/galaxy-plugin.json"),
    include_str!("../../../../plugins/intelephense/galaxy-plugin.json"),
    include_str!("../../../../plugins/phpactor/galaxy-plugin.json"),
    include_str!("../../../../plugins/laravel/galaxy-plugin.json"),
    include_str!("../../../../plugins/vue/galaxy-plugin.json"),
    include_str!("../../../../plugins/svelte/galaxy-plugin.json"),
    include_str!("../../../../plugins/sourcekit-lsp/galaxy-plugin.json"),
    include_str!("../../../../plugins/dart/galaxy-plugin.json"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The server that owns a document.
    Primary,
    /// Runs next to the primary (extra navigation, linting).
    Companion,
}

/// One file to fetch during install. Verified against `sha256` before use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    /// `darwin-arm64`, `darwin-x64`, ... or `*` for any platform.
    pub platform: String,
    pub url: String,
    pub sha256: String,
    /// Relative path inside the plugin folder.
    pub dest: String,
    pub executable: bool,
    /// `None` saves the file as is; `gz` decompresses one file to `dest`;
    /// `tar.gz` and `zip` are extracted into the folder `dest`.
    pub archive: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// Normalised: lowercase, no spaces or dashes.
    pub languages: Vec<String>,
    pub role: Role,
    /// A companion may take over as primary when nothing else resolves.
    pub primary_fallback: bool,
    pub priority: i64,
    pub root_markers: Vec<String>,
    pub command: Vec<String>,
    pub args: Vec<String>,
    pub initialization_options: Value,
    pub install_steps: Vec<Vec<String>>,
    pub downloads: Vec<Download>,
    /// Plugin-relative path to expected SHA-256 of that file after install.
    pub checksums: BTreeMap<String, String>,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        Self::from_json(&value)
    }

    pub fn from_json(value: &Value) -> Result<Self, String> {
        let object = value.as_object().ok_or("manifest must be an object")?;
        let string = |key: &str| object.get(key).and_then(Value::as_str);
        let id = string("id").ok_or("manifest needs a string id")?;
        validate_id(id)?;
        let version = string("version").ok_or("manifest needs a string version")?;
        let languages = string_list(object.get("languages"), "languages")?
            .iter()
            .map(|language| normalize_language(language))
            .collect::<Vec<_>>();
        if languages.is_empty() {
            return Err("manifest needs at least one language".into());
        }
        let server = object
            .get("server")
            .and_then(Value::as_object)
            .ok_or("manifest needs a server object")?;
        let command = match server.get("command") {
            Some(Value::String(single)) => vec![single.clone()],
            other => string_list(other, "server.command")?,
        };
        if command.is_empty() {
            return Err("server.command must not be empty".into());
        }
        let role = match string("role").unwrap_or("primary") {
            "primary" => Role::Primary,
            "companion" => Role::Companion,
            other => return Err(format!("unknown role {other:?}")),
        };
        let root_markers = match object.get("activation") {
            Some(activation) => string_list(activation.get("rootMarkers"), "rootMarkers")?,
            None => Vec::new(),
        };
        for marker in &root_markers {
            safe_relative(marker).map_err(|e| format!("rootMarkers: {e}"))?;
        }
        let install = object.get("install");
        let mut install_steps = Vec::new();
        if let Some(steps) = install.and_then(|i| i.get("steps")) {
            for step in steps.as_array().ok_or("install.steps must be an array")? {
                let argv = string_list(Some(step), "install step")?;
                if argv.is_empty() {
                    return Err("install step must not be empty".into());
                }
                install_steps.push(argv);
            }
        }
        let mut downloads = Vec::new();
        if let Some(list) = install.and_then(|i| i.get("downloads")) {
            for entry in list
                .as_array()
                .ok_or("install.downloads must be an array")?
            {
                downloads.push(parse_download(entry)?);
            }
        }
        let mut checksums = BTreeMap::new();
        if let Some(map) = object.get("checksums") {
            for (path, hash) in map.as_object().ok_or("checksums must be an object")? {
                safe_relative(path).map_err(|e| format!("checksums: {e}"))?;
                let hash = hash.as_str().ok_or("checksum must be a string")?;
                validate_sha256(hash)?;
                checksums.insert(path.clone(), hash.to_ascii_lowercase());
            }
        }
        Ok(Self {
            id: id.to_owned(),
            name: string("name").unwrap_or(id).to_owned(),
            version: version.to_owned(),
            description: string("description").unwrap_or_default().to_owned(),
            languages,
            role,
            primary_fallback: object
                .get("primaryFallback")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            priority: object.get("priority").and_then(Value::as_i64).unwrap_or(50),
            root_markers,
            command,
            args: string_list(server.get("args"), "server.args")?,
            initialization_options: server
                .get("initializationOptions")
                .cloned()
                .unwrap_or(Value::Null),
            install_steps,
            downloads,
            checksums,
        })
    }

    /// The manifest has something to run or download on install.
    pub fn has_install_work(&self) -> bool {
        !self.install_steps.is_empty() || !self.downloads.is_empty()
    }

    /// Downloads that apply to this machine.
    pub fn downloads_for(&self, platform: &str) -> Vec<&Download> {
        self.downloads
            .iter()
            .filter(|d| d.platform == "*" || d.platform == platform)
            .collect()
    }

    /// Commands the install would run, rendered for a confirmation prompt.
    pub fn install_summary(&self, platform: &str) -> Vec<String> {
        let mut lines: Vec<String> = self
            .downloads_for(platform)
            .iter()
            .map(|d| format!("download {} (sha256 {})", d.url, &d.sha256[..12]))
            .collect();
        lines.extend(self.install_steps.iter().map(|step| step.join(" ")));
        lines
    }
}

fn parse_download(entry: &Value) -> Result<Download, String> {
    let text = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("download needs string {key}"))
    };
    let url = text("url")?;
    if !(url.starts_with("https://") || url.starts_with("file://")) {
        return Err(format!("download url must be https: {url}"));
    }
    let sha256 = text("sha256")?;
    validate_sha256(sha256)?;
    let dest = text("dest")?;
    safe_relative(dest).map_err(|e| format!("download dest: {e}"))?;
    let archive = entry.get("archive").and_then(Value::as_str);
    if let Some(kind) = archive
        && !matches!(kind, "gz" | "tar.gz" | "zip")
    {
        return Err(format!("unsupported archive {kind:?}"));
    }
    Ok(Download {
        platform: entry
            .get("platform")
            .and_then(Value::as_str)
            .unwrap_or("*")
            .to_owned(),
        url: url.to_owned(),
        sha256: sha256.to_ascii_lowercase(),
        dest: dest.to_owned(),
        executable: entry
            .get("executable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        archive: archive.map(str::to_owned),
    })
}

fn string_list(value: Option<&Value>, what: &str) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| format!("{what} must be an array"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{what} must contain strings"))
        })
        .collect()
}

/// Plugin ids become folder names, so keep them to a safe charset.
pub fn validate_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && !id.starts_with(['.', '-'])
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid plugin id {id:?}: use lowercase letters, digits, '-', '_' or '.'"
        ))
    }
}

pub(crate) fn validate_sha256(hash: &str) -> Result<(), String> {
    if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(format!("invalid sha256 {hash:?}"))
    }
}

/// Rejects absolute paths and `..` so manifests cannot reach outside their folder.
pub(crate) fn safe_relative(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    let ok = !path.is_empty()
        && p.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if ok {
        Ok(())
    } else {
        Err(format!("path must stay inside the plugin folder: {path:?}"))
    }
}

fn normalize_language(language: &str) -> String {
    language.to_ascii_lowercase().replace([' ', '-'], "")
}

/// `darwin-arm64`, `darwin-x64`, `linux-x64`, ...
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// `LspKey::language` for a companion server: unique per kind and plugin so it
/// gets its own process next to the primary.
pub fn companion_language(server_kind: &str, plugin_id: &str) -> String {
    format!("{server_kind}-{plugin_id}")
}

#[derive(Debug, Clone)]
pub struct Plugin {
    pub manifest: Manifest,
    /// Install folder. `None` for built-ins.
    pub dir: Option<PathBuf>,
    pub builtin: bool,
    pub enabled: bool,
}

impl Plugin {
    fn active_for(&self, root: Option<&Path>) -> bool {
        self.manifest.root_markers.is_empty()
            || root.is_some_and(|root| {
                self.manifest
                    .root_markers
                    .iter()
                    .any(|marker| root.join(marker).exists())
            })
    }

    fn matches(&self, language: &str) -> bool {
        let normalized = normalize_language(language);
        let kind = default_server_kind(language);
        self.manifest
            .languages
            .iter()
            .any(|l| *l == normalized || Some(l.as_str()) == kind)
    }

    /// A bundled plugin with install steps whose server cannot be found yet.
    /// One already on `PATH` needs no local copy, so it is not offered.
    pub fn installable(&self, find: Find) -> bool {
        self.builtin && self.manifest.has_install_work() && self.server_config(find).is_none()
    }

    /// Launch details, or `None` when no command candidate resolves.
    pub fn server_config(&self, find: Find) -> Option<ServerConfig> {
        let ctx = Expand {
            plugin_dir: self.dir.as_deref(),
            find,
        };
        let args = self
            .manifest
            .args
            .iter()
            .map(|arg| ctx.string(arg))
            .collect::<Option<Vec<_>>>()?;
        let program = self
            .manifest
            .command
            .iter()
            .find_map(|candidate| find(&ctx.string(candidate)?))?;
        let initialization_options = match ctx.value(&self.manifest.initialization_options) {
            Some(Value::Object(map)) if map.is_empty() => Value::Null,
            Some(value) => value,
            None => Value::Null,
        };
        Some(ServerConfig {
            program: program.to_string_lossy().into_owned(),
            args,
            log_traffic: false,
            initialization_options,
            restart_backoff: [
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(2),
            ],
        })
    }
}

struct Expand<'a> {
    plugin_dir: Option<&'a Path>,
    find: Find<'a>,
}

impl Expand<'_> {
    fn string(&self, text: &str) -> Option<String> {
        let mut out = String::new();
        let mut rest = text;
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let end = rest[start..].find('}')? + start;
            let name = &rest[start + 2..end];
            match name {
                "pluginDir" => out.push_str(&self.plugin_dir?.to_string_lossy()),
                "platform" => out.push_str(&platform()),
                _ => {
                    let program = name.strip_prefix("which:")?;
                    out.push_str(&(self.find)(program)?.to_string_lossy());
                }
            }
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        Some(out)
    }

    /// Strings with unresolved placeholders vanish; arrays holding one vanish
    /// whole; object keys with vanished values are removed.
    fn value(&self, value: &Value) -> Option<Value> {
        match value {
            Value::String(text) => self.string(text).map(Value::String),
            Value::Array(items) => items
                .iter()
                .map(|item| self.value(item))
                .collect::<Option<Vec<_>>>()
                .map(Value::Array),
            Value::Object(map) => Some(Value::Object(
                map.iter()
                    .filter_map(|(key, item)| Some((key.clone(), self.value(item)?)))
                    .collect(),
            )),
            other => Some(other.clone()),
        }
    }
}

/// Every known plugin, enabled or not.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub plugins: Vec<Plugin>,
}

fn builtin_plugins() -> &'static [Plugin] {
    static BUILTINS: OnceLock<Vec<Plugin>> = OnceLock::new();
    BUILTINS.get_or_init(|| {
        BUILTIN_MANIFESTS
            .iter()
            .map(|text| Plugin {
                manifest: Manifest::parse(text).expect("built-in plugin manifest is valid"),
                dir: None,
                builtin: true,
                enabled: true,
            })
            .collect()
    })
}

/// Plugins the user added sort before built-ins. An installed copy of a
/// built-in keeps the built-in's rank, so installing phpactor does not push
/// it ahead of intelephense.
fn ranks_as_builtin(plugin: &Plugin) -> bool {
    plugin.builtin
        || builtin_plugins()
            .iter()
            .any(|b| b.manifest.id == plugin.manifest.id)
}

/// Embedded manifest text of a built-in plugin, for installing it locally.
pub(crate) fn builtin_manifest_text(id: &str) -> Option<&'static str> {
    BUILTIN_MANIFESTS
        .iter()
        .zip(builtin_plugins())
        .find(|(_, plugin)| plugin.manifest.id == id)
        .map(|(text, _)| *text)
}

impl Catalog {
    /// Built-in servers only.
    pub fn builtin() -> Self {
        Self {
            plugins: builtin_plugins().to_vec(),
        }
    }

    /// Built-ins as if each were installed, so tests can resolve their
    /// `${pluginDir}` commands.
    #[cfg(test)]
    pub(crate) fn builtin_with_dirs() -> Self {
        let mut catalog = Self::builtin();
        for plugin in &mut catalog.plugins {
            plugin.dir = Some(PathBuf::from(format!("/plugins/{}", plugin.manifest.id)));
        }
        catalog
    }

    /// Built-ins plus everything installed under `dir`. Installed plugins
    /// replace built-ins with the same id. Problems with single plugins are
    /// returned, never fatal.
    pub fn load(dir: &Path) -> (Self, Vec<String>) {
        let mut errors = Vec::new();
        let state = match install::State::load(dir) {
            Ok(state) => state,
            Err(error) => {
                errors.push(error);
                install::State::default()
            }
        };
        let mut plugins = Vec::new();
        for id in state.installed.keys() {
            let plugin_dir = dir.join(id);
            let manifest = std::fs::read_to_string(plugin_dir.join(MANIFEST_FILE))
                .map_err(|e| e.to_string())
                .and_then(|text| Manifest::parse(&text));
            match manifest {
                Ok(manifest) if manifest.id == *id => plugins.push(Plugin {
                    manifest,
                    dir: Some(plugin_dir),
                    builtin: false,
                    enabled: true,
                }),
                Ok(_) => errors.push(format!("plugin {id}: manifest id does not match folder")),
                Err(error) => errors.push(format!("plugin {id}: {error}")),
            }
        }
        let overridden: HashSet<String> = plugins.iter().map(|p| p.manifest.id.clone()).collect();
        plugins.extend(
            builtin_plugins()
                .iter()
                .filter(|p| !overridden.contains(&p.manifest.id))
                .cloned(),
        );
        for plugin in &mut plugins {
            plugin.enabled = !state.disabled.contains(&plugin.manifest.id);
        }
        (Self { plugins }, errors)
    }

    pub fn get(&self, id: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.manifest.id == id)
    }

    /// Candidates for `language`, installed before built-in, then by priority.
    fn candidates(&self, language: &str, root: Option<&Path>, role: Role) -> Vec<&Plugin> {
        let mut found: Vec<&Plugin> = self
            .plugins
            .iter()
            .filter(|p| p.enabled && p.manifest.role == role)
            .filter(|p| p.matches(language) && p.active_for(root))
            .collect();
        found.sort_by_key(|p| (ranks_as_builtin(p), p.manifest.priority));
        found
    }

    /// The server that owns `language` documents in `root`.
    pub fn primary(
        &self,
        language: &str,
        root: Option<&Path>,
        find: Find,
    ) -> Option<(&Plugin, ServerConfig)> {
        let first = self.candidates(language, root, Role::Primary);
        // Companions that allow it are the last resort, e.g. laravel-lsp alone.
        let fallback = self
            .candidates(language, root, Role::Companion)
            .into_iter()
            .filter(|p| p.manifest.primary_fallback);
        first
            .into_iter()
            .chain(fallback)
            .find_map(|plugin| Some((plugin, plugin.server_config(find)?)))
    }

    /// Servers to run next to a primary, skipping one that already is it.
    pub fn companions(
        &self,
        language: &str,
        root: Option<&Path>,
        primary_program: &str,
        find: Find,
    ) -> Vec<(&Plugin, ServerConfig)> {
        let primary_name = Path::new(primary_program).file_name();
        self.candidates(language, root, Role::Companion)
            .into_iter()
            .filter_map(|plugin| Some((plugin, plugin.server_config(find)?)))
            .filter(|(_, config)| Path::new(&config.program).file_name() != primary_name)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_find(available: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
        // Servers are launched from `${pluginDir}/...`, so match on file name.
        move |program| {
            let name = Path::new(program).file_name()?.to_str()?;
            available
                .contains(&name)
                .then(|| PathBuf::from(format!("/fake/bin/{name}")))
        }
    }

    #[test]
    fn builtin_manifests_all_parse() {
        let catalog = Catalog::builtin();
        assert_eq!(catalog.plugins.len(), BUILTIN_MANIFESTS.len());
        assert!(catalog.get("laravel").unwrap().manifest.role == Role::Companion);
    }

    #[test]
    fn install_is_offered_only_when_the_server_is_missing() {
        let catalog = Catalog::builtin_with_dirs();
        let phpactor = catalog.get("phpactor").unwrap();
        assert!(phpactor.installable(&fake_find(&[])));
        assert!(
            !phpactor.installable(&fake_find(&["phpactor"])),
            "already installed"
        );
        // Every built-in server needs an install, none falls back to PATH.
        // SourceKit-LSP and the Dart analysis server ship with their SDKs and
        // have nothing to install.
        for plugin in catalog.plugins.iter().filter(|plugin| {
            !matches!(
                plugin.manifest.id.as_str(),
                "sourcekit-lsp" | "dart-analysis-server"
            )
        }) {
            assert!(plugin.manifest.has_install_work(), "{}", plugin.manifest.id);
            assert!(
                plugin
                    .manifest
                    .command
                    .iter()
                    .all(|c| c.contains("${pluginDir}")),
                "{} may be launched from PATH",
                plugin.manifest.id
            );
        }
        // A local copy is never offered a second install.
        let mut local = phpactor.clone();
        local.builtin = false;
        assert!(!local.installable(&fake_find(&[])));
    }

    #[test]
    fn rejects_bad_manifests() {
        let bad = |extra: &str| {
            Manifest::parse(&format!(
                r#"{{"id":"x","version":"1","languages":["a"],"server":{{"command":["s"]}}{extra}}}"#
            ))
        };
        assert!(bad("").is_ok());
        assert!(bad(r#","role":"boss""#).is_err());
        assert!(bad(r#","activation":{"rootMarkers":["../x"]}"#).is_err());
        assert!(bad(r#","checksums":{"/etc/passwd":"00"}"#).is_err());
        assert!(
            bad(r#","install":{"downloads":[{"url":"http://x","sha256":"00","dest":"a"}]}"#)
                .is_err()
        );
        assert!(
            Manifest::parse(
                r#"{"id":"../evil","version":"1","languages":["a"],"server":{"command":"s"}}"#
            )
            .is_err()
        );
        assert!(
            Manifest::parse(
                r#"{"id":"Up","version":"1","languages":["a"],"server":{"command":"s"}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn placeholders_resolve_or_drop() {
        let find = fake_find(&["php"]);
        let ctx = Expand {
            plugin_dir: Some(Path::new("/p")),
            find: &find,
        };
        assert_eq!(ctx.string("${pluginDir}/bin").unwrap(), "/p/bin");
        assert_eq!(ctx.string("${which:php}").unwrap(), "/fake/bin/php");
        assert!(ctx.string("${which:ruby}").is_none());
        assert!(ctx.string("${nope}").is_none());
        let value = serde_json::json!({ "a": ["${which:ruby}"], "b": "${which:php}" });
        assert_eq!(
            ctx.value(&value).unwrap(),
            serde_json::json!({ "b": "/fake/bin/php" })
        );
        let no_dir = Expand {
            plugin_dir: None,
            find: &find,
        };
        assert!(no_dir.string("${pluginDir}/x").is_none());
    }

    #[test]
    fn primary_prefers_installed_then_priority() {
        let mut catalog = Catalog::builtin_with_dirs();
        let all = fake_find(&["intelephense", "phpactor"]);
        let (plugin, _) = catalog.primary("php", None, &all).unwrap();
        assert_eq!(plugin.manifest.id, "intelephense");

        // Disabled plugins are skipped.
        catalog
            .plugins
            .iter_mut()
            .find(|p| p.manifest.id == "intelephense")
            .unwrap()
            .enabled = false;
        let (plugin, _) = catalog.primary("php", None, &all).unwrap();
        assert_eq!(plugin.manifest.id, "phpactor");

        // An installed plugin beats built-ins whatever its priority.
        let mut extra = catalog.get("phpactor").unwrap().clone();
        extra.manifest.id = "devsense".into();
        extra.manifest.command = vec!["devsense".into()];
        extra.manifest.priority = 90;
        extra.builtin = false;
        catalog.plugins.push(extra);
        let both = fake_find(&["devsense", "phpactor"]);
        let (plugin, config) = catalog.primary("Blade", None, &both).unwrap();
        assert_eq!(plugin.manifest.id, "devsense");
        assert_eq!(config.program, "/fake/bin/devsense");
    }

    #[test]
    fn vue_resolves_to_its_own_server_with_a_bundled_typescript() {
        let bare = Catalog::builtin();
        // Not installed yet: nothing to launch, but an install is offered.
        let find = fake_find(&[]);
        assert!(bare.primary("Vue", None, &find).is_none());
        let mut catalog = Catalog::builtin_with_dirs();
        assert!(bare.get("vue-language-server").unwrap().installable(&find));
        // Typescript's own server never claims .vue files.
        let plugin = catalog
            .plugins
            .iter_mut()
            .find(|p| p.manifest.id == "vue-language-server")
            .unwrap();
        plugin.dir = Some(PathBuf::from("/p/vue"));
        plugin.builtin = false;
        let find = |program: &str| Some(PathBuf::from(program));
        let (plugin, config) = catalog.primary("Vue", None, &find).unwrap();
        assert_eq!(plugin.manifest.id, "vue-language-server");
        assert_eq!(
            config.program,
            "/p/vue/node_modules/.bin/vue-language-server"
        );
        assert_eq!(config.args, ["--stdio"]);
        assert_eq!(config.initialization_options["vue"]["hybridMode"], false);
        assert_eq!(
            config.initialization_options["typescript"]["tsdk"],
            "/p/vue/node_modules/typescript/lib"
        );
        let (ts, _) = catalog.primary("TypeScript", None, &find).unwrap();
        assert_eq!(ts.manifest.id, "typescript-language-server");
    }

    #[test]
    fn svelte_resolves_to_its_own_server_with_a_bundled_typescript() {
        let bare = Catalog::builtin();
        let find = fake_find(&[]);
        assert!(bare.primary("Svelte", None, &find).is_none());
        assert!(
            bare.get("svelte-language-server")
                .unwrap()
                .installable(&find)
        );
        let mut catalog = Catalog::builtin_with_dirs();
        let plugin = catalog
            .plugins
            .iter_mut()
            .find(|p| p.manifest.id == "svelte-language-server")
            .unwrap();
        plugin.dir = Some(PathBuf::from("/p/svelte"));
        plugin.builtin = false;
        let find = |program: &str| Some(PathBuf::from(program));
        let (plugin, config) = catalog.primary("Svelte", None, &find).unwrap();
        assert_eq!(plugin.manifest.id, "svelte-language-server");
        assert_eq!(config.program, "/p/svelte/node_modules/.bin/svelteserver");
        assert_eq!(config.args, ["--stdio"]);
        let (ts, _) = catalog.primary("TypeScript", None, &find).unwrap();
        assert_eq!(ts.manifest.id, "typescript-language-server");
    }

    #[test]
    fn plugin_languages_outside_the_builtin_table_resolve() {
        let mut catalog = Catalog::default();
        let manifest = Manifest::parse(
            r#"{"id":"ruby-lsp","version":"1","languages":["Ruby"],"server":{"command":"ruby-lsp"}}"#,
        )
        .unwrap();
        catalog.plugins.push(Plugin {
            manifest,
            dir: None,
            builtin: false,
            enabled: true,
        });
        let find = fake_find(&["ruby-lsp"]);
        assert!(catalog.primary("ruby", None, &find).is_some());
        assert!(catalog.primary("python", None, &find).is_none());
    }
}
