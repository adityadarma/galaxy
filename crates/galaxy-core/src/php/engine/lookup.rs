//! Finding classes, functions and members: the project index, the open
//! files and the bundled stubs.

use super::*;
use crate::php::version::VersionSelection;

impl PhpEngine {
    // ---- lookup -----------------------------------------------------

    pub(super) fn load(&mut self, path: &Path) -> Option<Arc<FileSymbols>> {
        if let Some(file) = self.open.get(path) {
            return Some(file.clone());
        }
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if let Some(cached) = self.parsed.get(path)
            && cached.mtime == mtime
        {
            return Some(cached.file.clone());
        }
        let text = std::fs::read_to_string(path).ok()?;
        let file = Arc::new(parse_file(path, &text));
        self.parsed.insert(
            path.to_path_buf(),
            Parsed {
                mtime,
                file: file.clone(),
            },
        );
        Some(file)
    }

    pub(super) fn scanned(&mut self) -> &HashMap<String, PathBuf> {
        if self.scanned.is_none() {
            if self.deferred {
                static EMPTY: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
                return EMPTY.get_or_init(HashMap::new);
            }
            self.scanned = Some(if self.autoload.is_empty() {
                scan_sources(&self.root)
                    .into_iter()
                    .map(|(fqn, path)| (fqn.to_lowercase(), path))
                    .collect()
            } else {
                HashMap::new()
            });
        }
        self.scanned.as_ref().expect("set above")
    }

    pub fn class(&mut self, fqn: &str) -> Option<Arc<ClassInfo>> {
        let fqn = generics::base(fqn).trim_start_matches('\\');
        for file in self.open.values() {
            if let Some(found) = file
                .classes
                .iter()
                .find(|c| c.fqn.eq_ignore_ascii_case(fqn))
            {
                return Some(found.clone());
            }
        }
        let path = match self.autoload.path_for(fqn) {
            Some(path) => Some(path),
            None => self.scanned().get(&fqn.to_lowercase()).cloned(),
        };
        if let Some(path) = path
            && let Some(file) = self.load(&path)
            && let Some(found) = file
                .classes
                .iter()
                .find(|c| c.fqn.eq_ignore_ascii_case(fqn))
        {
            return Some(found.clone());
        }
        self.builtin().class(fqn)
    }

    /// PHP's own declarations. Before the index is installed they are not
    /// parsed on the calling thread, so lookups see none.
    pub(super) fn builtin(&self) -> &'static Stubs {
        static NONE: OnceLock<Stubs> = OnceLock::new();
        if self.deferred && self.built_at.is_none() {
            NONE.get_or_init(Stubs::default)
        } else {
            match self.php_version {
                VersionSelection::Supported(version) => Stubs::for_version(version),
                VersionSelection::Unsupported => NONE.get_or_init(Stubs::default),
            }
        }
    }

    pub(in crate::php) fn function(&mut self, fqn: &str) -> Option<Arc<FunctionInfo>> {
        let key = fqn.trim_start_matches('\\').to_lowercase();
        for file in self.open.values() {
            if let Some(found) = file.functions.iter().find(|f| f.fqn.to_lowercase() == key) {
                return Some(found.clone());
            }
        }
        if let Some(found) = self.function_index().get(&key) {
            return Some(found.clone());
        }
        self.builtin().function(&key)
    }

    pub(super) fn function_index(&mut self) -> &HashMap<String, Arc<FunctionInfo>> {
        if self.functions.is_none() {
            let mut map = HashMap::new();
            let files: Vec<PathBuf> = self.autoload.function_files().to_vec();
            for path in files {
                if let Some(file) = self.load(&path) {
                    for function in &file.functions {
                        map.entry(function.fqn.to_lowercase())
                            .or_insert_with(|| function.clone());
                    }
                }
            }
            self.functions = Some(map);
        }
        self.functions.as_ref().expect("set above")
    }

    pub(super) fn class_names(&mut self) -> Arc<Vec<ClassName>> {
        if self.names.is_none() {
            if self.deferred {
                return Arc::new(Vec::new());
            }
            let built = build(&self.root);
            self.install(built);
        }
        self.names.clone().expect("set above")
    }

    /// The class and what it inherits from or mixes in, nearest first.
    pub(in crate::php) fn lineage(&mut self, class: &Arc<ClassInfo>) -> Vec<Arc<ClassInfo>> {
        let mut out = vec![class.clone()];
        let mut seen = HashSet::from([class.fqn.to_lowercase()]);
        let mut index = 0;
        while index < out.len() && out.len() < 48 {
            let current = out[index].clone();
            index += 1;
            let parents: Vec<String> = current
                .traits
                .iter()
                .chain(&current.extends)
                .chain(&current.implements)
                .chain(&current.mixins)
                .cloned()
                .collect();
            for parent in parents {
                if seen.insert(parent.to_lowercase())
                    && let Some(found) = self.class(&parent)
                {
                    out.push(found);
                }
            }
        }
        out
    }

    pub(in crate::php) fn find_member(
        &mut self,
        class: &Arc<ClassInfo>,
        name: &str,
        call: bool,
    ) -> Option<(Arc<ClassInfo>, Member)> {
        let name = name.trim_start_matches('$');
        for owner in self.lineage(class) {
            let hit = owner.members.iter().find(|m| {
                if call {
                    m.kind == MemberKind::Method && m.name.eq_ignore_ascii_case(name)
                } else {
                    m.kind != MemberKind::Method && m.name == name
                }
            });
            if let Some(member) = hit {
                return Some((owner.clone(), member.clone()));
            }
        }
        None
    }

    /// `$object->name` where no property `name` exists but the class has
    /// `__get`: PHP routes the read through it, and libraries commonly
    /// back such reads with a method of that name (Eloquent relations).
    /// Used for navigation and hover only; the method's return type is not
    /// the type of the property.
    pub(super) fn magic_property_method(
        &mut self,
        class: &Arc<ClassInfo>,
        name: &str,
    ) -> Option<(Arc<ClassInfo>, Member)> {
        let has_getter = self.lineage(class).iter().any(|owner| {
            owner
                .members
                .iter()
                .any(|m| m.kind == MemberKind::Method && m.name.eq_ignore_ascii_case("__get"))
        });
        if has_getter {
            self.find_member(class, name, true)
        } else {
            None
        }
    }

    /// The declaration a member access points at, with the `__get` fallback
    /// for reads of properties that are really methods.
    pub(super) fn find_member_or_magic(
        &mut self,
        class: &Arc<ClassInfo>,
        name: &str,
        call: bool,
    ) -> Option<(Arc<ClassInfo>, Member)> {
        self.find_member(class, name, call).or_else(|| {
            if call {
                let frameworks = self.frameworks.clone();
                frameworks
                    .iter()
                    .find_map(|fw| fw.forwarded_method(self, class, name))
            } else {
                self.magic_property_method(class, name)
            }
        })
    }
}
