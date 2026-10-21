//! Locating GOROOT and the module cache from the environment.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub(super) const GOROOT_FALLBACKS: [&str; 3] = [
    "/usr/local/go",
    "/opt/homebrew/opt/go/libexec",
    "/usr/lib/go",
];

/// GOROOT from the environment, the `go` executable on `PATH`, or a
/// well-known install location.
pub fn discover_goroot() -> Option<PathBuf> {
    let fallbacks: Vec<PathBuf> = GOROOT_FALLBACKS.iter().map(PathBuf::from).collect();
    discover_goroot_from(
        std::env::var_os("GOROOT").as_deref(),
        std::env::var_os("PATH").as_deref(),
        &fallbacks,
    )
}

/// [`discover_goroot`] with the environment passed in.
pub fn discover_goroot_from(
    env_goroot: Option<&OsStr>,
    path_env: Option<&OsStr>,
    fallbacks: &[PathBuf],
) -> Option<PathBuf> {
    if let Some(root) = env_goroot.filter(|v| !v.is_empty()) {
        let root = PathBuf::from(root);
        if root.join("src").is_dir() {
            return Some(root);
        }
    }
    let has_runtime = |root: &Path| root.join("src/runtime").is_dir();
    if let Some(path_env) = path_env {
        for dir in std::env::split_paths(path_env) {
            let exe = dir.join("go");
            if !exe.is_file() {
                continue;
            }
            let Ok(real) = exe.canonicalize() else {
                continue;
            };
            let Some(root) = real.parent().and_then(Path::parent) else {
                continue;
            };
            if has_runtime(root) {
                return Some(root.to_path_buf());
            }
            let libexec = root.join("libexec");
            if has_runtime(&libexec) {
                return Some(libexec);
            }
        }
    }
    fallbacks.iter().find(|root| has_runtime(root)).cloned()
}

/// The module cache: `GOMODCACHE`, else the first `GOPATH` entry plus
/// `pkg/mod`, else `$HOME/go/pkg/mod`.
pub fn discover_modcache() -> Option<PathBuf> {
    discover_modcache_from(
        std::env::var_os("GOMODCACHE").as_deref(),
        std::env::var_os("GOPATH").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// [`discover_modcache`] with the environment passed in.
pub fn discover_modcache_from(
    gomodcache: Option<&OsStr>,
    gopath: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    if let Some(dir) = gomodcache.filter(|v| !v.is_empty()) {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    if let Some(gopath) = gopath
        && let Some(first) = std::env::split_paths(gopath).find(|p| !p.as_os_str().is_empty())
    {
        let dir = first.join("pkg/mod");
        if dir.is_dir() {
            return Some(dir);
        }
    }
    let dir = Path::new(home.filter(|v| !v.is_empty())?).join("go/pkg/mod");
    dir.is_dir().then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn goroot_from_environment_and_fallbacks() {
        let tmp = tempfile::tempdir().unwrap();
        let with_src = tmp.path().join("a");
        std::fs::create_dir_all(with_src.join("src")).unwrap();
        let env = Some(with_src.as_os_str());
        assert_eq!(discover_goroot_from(env, None, &[]), Some(with_src.clone()));

        let bare = tmp.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(
            discover_goroot_from(Some(bare.as_os_str()), None, &[]),
            None
        );

        let fallback = tmp.path().join("fb");
        std::fs::create_dir_all(fallback.join("src/runtime")).unwrap();
        assert_eq!(
            discover_goroot_from(
                Some(bare.as_os_str()),
                None,
                std::slice::from_ref(&fallback)
            ),
            Some(fallback)
        );
    }

    #[cfg(unix)]
    #[test]
    fn goroot_from_go_executable_on_path() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();

        // Layout A: the real root is the parent of bin.
        put(&base, "a/cellar/root/src/runtime/r.go", "package runtime\n");
        put(&base, "a/cellar/root/bin/go", "");
        std::fs::create_dir_all(base.join("a/bin")).unwrap();
        symlink(base.join("a/cellar/root/bin/go"), base.join("a/bin/go")).unwrap();
        let path = std::env::join_paths([base.join("missing"), base.join("a/bin")]).unwrap();
        assert_eq!(
            discover_goroot_from(None, Some(&path), &[]),
            Some(base.join("a/cellar/root"))
        );

        // Layout B (Homebrew): the root is <parent of bin>/libexec.
        put(
            &base,
            "b/cellar/1.0/libexec/src/runtime/r.go",
            "package runtime\n",
        );
        put(&base, "b/cellar/1.0/bin/go", "");
        std::fs::create_dir_all(base.join("b/bin")).unwrap();
        symlink(base.join("b/cellar/1.0/bin/go"), base.join("b/bin/go")).unwrap();
        let path = std::env::join_paths([base.join("b/bin")]).unwrap();
        assert_eq!(
            discover_goroot_from(None, Some(&path), &[]),
            Some(base.join("b/cellar/1.0/libexec"))
        );
    }

    #[test]
    fn modcache_from_environment() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let explicit = root.join("explicit");
        let gopath = root.join("gopath");
        let home = root.join("home");
        for dir in [
            explicit.clone(),
            gopath.join("pkg/mod"),
            home.join("go/pkg/mod"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let gopaths = std::env::join_paths([&gopath, &root.join("second")]).unwrap();
        assert_eq!(
            discover_modcache_from(
                Some(explicit.as_os_str()),
                Some(&gopaths),
                Some(home.as_os_str())
            ),
            Some(explicit)
        );
        assert_eq!(
            discover_modcache_from(None, Some(&gopaths), Some(home.as_os_str())),
            Some(gopath.join("pkg/mod"))
        );
        assert_eq!(
            discover_modcache_from(Some(OsStr::new("")), None, Some(home.as_os_str())),
            Some(home.join("go/pkg/mod"))
        );
        assert_eq!(
            discover_modcache_from(None, None, Some(root.join("nohome").as_os_str())),
            None
        );
        assert_eq!(discover_modcache_from(None, None, None), None);
    }
}
