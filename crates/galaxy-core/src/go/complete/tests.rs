use super::*;
use crate::go::diag::fixture::{Fx, apply};
use crate::go::parse::parse;
use crate::lang::Edit;

const MODELS: &str = "package models\n\nconst Max = 10\nconst hidden = 1\n\nvar Default = 3\n\ntype Account struct {\n\tID     int\n\tsecret string\n}\n\nfunc (a *Account) Save(force bool) error { return nil }\nfunc (a Account) Label() string { return \"\" }\nfunc (a Account) internal() {}\n\ntype Store interface {\n\tGet(id int) Account\n}\n\nfunc New(name string) *Account { return nil }\nfunc Reset() {}\nfunc helper() {}\n";

const USER: &str = "type User struct {\n\tName string\n\tage  int\n}\n\nfunc (u *User) Greet(greeting string) string { return \"\" }\nfunc (u User) Id() int { return 0 }\n\n";

/// Completes at the `|` marker of `marked`, written as `main.go` of package
/// main next to `files`.
fn run(files: &[(&str, &str)], marked: &str) -> (String, Vec<Completion>) {
    let mut fx = Fx::new(files);
    let at = marked.find('|').expect("cursor marker");
    let src = format!("{}{}", &marked[..at], &marked[at + 1..]);
    let line = marked[..at].matches('\n').count() as u32;
    let start = marked[..at].rfind('\n').map_or(0, |i| i + 1);
    let column = marked[start..at].encode_utf16().count() as u32;
    let path = fx.main();
    let (_, tree) = parse(&path, &src).expect("parse");
    let found = complete(&mut fx.engine, &path, &src, &tree, line, column);
    (src, found)
}

fn body(imports: &str, decls: &str, stmts: &str) -> String {
    format!("package main\n\n{imports}\n{decls}\nfunc main() {{\n{stmts}\n}}\n")
}

fn labels(found: &[Completion]) -> Vec<&str> {
    found.iter().map(|c| c.label.as_str()).collect()
}

fn get<'a>(found: &'a [Completion], label: &str) -> &'a Completion {
    found
        .iter()
        .find(|c| c.label == label)
        .unwrap_or_else(|| panic!("no {label:?} in {:?}", labels(found)))
}

fn has(found: &[Completion], label: &str) -> bool {
    found.iter().any(|c| c.label == label)
}

fn pos(found: &[Completion], label: &str) -> usize {
    found
        .iter()
        .position(|c| c.label == label)
        .unwrap_or_else(|| panic!("no {label:?} in {:?}", labels(found)))
}

fn applied(src: &str, c: &Completion) -> String {
    let edits: Vec<Edit> = c
        .extra
        .iter()
        .map(|i| Edit {
            start_line: i.line,
            start_column: i.column,
            end_line: i.line,
            end_column: i.column,
            text: i.text.clone(),
        })
        .collect();
    apply(src, &edits)
}

fn user(stmts: &str) -> String {
    body("", USER, stmts)
}

// ---- members ----------------------------------------------------------

#[test]
fn member_fields_and_methods() {
    let (_, found) = run(&[], &user("\tu := User{}\n\tu.|"));
    for name in ["Name", "age", "Greet", "Id"] {
        assert!(has(&found, name), "{name} in {:?}", labels(&found));
    }
}

#[test]
fn member_kinds_and_details() {
    let (_, found) = run(&[], &user("\tu := User{}\n\tu.|"));
    assert_eq!(get(&found, "Name").kind, ItemKind::Property);
    assert_eq!(get(&found, "Name").detail.as_deref(), Some("string"));
    assert_eq!(get(&found, "Greet").kind, ItemKind::Method);
    assert_eq!(
        get(&found, "Greet").detail.as_deref(),
        Some("(greeting string) string")
    );
}

#[test]
fn member_insert_text_and_snippets() {
    let (_, found) = run(&[], &user("\tu := User{}\n\tu.|"));
    let name = get(&found, "Name");
    assert_eq!(name.insert_text, "Name");
    assert!(!name.is_snippet);
    let greet = get(&found, "Greet");
    assert_eq!(greet.insert_text, "Greet($1)");
    assert!(greet.is_snippet);
    let id = get(&found, "Id");
    assert_eq!(id.insert_text, "Id()");
    assert!(!id.is_snippet);
    assert_eq!(id.replace_from, None);
}

#[test]
fn pointer_receiver_methods_are_not_hidden() {
    let (_, found) = run(&[], &user("\tu := User{}\n\tu.|"));
    assert!(has(&found, "Greet"));
    let (_, found) = run(&[], &user("\tu := &User{}\n\tu.|"));
    assert!(has(&found, "Greet") && has(&found, "Id"));
}

#[test]
fn member_prefix_is_case_sensitive() {
    let (_, found) = run(&[], &user("\tu := User{}\n\tu.Na|"));
    assert_eq!(labels(&found).first(), Some(&"Name"));
    assert!(!has(&found, "Greet"));
    let (_, found) = run(&[], &user("\tu := User{}\n\tu.na|"));
    assert!(!has(&found, "Name"));
}

#[test]
fn member_matches_only_prefix() {
    let decls = "type T struct {\n\tWidth int\n\tId    int\n\tTitle string\n\tZed   int\n}\n";
    let (_, found) = run(&[], &body("", decls, "\tt := T{}\n\tt.i|"));
    assert!(!has(&found, "Id"));
    assert!(!has(&found, "Title"));
    assert!(!has(&found, "Width"));
}

#[test]
fn subsequence_does_not_match() {
    let (_, found) = run(&[], &user("\tu := User{}\n\tu.gt|"));
    assert!(!has(&found, "Greet"));
}

#[test]
fn promoted_members() {
    let decls = "type Base struct{ Created int }\n\nfunc (b *Base) Touch() {}\n\ntype Doc struct {\n\tBase\n\tTitle string\n}\n";
    let (_, found) = run(&[], &body("", decls, "\td := Doc{}\n\td.|"));
    for name in ["Base", "Title", "Created", "Touch"] {
        assert!(has(&found, name), "{name} in {:?}", labels(&found));
    }
}

#[test]
fn unexported_hidden_across_packages() {
    let main = body(
        "import \"example.com/app/models\"",
        "",
        "\ta := models.Account{}\n\ta.|",
    );
    let (_, found) = run(&[("models/models.go", MODELS)], &main);
    for name in ["ID", "Save", "Label"] {
        assert!(has(&found, name), "{name} in {:?}", labels(&found));
    }
    assert!(!has(&found, "secret") && !has(&found, "internal"));
}

#[test]
fn unexported_shown_in_same_package_other_file() {
    let other = "package main\n\ntype Acc struct {\n\tID     int\n\tsecret string\n}\n";
    let (_, found) = run(&[("acc.go", other)], &body("", "", "\ta := Acc{}\n\ta.|"));
    assert!(has(&found, "secret") && has(&found, "ID"));
}

#[test]
fn package_receiver_lists_exported_names() {
    let main = body("import \"example.com/app/models\"", "", "\tmodels.|");
    let (_, found) = run(&[("models/models.go", MODELS)], &main);
    for name in ["Max", "Default", "Account", "Store", "New", "Reset"] {
        assert!(has(&found, name), "{name} in {:?}", labels(&found));
    }
    for name in ["hidden", "helper", "Save", "Label"] {
        assert!(!has(&found, name), "{name} listed");
    }
}

#[test]
fn package_item_kinds() {
    let main = body("import \"example.com/app/models\"", "", "\tmodels.|");
    let (_, found) = run(&[("models/models.go", MODELS)], &main);
    assert_eq!(get(&found, "Account").kind, ItemKind::Class);
    assert_eq!(get(&found, "Store").kind, ItemKind::Interface);
    assert_eq!(get(&found, "Max").kind, ItemKind::Constant);
    assert_eq!(get(&found, "Default").kind, ItemKind::Variable);
    assert_eq!(get(&found, "New").kind, ItemKind::Function);
}

#[test]
fn package_functions_insert_calls() {
    let main = body("import \"example.com/app/models\"", "", "\tmodels.|");
    let (_, found) = run(&[("models/models.go", MODELS)], &main);
    let new = get(&found, "New");
    assert_eq!(new.insert_text, "New($1)");
    assert!(new.is_snippet);
    assert_eq!(new.detail.as_deref(), Some("(name string) *Account"));
    let reset = get(&found, "Reset");
    assert_eq!(reset.insert_text, "Reset()");
    assert!(!reset.is_snippet);
}

#[test]
fn package_prefix_filters() {
    let main = body("import \"example.com/app/models\"", "", "\tmodels.Ne|");
    let (_, found) = run(&[("models/models.go", MODELS)], &main);
    assert_eq!(labels(&found).first(), Some(&"New"));
    assert!(!has(&found, "Reset") || pos(&found, "New") < pos(&found, "Reset"));
}

#[test]
fn error_has_error_method() {
    let decls = "func work() error { return nil }\n";
    let (_, found) = run(&[], &body("", decls, "\terr := work()\n\terr.|"));
    assert_eq!(labels(&found), ["Error"]);
}

#[test]
fn chained_call_receiver() {
    let decls = format!("{USER}func mk() *User {{ return nil }}\n");
    let (_, found) = run(&[], &body("", &decls, "\tmk().|"));
    assert!(has(&found, "Name") && has(&found, "Greet"));
}

#[test]
fn member_inside_call_arguments() {
    let (_, found) = run(&[], &user("\tu := User{}\n\tprintln(u.|)"));
    assert!(has(&found, "Name"));
}

#[test]
fn member_capped_at_200() {
    let mut text = String::from("package models\n\n");
    for i in 0..300 {
        text.push_str(&format!("func Fn{i:03}() {{}}\n"));
    }
    let main = body("import \"example.com/app/models\"", "", "\tmodels.|");
    let (_, found) = run(&[("models/many.go", &text)], &main);
    assert_eq!(found.len(), 200);
}

#[test]
fn sorted_by_sort_key() {
    let (_, found) = run(&[], &user("\tu := User{}\n\tu.|"));
    let keys: Vec<&str> = found.iter().map(|c| c.sort.as_str()).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

#[test]
fn utf16_columns() {
    let (_, found) = run(&[], &user("\tu := User{}\n\ts := \"😀é\"; _ = s; u.|"));
    assert!(has(&found, "Name"));
}

// ---- incomplete code --------------------------------------------------

#[test]
fn dot_at_end_of_file() {
    let (_, found) = run(
        &[],
        &format!("package main\n\n{USER}func main() {{\n\tu := User{{}}\n\tu.|"),
    );
    assert!(has(&found, "Name"));
}

#[test]
fn dot_in_unclosed_call() {
    let (_, found) = run(
        &[],
        &format!("package main\n\n{USER}func main() {{\n\tu := User{{}}\n\tfoo(u.|"),
    );
    assert!(has(&found, "Name"));
}

#[test]
fn dot_in_if_condition() {
    let (_, found) = run(
        &[],
        &format!("package main\n\n{USER}func main() {{\n\tu := User{{}}\n\tif u.|"),
    );
    assert!(has(&found, "Name"));
}

#[test]
fn dot_with_partial_name_in_broken_code() {
    let (_, found) = run(
        &[],
        &format!("package main\n\n{USER}func main() {{\n\tu := User{{}}\n\tx := (u.Na|"),
    );
    assert_eq!(labels(&found).first(), Some(&"Name"));
}

// ---- nothing -----------------------------------------------------------

#[test]
fn nothing_in_line_comment() {
    let (_, found) = run(&[], &user("\tu := User{}\n\t// u.|"));
    assert!(found.is_empty());
    let (_, found) = run(&[], &user("\t// fo|"));
    assert!(found.is_empty());
}

#[test]
fn nothing_in_block_comment() {
    let (_, found) = run(&[], &user("\t/* u.|"));
    assert!(found.is_empty());
}

#[test]
fn nothing_in_strings() {
    let (_, found) = run(&[], &user("\ts := \"u.|"));
    assert!(found.is_empty());
    let (_, found) = run(&[], &user("\ts := `fo|"));
    assert!(found.is_empty());
    let (_, found) = run(&[], &user("\ts := 'a|"));
    assert!(found.is_empty());
}

#[test]
fn nothing_after_number() {
    let (_, found) = run(&[], &user("\tx := 1.|"));
    assert!(found.is_empty());
    let (_, found) = run(&[], &user("\tx := 12|"));
    assert!(found.is_empty());
}

#[test]
fn comment_marker_inside_string_does_not_hide_code() {
    let (_, found) = run(&[], &user("\ts := \"//\"\n\tu := User{}\n\tu.|"));
    assert!(has(&found, "Name"));
}

// ---- identifiers ------------------------------------------------------

#[test]
fn locals_params_and_package_level() {
    let decls = "var global = 1\n\nfunc helper(a int) int { return a }\n\n";
    let (_, found) = run(&[], &body("", decls, "\tlocal := 1\n\t|"));
    assert!(has(&found, "local") && has(&found, "global") && has(&found, "helper"));
    assert_eq!(get(&found, "local").kind, ItemKind::Variable);
    assert_eq!(get(&found, "helper").kind, ItemKind::Function);
    assert_eq!(get(&found, "helper").insert_text, "helper($1)");
}

#[test]
fn function_parameters_are_locals() {
    let src = "package main\n\nfunc f(count int, name string) {\n\t|\n}\n";
    let (_, found) = run(&[], src);
    assert_eq!(get(&found, "count").detail.as_deref(), Some("int"));
    assert!(has(&found, "name"));
}

#[test]
fn locals_not_yet_declared_are_absent() {
    let (_, found) = run(&[], &body("", "", "\t|\n\tlater := 1\n\t_ = later"));
    assert!(!has(&found, "later"));
}

#[test]
fn tiers_order_locals_package_imports_universe_keywords() {
    let decls = "var pkgvar = 1\n";
    let (_, found) = run(
        &[],
        &body(
            "import \"fmt\"",
            decls,
            "\tlocalvar := 1\n\t_ = localvar\n\t|",
        ),
    );
    let order = [
        pos(&found, "localvar"),
        pos(&found, "pkgvar"),
        pos(&found, "fmt"),
        pos(&found, "int"),
        pos(&found, "func"),
    ];
    let mut sorted = order;
    sorted.sort();
    assert_eq!(order, sorted, "{:?}", labels(&found));
}

#[test]
fn non_prefix_is_excluded_across_tiers() {
    let decls = "var xnet = 1\nvar network = 2\n";
    let (_, found) = run(&[], &body("", decls, "\tnet|"));
    assert!(has(&found, "network"));
    assert!(!has(&found, "xnet"));
}

#[test]
fn imports_listed_with_package_detail() {
    let (_, found) = run(&[], &body("import \"fmt\"", "", "\tfm|"));
    let fmt = get(&found, "fmt");
    assert_eq!(fmt.kind, ItemKind::Class);
    assert_eq!(fmt.detail.as_deref(), Some("package fmt"));
    assert!(fmt.extra.is_empty());
}

#[test]
fn universe_names_and_keywords() {
    let (_, found) = run(&[], &body("", "", "\t|"));
    for name in [
        "int", "string", "error", "any", "true", "nil", "len", "append",
    ] {
        assert!(has(&found, name), "{name}");
    }
    assert_eq!(get(&found, "len").kind, ItemKind::Function);
    assert_eq!(get(&found, "true").kind, ItemKind::Constant);
    assert_eq!(get(&found, "int").kind, ItemKind::Class);
    let kw = get(&found, "func");
    assert_eq!(kw.kind, ItemKind::Keyword);
    assert_eq!(kw.insert_text, "func");
    for name in ["if", "for", "switch", "select", "go", "defer", "return"] {
        assert_eq!(get(&found, name).kind, ItemKind::Keyword);
    }
}

#[test]
fn local_shadows_outer_name() {
    let decls = "var count = 1\n";
    let src = body(
        "",
        decls,
        "\tcount := \"text\"\n\t_ = count\n\t{\n\t\tcount := 2.5\n\t\t_ = count\n\t\tco|\n\t}",
    );
    let (_, found) = run(&[], &src);
    assert_eq!(found.iter().filter(|c| c.label == "count").count(), 1);
    assert_eq!(get(&found, "count").kind, ItemKind::Variable);
    assert_eq!(get(&found, "count").detail.as_deref(), Some("2.5"));
}

#[test]
fn local_shadows_package_level_and_import() {
    let decls = "func shadow() {}\n";
    let src = body(
        "import \"fmt\"",
        decls,
        "\tshadow := 1\n\tfmt := 2\n\t_, _ = shadow, fmt\n\t|",
    );
    let (_, found) = run(&[], &src);
    assert_eq!(found.iter().filter(|c| c.label == "shadow").count(), 1);
    assert_eq!(get(&found, "shadow").kind, ItemKind::Variable);
    assert_eq!(found.iter().filter(|c| c.label == "fmt").count(), 1);
    assert_eq!(get(&found, "fmt").kind, ItemKind::Variable);
}

#[test]
fn package_level_from_other_files_unexported_included() {
    let other = "package main\n\nfunc secretHelper() {}\n\nconst limit = 3\n";
    let (_, found) = run(&[("other.go", other)], &body("", "", "\tsec|"));
    assert!(has(&found, "secretHelper"));
    let (_, found) = run(&[("other.go", other)], &body("", "", "\tlim|"));
    assert_eq!(get(&found, "limit").kind, ItemKind::Constant);
}

// ---- auto-import -------------------------------------------------------

#[test]
fn auto_import_offers_std_package() {
    let (_, found) = run(&[], &body("", "", "\tstr|"));
    let item = get(&found, "strings");
    assert_eq!(item.detail.as_deref(), Some("import \"strings\""));
    assert_eq!(item.insert_text, "strings");
    assert_eq!(item.extra.len(), 1);
}

#[test]
fn auto_import_edit_makes_valid_source() {
    let (src, found) = run(&[], &body("", "", "\tstr|"));
    let out = applied(&src, get(&found, "strings"));
    assert!(out.contains("import \"strings\""), "{out}");
    let (file, tree) = parse(Path::new("main.go"), &out).unwrap();
    assert!(!tree.root_node().has_error(), "{out}");
    assert!(file.imports.iter().any(|i| i.path == "strings"));
}

#[test]
fn auto_import_into_existing_block() {
    let imports = "import (\n\t\"fmt\"\n\t\"os\"\n)\n";
    let (src, found) = run(&[], &body(imports, "", "\tstr|"));
    let out = applied(&src, get(&found, "strings"));
    let (file, tree) = parse(Path::new("main.go"), &out).unwrap();
    assert!(!tree.root_node().has_error(), "{out}");
    let paths: Vec<&str> = file.imports.iter().map(|i| i.path.as_str()).collect();
    assert_eq!(paths, ["fmt", "os", "strings"], "{out}");
}

#[test]
fn auto_import_next_to_single_import() {
    let (src, found) = run(&[], &body("import \"fmt\"", "", "\tstr|"));
    let out = applied(&src, get(&found, "strings"));
    let (file, tree) = parse(Path::new("main.go"), &out).unwrap();
    assert!(!tree.root_node().has_error(), "{out}");
    let paths: Vec<&str> = file.imports.iter().map(|i| i.path.as_str()).collect();
    assert_eq!(paths, ["fmt", "strings"], "{out}");
}

#[test]
fn auto_import_skipped_when_imported() {
    let (_, found) = run(&[], &body("import \"strings\"", "", "\tstr|"));
    let all: Vec<_> = found.iter().filter(|c| c.label == "strings").collect();
    assert_eq!(all.len(), 1);
    assert!(all[0].extra.is_empty());
}

#[test]
fn auto_import_skipped_when_local_has_the_name() {
    let (_, found) = run(&[], &body("", "", "\tstrings := 1\n\t_ = strings\n\tstr|"));
    let all: Vec<_> = found.iter().filter(|c| c.label == "strings").collect();
    assert_eq!(all.len(), 1);
    assert!(all[0].extra.is_empty());
}

#[test]
fn auto_import_skipped_when_package_symbol_has_the_name() {
    let (_, found) = run(&[], &body("", "var sort = 1\n", "\tsor|"));
    let all: Vec<_> = found.iter().filter(|c| c.label == "sort").collect();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].kind, ItemKind::Variable);
}

#[test]
fn auto_import_needs_two_characters() {
    let (_, found) = run(&[], &body("", "", "\ts|"));
    assert!(found.iter().all(|c| c.extra.is_empty()));
}

#[test]
fn auto_import_std_before_dependencies_and_short_paths_first() {
    let (_, found) = run(&[], &body("", "", "\trand|"));
    let rands: Vec<&Completion> = found
        .iter()
        .filter(|c| c.label == "rand" && !c.extra.is_empty())
        .collect();
    assert_eq!(rands.len(), 2);
    assert_eq!(rands[0].detail.as_deref(), Some("import \"math/rand\""));
    assert_eq!(rands[1].detail.as_deref(), Some("import \"crypto/rand\""));
}

#[test]
fn auto_import_module_package() {
    let (_, found) = run(&[], &body("", "", "\tli|"));
    let lib = get(&found, "lib");
    assert_eq!(
        lib.detail.as_deref(),
        Some("import \"example.com/app/lib\"")
    );
}

#[test]
fn auto_import_dependency_package() {
    let (_, found) = run(&[], &body("", "", "\tdep|"));
    let dep = found
        .iter()
        .find(|c| c.detail.as_deref() == Some("import \"example.com/dep\""));
    assert!(dep.is_some(), "{:?}", labels(&found));
}

#[test]
fn auto_imports_come_last() {
    let (_, found) = run(&[], &body("", "", "\tfu|"));
    let first = found.iter().position(|c| !c.extra.is_empty());
    if let Some(i) = first {
        assert!(
            found[i..]
                .iter()
                .all(|c| !c.extra.is_empty() || c.label == found[i].label)
        );
    }
    assert!(
        pos(&found, "func")
            < found
                .iter()
                .position(|c| !c.extra.is_empty())
                .unwrap_or(usize::MAX)
    );
}

// ---- import paths ------------------------------------------------------

#[test]
fn import_path_single() {
    let src = "package main\n\nimport \"str|\"\n";
    let (_, found) = run(&[], src);
    assert_eq!(labels(&found).first(), Some(&"strings"));
    assert_eq!(get(&found, "strings").insert_text, "strings");
    assert_eq!(get(&found, "strings").replace_from, Some(8));
}

#[test]
fn import_path_in_block() {
    let src = "package main\n\nimport (\n\t\"fmt\"\n\t\"net/|\"\n)\n";
    let (_, found) = run(&[], src);
    assert!(has(&found, "net/http"));
    assert_eq!(get(&found, "net/http").replace_from, Some(2));
    assert!(!has(&found, "fmt"));
}

#[test]
fn import_path_with_alias() {
    let (_, found) = run(&[], "package main\n\nimport f \"fm|\"\n");
    assert!(has(&found, "fmt"));
    let src = "package main\n\nimport (\n\tf \"fm|\"\n)\n";
    let (_, found) = run(&[], src);
    assert!(has(&found, "fmt"));
}

#[test]
fn import_path_empty_prefix_is_capped() {
    let (_, found) = run(&[], "package main\n\nimport \"|\"\n");
    assert!(found.len() <= 100 && !found.is_empty());
}

#[test]
fn import_path_module_and_dependency() {
    let (_, found) = run(&[], "package main\n\nimport \"example.com/|\"\n");
    assert!(has(&found, "example.com/app/lib"), "{:?}", labels(&found));
    assert!(has(&found, "example.com/dep"));
    assert!(has(&found, "example.com/dep/util"));
}

#[test]
fn import_path_excludes_already_imported() {
    let src = "package main\n\nimport (\n\t\"fmt\"\n\t\"fm|\"\n)\n";
    let (_, found) = run(&[], src);
    assert!(!has(&found, "fmt"));
}

#[test]
fn string_outside_imports_stays_empty() {
    let src = "package main\n\nvar x = \"fm|\"\n";
    let (_, found) = run(&[], src);
    assert!(found.is_empty());
}

// ---- struct literal keys ----------------------------------------------

#[test]
fn literal_keys_inline() {
    let (_, found) = run(&[], &user("\tu := User{|}\n\t_ = u"));
    assert_eq!(
        labels(&found)
            .iter()
            .filter(|l| ["Name", "age"].contains(l))
            .count(),
        2
    );
    let name = get(&found, "Name");
    assert_eq!(name.insert_text, "Name: ");
    assert_eq!(name.kind, ItemKind::Property);
    assert!(!has(&found, "Greet"));
    assert!(!has(&found, "len"));
}

#[test]
fn literal_keys_skip_keyed_fields() {
    let (_, found) = run(
        &[],
        &user("\tu := User{\n\t\tName: \"x\",\n\t\t|\n\t}\n\t_ = u"),
    );
    assert_eq!(labels(&found), ["age"]);
}

#[test]
fn literal_keys_after_comma_same_line() {
    let (_, found) = run(&[], &user("\tu := User{Name: \"x\", a|}\n\t_ = u"));
    assert_eq!(labels(&found), ["age"]);
}

#[test]
fn literal_keys_address_of_and_assignment() {
    let (_, found) = run(&[], &user("\tvar u *User = &User{\n\t\t|\n\t}\n\t_ = u"));
    assert!(has(&found, "Name"));
}

#[test]
fn literal_keys_for_other_package_hide_unexported() {
    let main = body(
        "import \"example.com/app/models\"",
        "",
        "\ta := models.Account{\n\t\t|\n\t}\n\t_ = a",
    );
    let (_, found) = run(&[("models/models.go", MODELS)], &main);
    assert_eq!(labels(&found), ["ID"]);
}

#[test]
fn literal_value_position_is_not_a_key() {
    let (_, found) = run(&[], &user("\tx := 1\n\tu := User{Name: |}\n\t_, _ = u, x"));
    assert!(!found.iter().any(|c| c.insert_text.ends_with(": ")));
    assert!(has(&found, "x"));
}

#[test]
fn slice_literal_elements_are_not_keys() {
    let (_, found) = run(&[], &user("\tus := []User{|}\n\t_ = us"));
    assert!(!found.iter().any(|c| c.insert_text.ends_with(": ")));
    assert!(has(&found, "len"));
}

#[test]
fn block_braces_are_not_literals() {
    let (_, found) = run(
        &[],
        &user("\tu := User{}\n\tif u.Name == \"\" {\n\t\t|\n\t}"),
    );
    assert!(!found.iter().any(|c| c.insert_text.ends_with(": ")));
    assert!(has(&found, "u"));
}

// ---- types ------------------------------------------------------------

fn type_labels(marked: &str) -> Vec<String> {
    let (_, found) = run(&[("models/models.go", MODELS)], marked);
    labels(&found).into_iter().map(str::to_owned).collect()
}

fn assert_types_only(names: &[String]) {
    assert!(names.iter().any(|n| n == "User"), "{names:?}");
    assert!(names.iter().any(|n| n == "int"));
    assert!(names.iter().any(|n| n == "models"));
    for bad in ["len", "if", "true", "local", "helper"] {
        assert!(!names.iter().any(|n| n == bad), "{bad} in {names:?}");
    }
}

#[test]
fn type_after_var_name() {
    let decls = format!("{USER}func helper() {{}}\n");
    let imports = "import \"example.com/app/models\"";
    let names = type_labels(&body(
        imports,
        &decls,
        "\tlocal := 1\n\t_ = local\n\tvar x |",
    ));
    assert_types_only(&names);
}

#[test]
fn type_after_star_and_brackets() {
    let imports = "import \"example.com/app/models\"";
    for stmt in [
        "\tvar p *|",
        "\tvar s []|",
        "\tvar m map[string]|",
        "\tvar m map[|",
        "\tvar a [3]|",
    ] {
        let decls = format!("{USER}func helper() {{}}\n");
        let names = type_labels(&body(
            imports,
            &decls,
            &format!("\tlocal := 1\n\t_ = local\n{stmt}"),
        ));
        assert_types_only(&names);
    }
}

#[test]
fn type_in_function_parameters_and_results() {
    let imports = "import \"example.com/app/models\"";
    for head in [
        "func f(a |",
        "func f(a int, b |",
        "func f(a int) |",
        "func (r User) f(a |",
    ] {
        let src = format!("package main\n\n{imports}\n{USER}func helper() {{}}\n{head}");
        let names = type_labels(&src);
        assert!(
            names.iter().any(|n| n == "User") && !names.iter().any(|n| n == "len"),
            "{head}: {names:?}"
        );
    }
}

#[test]
fn type_in_struct_field() {
    let src = "package main\n\ntype T struct {\n\tName string\n\tOther |\n}\n";
    let names = type_labels(src);
    assert!(names.iter().any(|n| n == "T") && names.iter().any(|n| n == "string"));
    assert!(!names.iter().any(|n| n == "len"));
}

#[test]
fn qualified_type_lists_only_types() {
    let main = body("import \"example.com/app/models\"", "", "\tvar a models.|");
    let (_, found) = run(&[("models/models.go", MODELS)], &main);
    let mut names = labels(&found);
    names.sort();
    assert_eq!(names, ["Account", "Store"]);
}

#[test]
fn qualified_type_after_slice_or_map_prefix() {
    for decl in [
        "\tvar a []models.|",
        "\tvar a [3]models.|",
        "\tvar a map[string][]models.|",
        "\tvar a *models.|",
        "\tvar a []*models.|",
    ] {
        let main = body("import \"example.com/app/models\"", "", decl);
        let (_, found) = run(&[("models/models.go", MODELS)], &main);
        let mut names = labels(&found);
        names.sort();
        assert_eq!(names, ["Account", "Store"], "{decl}");
    }
}

#[test]
fn qualified_type_in_result_list() {
    let main = "package main\n\nimport \"example.com/app/models\"\n\nfunc f() []models.|\n";
    let (_, found) = run(&[("models/models.go", MODELS)], main);
    let mut names = labels(&found);
    names.sort();
    assert_eq!(names, ["Account", "Store"]);
}

#[test]
fn member_on_a_continuation_line_of_an_open_call() {
    let decls = "type Absen struct {\n\tStaffID int\n\tRemarks string\n}\n\nfunc exec(args ...any) {}\n\nfunc add(a Absen) error {\n\texec(\"sql\",\n\t\ta.|\n\tif true {\n\t\treturn nil\n\t}\n\treturn nil\n}\n";
    let main = format!("package main\n\n{decls}\nfunc main() {{}}\n");
    let (_, found) = run(&[], &main);
    for name in ["StaffID", "Remarks"] {
        assert!(has(&found, name), "{name} in {:?}", labels(&found));
    }
}

#[test]
fn member_in_an_open_call_closed_on_the_same_line() {
    let decls = "type Absen struct {\n\tStaffID int\n}\n\nfunc exec(args ...any) {}\n\nfunc add(a Absen) error {\n\texec(\"sql\", a.|)\n\treturn nil\n}\n";
    let main = format!("package main\n\n{decls}\nfunc main() {{}}\n");
    let (_, found) = run(&[], &main);
    assert!(has(&found, "StaffID"), "{:?}", labels(&found));
}

#[test]
fn member_after_a_comma_inside_an_unclosed_call_in_a_method() {
    // Tree-sitter drops the whole declaration into loose tokens here, so the
    // parameters are only found from the tokens of the header.
    let main = "package main\n\ntype Absen struct {\n\tStaffID int\n\tRemarks string\n}\n\ntype repo struct{ db *DB }\n\ntype DB struct{}\n\nfunc (d *DB) Exec(q string, args ...any) (int, error) { return 0, nil }\n\nfunc (r *repo) Add(a Absen) error {\n\tresult, err := r.db.Exec(\"INSERT INTO t (a, b, c) VALUES (?, ?, ?)\",\n\t\ta.|\n\tif err != nil {\n\t\treturn err\n\t}\n\tid, err := result.Last()\n\treturn nil\n}\n\nfunc main() {}\n";
    let (_, found) = run(&[], main);
    for name in ["StaffID", "Remarks"] {
        assert!(has(&found, name), "{name} in {:?}", labels(&found));
    }
}

#[test]
fn walrus_is_not_a_type_position() {
    let (_, found) = run(&[], &user("\tx := 1\n\ty := |\n\t_, _ = x, y"));
    assert!(has(&found, "x") && has(&found, "len") && has(&found, "if"));
}

#[test]
fn declared_names_complete_nothing() {
    let (_, found) = run(&[], &body("", "", "\tvar na|"));
    assert!(found.is_empty());
    let (_, found) = run(&[], "package main\n\nfunc na|\n");
    assert!(found.is_empty());
}

#[test]
fn type_keywords_in_type_position() {
    let (_, found) = run(&[], &body("", "", "\tvar x |"));
    for name in ["struct", "map", "chan", "interface", "func"] {
        assert!(has(&found, name), "{name}");
    }
    assert!(!has(&found, "if"));
}

#[test]
fn type_position_has_no_auto_import_edits() {
    let (_, found) = run(&[], &body("", "", "\tvar x str|"));
    assert!(
        found
            .iter()
            .all(|c| c.kind != ItemKind::Keyword || c.extra.is_empty())
    );
}
