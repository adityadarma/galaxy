use super::*;

fn out(src: &str) -> Vec<OutlineSymbol> {
    outline(Path::new("x.go"), src)
}

fn flat(symbols: &[OutlineSymbol]) -> Vec<(String, &'static str)> {
    symbols.iter().map(|s| (s.name.clone(), s.kind)).collect()
}

fn pairs(list: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
    list.iter().map(|(n, k)| ((*n).to_owned(), *k)).collect()
}

#[test]
fn kinds_of_top_level_declarations() {
    let src = "package x\n\ntype S struct{}\ntype I interface{}\ntype N int\ntype A = string\nfunc F() {}\nconst C = 1\nvar V = 2\n";
    assert_eq!(
        flat(&out(src)),
        pairs(&[
            ("S", "Class"),
            ("I", "Interface"),
            ("N", "Type"),
            ("A", "Type"),
            ("F", "Function"),
            ("C", "Constant"),
            ("V", "Variable"),
        ])
    );
}

#[test]
fn sorted_by_position_across_kinds() {
    let src = "package x\n\nvar V = 1\n\nfunc F() {}\n\ntype T struct{}\n\nconst C = 2\n";
    let names: Vec<String> = out(src).into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["V", "F", "T", "C"]);
}

#[test]
fn struct_fields_are_children() {
    let src = "package x\n\ntype User struct {\n\tName, Nick string\n\tage int\n\tBase\n}\n";
    let symbols = out(src);
    assert_eq!(symbols.len(), 1);
    assert_eq!(
        flat(&symbols[0].children),
        pairs(&[
            ("Name", "Property"),
            ("Nick", "Property"),
            ("age", "Property"),
            ("Base", "Property"),
        ])
    );
    assert_eq!((symbols[0].line, symbols[0].column), (2, 5));
    assert_eq!(
        (symbols[0].children[0].line, symbols[0].children[0].column),
        (3, 1)
    );
}

#[test]
fn interface_methods_are_method_children() {
    let src = "package x\n\ntype R interface {\n\tfmt.Stringer\n\tRead() error\n\tClose()\n}\n";
    let symbols = out(src);
    assert_eq!(
        flat(&symbols[0].children),
        pairs(&[("Read", "Method"), ("Close", "Method")])
    );
}

#[test]
fn methods_nest_under_receiver_in_same_file() {
    let src = "package x\n\ntype S struct{ a int }\n\nfunc (s *S) Run() {}\nfunc (s S) Stop() {}\nfunc Free() {}\n";
    let symbols = out(src);
    assert_eq!(
        flat(&symbols),
        pairs(&[("S", "Class"), ("Free", "Function")])
    );
    assert_eq!(
        flat(&symbols[0].children),
        pairs(&[("a", "Property"), ("Run", "Method"), ("Stop", "Method")])
    );
}

#[test]
fn children_sorted_by_position() {
    // The method comes before the field in the file.
    let src = "package x\n\nfunc (s *S) Early() {}\n\ntype S struct {\n\tf int\n}\n\nfunc (s *S) Late() {}\n";
    let symbols = out(src);
    let s = symbols.iter().find(|s| s.name == "S").unwrap();
    let names: Vec<&str> = s.children.iter().map(|c| c.name.as_str()).collect();
    // `Early` precedes the type in the file but still nests under it.
    assert_eq!(names, ["Early", "f", "Late"]);
    assert_eq!(symbols.len(), 1);
}

#[test]
fn method_of_foreign_type_stays_top_level() {
    let src = "package x\n\nfunc (s *Other) Run() {}\nfunc (g Gen[T]) Do() {}\n";
    assert_eq!(
        flat(&out(src)),
        pairs(&[("Run", "Method"), ("Do", "Method")])
    );
}

#[test]
fn generic_receiver_nests() {
    let src =
        "package x\n\ntype Box[T any] struct{}\n\nfunc (b *Box[T]) Get() T { var z T; return z }\n";
    let symbols = out(src);
    assert_eq!(symbols.len(), 1);
    assert_eq!(flat(&symbols[0].children), pairs(&[("Get", "Method")]));
}

#[test]
fn grouped_const_and_var_give_one_entry_per_name() {
    let src = "package x\n\nconst (\n\tA = iota\n\tB\n\tC\n)\n\nvar (\n\tx, y = 1, 2\n\tz int\n)\n";
    assert_eq!(
        flat(&out(src)),
        pairs(&[
            ("A", "Constant"),
            ("B", "Constant"),
            ("C", "Constant"),
            ("x", "Variable"),
            ("y", "Variable"),
            ("z", "Variable"),
        ])
    );
}

#[test]
fn blank_names_and_empty_file() {
    assert!(out("").is_empty());
    assert!(out("package x\n").is_empty());
    assert!(out("package x\n\nvar _ = 1\ntype _ int\n").is_empty());
}

#[test]
fn survives_syntax_errors() {
    let src = "package x\n\nfunc Good() {}\n\nfunc broken( {\n\nfunc Also() {}\n";
    let names: Vec<String> = out(src).into_iter().map(|s| s.name).collect();
    assert!(names.contains(&"Good".to_owned()));
}

#[test]
fn utf16_columns() {
    let src = "package x\n\nvar s = \"\u{1F600}\"; var Ünï = 1\n";
    let symbols = out(src);
    let v = symbols.iter().find(|s| s.name == "Ünï").unwrap();
    assert_eq!((v.line, v.column), (2, 18));
}

#[test]
fn capped_at_two_thousand() {
    let mut src = String::from("package x\n\n");
    for i in 0..2500 {
        src.push_str(&format!("func f{i}() {{}}\n"));
    }
    assert_eq!(out(&src).len(), 2000);
    let mut fields = String::from("package x\n\ntype T struct {\n");
    for i in 0..2500 {
        fields.push_str(&format!("\tf{i} int\n"));
    }
    fields.push_str("}\n");
    let symbols = out(&fields);
    assert_eq!(symbols.len() + symbols[0].children.len(), 2000);
}
