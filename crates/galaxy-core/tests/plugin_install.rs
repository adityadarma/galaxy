//! End-to-end plugin install against throwaway local git repositories.

use std::fs;
use std::path::Path;
use std::process::Command;

use galaxy_core::plugins::Catalog;
use galaxy_core::plugins::install::{self, Source, State};
use galaxy_core::plugins::registry;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn manifest(version: &str, extra: &str) -> String {
    format!(
        r#"{{"id":"demo","name":"Demo","version":"{version}","languages":["zig"],
        "server":{{"command":["${{pluginDir}}/bin/demo-ls"]}},
        "install":{{"steps":[["touch","${{pluginDir}}/built"]]}}{extra}}}"#
    )
}

fn repo(manifest_text: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    fs::create_dir_all(dir.path().join("plugins/demo/bin")).unwrap();
    fs::write(
        dir.path().join("plugins/demo/galaxy-plugin.json"),
        manifest_text,
    )
    .unwrap();
    fs::write(dir.path().join("plugins/demo/bin/demo-ls"), "#!/bin/sh\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "one"]);
    dir
}

fn source(repo: &Path, rev: &str) -> Source {
    Source::parse(&format!("file://{}#{rev}:plugins/demo", repo.display())).unwrap()
}

#[test]
fn install_update_and_uninstall_from_git() {
    let remote = repo(&manifest("1.0.0", ""));
    git(remote.path(), &["tag", "v1"]);
    let plugins = tempfile::tempdir().unwrap();
    let dir = plugins.path();

    let pending = install::fetch(dir, &source(remote.path(), "v1"), None).unwrap();
    assert_eq!(pending.manifest.id, "demo");
    assert!(pending.previous_commit.is_none());
    assert!(
        !dir.join("demo").exists(),
        "fetch must not install anything"
    );

    install::install(dir, &pending.token).unwrap();
    assert!(
        dir.join("demo/built").is_file(),
        "install steps ran in the plugin folder"
    );
    assert!(!dir.join(".staging").join(&pending.token).exists());
    let state = State::load(dir).unwrap();
    assert_eq!(state.installed["demo"].commit, pending.commit);

    // The installed plugin shows up in the catalog and serves its language.
    let (catalog, errors) = Catalog::load(dir);
    assert!(errors.is_empty(), "{errors:?}");
    let find = |p: &str| Path::new(p).is_file().then(|| p.into());
    let (plugin, config) = catalog.primary("zig", None, &find).unwrap();
    assert_eq!(plugin.manifest.id, "demo");
    assert!(config.program.ends_with("demo/bin/demo-ls"));

    // Branch tracking: no update until upstream moves.
    let tracking = Source::parse(&format!(
        "file://{}#main:plugins/demo",
        remote.path().display()
    ))
    .unwrap();
    let again = install::fetch(dir, &tracking, None).unwrap();
    assert_eq!(
        again.previous_commit.as_deref(),
        Some(pending.commit.as_str())
    );
    install::install(dir, &again.token).unwrap();
    assert_eq!(install::check_update(dir, "demo").unwrap(), None);

    fs::write(
        remote.path().join("plugins/demo/galaxy-plugin.json"),
        manifest("1.1.0", ""),
    )
    .unwrap();
    git(remote.path(), &["commit", "-qam", "two"]);
    let latest = install::check_update(dir, "demo")
        .unwrap()
        .expect("update available");
    assert_eq!(latest, git(remote.path(), &["rev-parse", "HEAD"]));

    // A commit elsewhere in the repository is not an update for this plugin.
    let updated = install::fetch(dir, &tracking, None).unwrap();
    install::install(dir, &updated.token).unwrap();
    fs::write(remote.path().join("README"), "x").unwrap();
    git(remote.path(), &["add", "."]);
    git(remote.path(), &["commit", "-qm", "unrelated"]);
    assert_eq!(install::check_update(dir, "demo").unwrap(), None);

    install::set_enabled(dir, "demo", false).unwrap();
    let (catalog, _) = Catalog::load(dir);
    assert!(catalog.primary("zig", None, &find).is_none());

    install::uninstall(dir, "demo").unwrap();
    assert!(!dir.join("demo").exists());
    assert!(State::load(dir).unwrap().installed.is_empty());
    assert!(install::uninstall(dir, "demo").is_err());
}

#[test]
fn failed_install_leaves_the_previous_version_in_place() {
    let remote = repo(&manifest("1.0.0", ""));
    let plugins = tempfile::tempdir().unwrap();
    let dir = plugins.path();
    let good = install::fetch(dir, &source(remote.path(), "main"), None).unwrap();
    install::install(dir, &good.token).unwrap();

    let broken = manifest("2.0.0", "").replace("\"touch\"", "\"definitely-not-a-program\"");
    fs::write(
        remote.path().join("plugins/demo/galaxy-plugin.json"),
        broken,
    )
    .unwrap();
    git(remote.path(), &["commit", "-qam", "broken"]);
    let pending = install::fetch(dir, &source(remote.path(), "main"), None).unwrap();
    let error = install::install(dir, &pending.token).unwrap_err();
    assert!(error.contains("definitely-not-a-program"), "{error}");
    assert!(dir.join("demo/built").is_file(), "old version restored");
    assert_eq!(State::load(dir).unwrap().installed["demo"].version, "1.0.0");
    assert!(!dir.join(".staging").join(&pending.token).exists());
}

#[test]
fn checksums_are_enforced() {
    let plugins = tempfile::tempdir().unwrap();
    let dir = plugins.path();

    // Tree hash pinned by a registry.
    let remote = repo(&manifest("1.0.0", ""));
    let src = source(remote.path(), "main");
    let wrong = "0".repeat(64);
    let error = install::fetch(dir, &src, Some(&wrong)).unwrap_err();
    assert!(error.contains("checksum mismatch"), "{error}");
    let real = install::fetch(dir, &src, None).unwrap().tree_sha256;
    install::fetch(dir, &src, Some(&real)).unwrap();

    // File checksum declared by the manifest.
    let bad = repo(&manifest(
        "1.0.0",
        &format!(r#","checksums":{{"bin/demo-ls":"{wrong}"}}"#),
    ));
    let pending = install::fetch(dir, &source(bad.path(), "main"), None).unwrap();
    let error = install::install(dir, &pending.token).unwrap_err();
    assert!(
        error.contains("checksum mismatch for bin/demo-ls"),
        "{error}"
    );
    assert!(!dir.join("demo").exists());

    // Download checksum: file:// URL, wrong hash.
    let payload = tempfile::NamedTempFile::new().unwrap();
    fs::write(payload.path(), "payload").unwrap();
    let extra = format!(
        r#","install":{{"downloads":[{{"url":"file://{}","sha256":"{wrong}","dest":"bin/x"}}]}}"#,
        payload.path().display()
    );
    let text = manifest("1.0.0", "").replace(
        r#""install":{"steps":[["touch","${pluginDir}/built"]]}"#,
        &extra[1..],
    );
    let dl = repo(&text);
    let pending = install::fetch(dir, &source(dl.path(), "main"), None).unwrap();
    let error = install::install(dir, &pending.token).unwrap_err();
    assert!(error.contains("checksum mismatch"), "{error}");
    assert!(!dir.join("demo").exists());
}

#[test]
fn registry_in_a_git_repo_lists_and_pins_plugins() {
    let remote = repo(&manifest("1.0.0", ""));
    let hash = install::hash_tree(&remote.path().join("plugins/demo")).unwrap();
    fs::write(
        remote.path().join("index.json"),
        format!(r#"{{"plugins":[{{"id":"demo","name":"Demo","version":"1.0.0","languages":["zig"],"path":"plugins/demo","sha256":"{hash}"}}]}}"#),
    )
    .unwrap();
    git(remote.path(), &["add", "."]);
    git(remote.path(), &["commit", "-qm", "index"]);

    let spec = format!("file://{}#main", remote.path().display());
    let entries = registry::fetch_entries(&spec).unwrap();
    assert_eq!(entries.len(), 1);
    let plugins = tempfile::tempdir().unwrap();
    let entry = &entries[0];
    let pending = install::fetch(plugins.path(), &entry.source, entry.sha256.as_deref()).unwrap();
    install::install(plugins.path(), &pending.token).unwrap();
    assert!(plugins.path().join("demo/built").is_file());

    // A plain folder works as a registry too.
    let folder = tempfile::tempdir().unwrap();
    fs::write(folder.path().join("index.json"), r#"{"plugins":[]}"#).unwrap();
    assert!(
        registry::fetch_entries(&folder.path().to_string_lossy())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn repository_plugins_and_index_agree() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let index = fs::read_to_string(root.join("index.json")).unwrap();
    let own = Source::parse("https://example.com/plugins.git").unwrap();
    let entries = galaxy_core::plugins::registry::parse_index(&index, &own, "repo").unwrap();
    assert!(!entries.is_empty());
    for entry in entries {
        // Index paths are relative to the repository root, one level above `plugins/`.
        let folder = root
            .join("..")
            .join(entry.source.subdir.as_deref().unwrap());
        let manifest = galaxy_core::plugins::Manifest::parse(
            &fs::read_to_string(folder.join("galaxy-plugin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.id, entry.id);
        assert_eq!(
            manifest.version, entry.version,
            "{} version in index.json",
            entry.id
        );
    }
}

/// Installs the repository's own `laravel` plugin with composer. Needs network
/// and composer: `cargo test -p galaxy-core --test plugin_install -- --ignored`.
#[test]
#[ignore = "needs network and composer"]
fn real_laravel_plugin_installs_and_resolves() {
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "-b", "main"]);
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/laravel");
    fs::create_dir_all(remote.path().join("plugins/laravel")).unwrap();
    fs::copy(
        from.join("galaxy-plugin.json"),
        remote.path().join("plugins/laravel/galaxy-plugin.json"),
    )
    .unwrap();
    git(remote.path(), &["add", "."]);
    git(remote.path(), &["commit", "-q", "-m", "laravel"]);

    let plugins = tempfile::tempdir().unwrap();
    let spec = format!("file://{}#main:plugins/laravel", remote.path().display());
    let pending = install::fetch(plugins.path(), &Source::parse(&spec).unwrap(), None).unwrap();
    install::install(plugins.path(), &pending.token).unwrap();

    let (catalog, errors) = Catalog::load(plugins.path());
    assert!(errors.is_empty(), "{errors:?}");
    let laravel = catalog.get("laravel").unwrap();
    assert!(!laravel.builtin, "installed copy replaces the built-in");
    let config = laravel
        .server_config(&galaxy_core::lsp::find_in_path)
        .unwrap();
    assert!(
        config
            .program
            .starts_with(&*plugins.path().to_string_lossy())
            && config.program.ends_with("vendor/bin/laravel-lsp"),
        "{}",
        config.program
    );
}

#[test]
fn builtin_plugins_install_locally_only_on_request() {
    let plugins = tempfile::tempdir().unwrap();
    let dir = plugins.path();
    // Nothing is created until fetch + install.
    let (catalog, _) = Catalog::load(dir);
    assert!(catalog.get("phpactor").unwrap().builtin);

    // Every built-in is installed on request; unknown ids are rejected.
    for id in ["rust-analyzer", "gopls", "pyright", "vue-language-server"] {
        let pending = install::fetch_builtin(dir, id).unwrap();
        install::discard(dir, &pending.token).unwrap();
    }
    assert!(install::fetch_builtin(dir, "no-such-plugin").is_err());

    // Staging alone changes nothing; discarding removes it.
    let pending = install::fetch_builtin(dir, "phpactor").unwrap();
    assert!(pending.manifest.install_steps.len() >= 3);
    assert!(!dir.join("phpactor").exists());
    install::discard(dir, &pending.token).unwrap();
    assert!(!dir.join(".staging").join(&pending.token).exists());
    let (catalog, _) = Catalog::load(dir);
    assert!(catalog.get("phpactor").unwrap().builtin);
    assert!(State::load(dir).unwrap().installed.is_empty());

    // Installed copy of a built-in never reports updates, and uninstall
    // returns to the built-in.
    let mut state = State::load(dir).unwrap();
    state.installed.insert(
        "gopls".into(),
        install::Installed {
            source: Source::builtin("gopls"),
            commit: "builtin".into(),
            version: "0.22.0".into(),
            tree_sha256: "x".into(),
            installed_at: 0,
        },
    );
    state.save(dir).unwrap();
    assert_eq!(install::check_update(dir, "gopls").unwrap(), None);
}

/// Runs the real npm install for a built-in. Needs network and npm:
/// `cargo test -p galaxy-core --test plugin_install -- --ignored`.
#[test]
#[ignore = "needs network and npm"]
fn real_pyright_builtin_installs_and_takes_over() {
    let plugins = tempfile::tempdir().unwrap();
    let dir = plugins.path();
    let pending = install::fetch_builtin(dir, "pyright").unwrap();
    install::install(dir, &pending.token).unwrap();
    let (catalog, errors) = Catalog::load(dir);
    assert!(errors.is_empty(), "{errors:?}");
    let plugin = catalog.get("pyright").unwrap();
    assert!(!plugin.builtin);
    let config = plugin
        .server_config(&galaxy_core::lsp::find_in_path)
        .unwrap();
    assert!(
        config.program.starts_with(&*dir.to_string_lossy()),
        "{}",
        config.program
    );
    install::uninstall(dir, "pyright").unwrap();
    assert!(Catalog::load(dir).0.get("pyright").unwrap().builtin);
}

/// A `gz` download is decompressed to a single executable file.
#[test]
fn gz_download_installs_an_executable() {
    let work = tempfile::tempdir().unwrap();
    let raw = work.path().join("demo-ls");
    fs::write(&raw, "#!/bin/sh\necho hi\n").unwrap();
    assert!(
        Command::new("gzip")
            .arg("-k")
            .arg(&raw)
            .status()
            .unwrap()
            .success()
    );
    let gz = work.path().join("demo-ls.gz");
    let sum = Command::new("shasum")
        .args(["-a", "256"])
        .arg(&gz)
        .output()
        .unwrap();
    let sha = String::from_utf8(sum.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let extra = format!(
        r#""install":{{"downloads":[{{"platform":"*","url":"file://{}","sha256":"{sha}","dest":"bin/demo-ls","archive":"gz","executable":true}}]}}"#,
        gz.display()
    );
    let text = manifest("1.0.0", "").replace(
        r#""install":{"steps":[["touch","${pluginDir}/built"]]}"#,
        &extra,
    );
    let remote = repo(&text);
    let plugins = tempfile::tempdir().unwrap();
    let dir = plugins.path();
    let pending = install::fetch(dir, &source(remote.path(), "main"), None).unwrap();
    install::install(dir, &pending.token).unwrap();

    let binary = dir.join("demo/bin/demo-ls");
    assert_eq!(fs::read_to_string(&binary).unwrap(), "#!/bin/sh\necho hi\n");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(&binary).unwrap().permissions().mode() & 0o111,
        0o111
    );
}
