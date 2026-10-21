use tree_sitter::{Parser, Tree};

use super::*;

const MARK: &str = "/*|*/";

fn parse(src: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .unwrap();
    parser.parse(src, None).unwrap()
}

/// Removes the cursor marker; gives the source and the marker byte.
fn fixture(text: &str) -> (String, usize) {
    let at = text.find(MARK).expect("cursor marker");
    let src = format!("{}{}", &text[..at], &text[at + MARK.len()..]);
    (src, at)
}

fn scope(text: &str) -> Scope {
    let (src, at) = fixture(text);
    scope_at(&parse(&src), &src, at)
}

fn names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = scope(text)
        .names()
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    names.sort();
    names
}

fn has(text: &str, expected: &[&str]) {
    let mut want: Vec<String> = expected.iter().map(|s| (*s).to_owned()).collect();
    want.sort();
    assert_eq!(names(text), want, "{text}");
}

fn declared(ty: &str) -> Origin {
    Origin::Declared(ty.to_owned())
}

fn expr(text: &str, index: usize, count: usize) -> Origin {
    Origin::Expr {
        text: text.to_owned(),
        index,
        count,
    }
}

fn func(text: &str) -> Option<FuncContext> {
    let (src, at) = fixture(text);
    enclosing_func(&parse(&src), &src, at)
}

#[test]
fn params_results_receiver() {
    let s = scope(
        "package p\nfunc (s *Server[T]) Do(a, b int, c string) (n int, err error) {\n\t/*|*/\n}\n",
    );
    let get = |n: &str| s.get(n).unwrap_or_else(|| panic!("{n}"));
    assert_eq!(get("s").kind, BindingKind::Receiver);
    assert_eq!(get("s").origin, declared("*Server[T]"));
    assert_eq!(get("a").kind, BindingKind::Param);
    assert_eq!(get("a").origin, declared("int"));
    assert_eq!(get("b").origin, declared("int"));
    assert_eq!(get("c").origin, declared("string"));
    assert_eq!(get("n").kind, BindingKind::Result);
    assert_eq!(get("err").origin, declared("error"));
}

#[test]
fn variadic_and_unnamed_params() {
    let s = scope("package p\nfunc f(_ int, string, args ...int) {\n\t/*|*/\n}\n");
    assert_eq!(s.names(), ["args"]);
    assert_eq!(s.get("args").unwrap().origin, declared("[]int"));
}

#[test]
fn unnamed_results_are_skipped() {
    has("package p\nfunc f() (int, error) {\n\t/*|*/\n}\n", &[]);
}

#[test]
fn outside_any_function_is_empty() {
    has("package p\nvar x = 1\n/*|*/\nfunc f() { y := 1 }\n", &[]);
}

#[test]
fn short_var_visible_after_statement() {
    let s = scope("package p\nfunc f() {\n\tx := g()\n\t/*|*/\n}\n");
    let x = s.get("x").unwrap();
    assert_eq!(x.kind, BindingKind::ShortVar);
    assert_eq!(x.origin, expr("g()", 0, 1));
    assert_eq!((x.line, x.column, x.end_column), (2, 1, 2));
}

#[test]
fn short_var_not_visible_in_own_statement() {
    has("package p\nfunc f(x int) {\n\ty := /*|*/x + 1\n}\n", &["x"]);
    has("package p\nfunc f() {\n\ty := 1 + /*|*/\n}\n", &[]);
    has(
        "package p\nfunc f() {\n\ty := g(func() { /*|*/ })\n}\n",
        &[],
    );
}

#[test]
fn rhs_sees_outer_binding_when_shadowing() {
    let s = scope("package p\nfunc f(x int) {\n\tx := x + /*|*/1\n}\n");
    assert_eq!(s.get("x").unwrap().kind, BindingKind::Param);
}

#[test]
fn shadowing_nearest_wins_and_names_unique() {
    let s =
        scope("package p\nfunc f(x int) {\n\tx := \"s\"\n\t{\n\t\tx := 2.0\n\t\t/*|*/\n\t}\n}\n");
    assert_eq!(s.names(), ["x"]);
    assert_eq!(s.get("x").unwrap().origin, expr("2.0", 0, 1));
}

#[test]
fn shadow_ends_with_inner_block() {
    let s = scope("package p\nfunc f(x int) {\n\t{\n\t\tx := 2.0\n\t\t_ = x\n\t}\n\t/*|*/\n}\n");
    assert_eq!(s.get("x").unwrap().kind, BindingKind::Param);
}

#[test]
fn multi_value_short_var() {
    let s = scope("package p\nfunc f() {\n\ta, b := g()\n\t/*|*/\n}\n");
    assert_eq!(s.get("a").unwrap().origin, expr("g()", 0, 2));
    assert_eq!(s.get("b").unwrap().origin, expr("g()", 1, 2));
}

#[test]
fn paired_short_var() {
    let s = scope("package p\nfunc f() {\n\ta, b := 1, \"x\"\n\t/*|*/\n}\n");
    assert_eq!(s.get("a").unwrap().origin, expr("1", 0, 1));
    assert_eq!(s.get("b").unwrap().origin, expr("\"x\"", 0, 1));
}

#[test]
fn comma_ok_forms() {
    let s = scope(
        "package p\nfunc f() {\n\tv, ok := x.(T)\n\tm, found := mp[k]\n\tr, open := <-ch\n\t/*|*/\n}\n",
    );
    assert_eq!(s.get("v").unwrap().origin, expr("x.(T)", 0, 2));
    assert_eq!(s.get("ok").unwrap().origin, expr("x.(T)", 1, 2));
    assert_eq!(s.get("found").unwrap().origin, expr("mp[k]", 1, 2));
    assert_eq!(s.get("open").unwrap().origin, expr("<-ch", 1, 2));
    assert_eq!(s.get("r").unwrap().origin, expr("<-ch", 0, 2));
}

#[test]
fn redeclaration_keeps_one_err_and_the_first_origin() {
    let s = scope("package p\nfunc f() {\n\ta, err := f()\n\tb, err := g()\n\t/*|*/\n}\n");
    assert_eq!(s.names(), ["a", "err", "b"]);
    assert_eq!(s.get("err").unwrap().origin, expr("f()", 1, 2));
    assert_eq!(s.get("b").unwrap().origin, expr("g()", 0, 2));
}

#[test]
fn redeclaration_in_inner_block_shadows() {
    let s = scope(
        "package p\nfunc f() {\n\ta, err := f()\n\tif true {\n\t\tb, err := g()\n\t\t/*|*/\n\t}\n}\n",
    );
    assert_eq!(s.get("err").unwrap().origin, expr("g()", 1, 2));
}

#[test]
fn blank_identifier_never_recorded() {
    has(
        "package p\nfunc f(_ int) {\n\t_, b := g()\n\tvar _ = 1\n\tfor _, _ = range x {}\n\t/*|*/\n}\n",
        &["b"],
    );
}

#[test]
fn var_declarations() {
    let s = scope(
        "package p\nfunc f() {\n\tvar a int\n\tvar b, c = 1, 2\n\tvar d, e = g()\n\tvar fn func(int) error\n\tvar (\n\t\tg1 string\n\t\th = k()\n\t)\n\t/*|*/\n}\n",
    );
    assert_eq!(s.get("a").unwrap().origin, declared("int"));
    assert_eq!(s.get("a").unwrap().kind, BindingKind::Var);
    assert_eq!(s.get("b").unwrap().origin, expr("1", 0, 1));
    assert_eq!(s.get("c").unwrap().origin, expr("2", 0, 1));
    assert_eq!(s.get("d").unwrap().origin, expr("g()", 0, 2));
    assert_eq!(s.get("e").unwrap().origin, expr("g()", 1, 2));
    assert_eq!(s.get("fn").unwrap().origin, declared("func(int) error"));
    assert_eq!(s.get("g1").unwrap().origin, declared("string"));
    assert_eq!(s.get("h").unwrap().origin, expr("k()", 0, 1));
}

#[test]
fn var_not_visible_in_own_initializer() {
    has("package p\nfunc f(x int) {\n\tvar y = /*|*/x\n}\n", &["x"]);
    has(
        "package p\nfunc f() {\n\tvar (\n\t\ta = 1\n\t\tb = /*|*/2\n\t)\n}\n",
        &["a"],
    );
}

#[test]
fn typed_var_with_value_keeps_declared_type() {
    let s = scope("package p\nfunc f() {\n\tvar x float64 = 1\n\t/*|*/\n}\n");
    assert_eq!(s.get("x").unwrap().origin, declared("float64"));
}

#[test]
fn const_groups_with_iota() {
    let s = scope(
        "package p\nfunc f() {\n\tconst (\n\t\tA Color = iota\n\t\tB\n\t\tC\n\t)\n\tconst (\n\t\tX = iota\n\t\tY\n\t)\n\tconst N = 10\n\t/*|*/\n}\n",
    );
    for name in ["A", "B", "C"] {
        assert_eq!(s.get(name).unwrap().origin, declared("Color"), "{name}");
        assert_eq!(s.get(name).unwrap().kind, BindingKind::Const);
    }
    assert_eq!(s.get("X").unwrap().origin, expr("iota", 0, 1));
    assert_eq!(s.get("Y").unwrap().origin, expr("iota", 0, 1));
    assert_eq!(s.get("N").unwrap().origin, expr("10", 0, 1));
}

#[test]
fn if_init_visible_in_body_and_else() {
    let head = "package p\nfunc f() {\n\tif v, ok := m[k]; ok {\n";
    let in_body = format!("{head}\t\t/*|*/\n\t}} else {{\n\t}}\n}}\n");
    has(&in_body, &["v", "ok"]);
    let in_else = format!("{head}\t}} else {{\n\t\t/*|*/\n\t}}\n}}\n");
    has(&in_else, &["v", "ok"]);
    let in_else_if = format!("{head}\t}} else if w := v; w {{\n\t\t/*|*/\n\t}}\n}}\n");
    has(&in_else_if, &["v", "ok", "w"]);
}

#[test]
fn if_init_not_visible_after_if() {
    has(
        "package p\nfunc f() {\n\tif v := g(); v {\n\t}\n\t/*|*/\n}\n",
        &[],
    );
}

#[test]
fn if_init_not_visible_inside_its_own_statement() {
    has(
        "package p\nfunc f() {\n\tif v := /*|*/g(); v {\n\t}\n}\n",
        &[],
    );
}

#[test]
fn if_condition_sees_init() {
    has(
        "package p\nfunc f() {\n\tif v := g(); /*|*/v {\n\t}\n}\n",
        &["v"],
    );
}

#[test]
fn for_clause_scope() {
    has(
        "package p\nfunc f() {\n\tfor i := 0; i < 3; i++ {\n\t\t/*|*/\n\t}\n}\n",
        &["i"],
    );
    has(
        "package p\nfunc f() {\n\tfor i := 0; i < 3; i++ {\n\t}\n\t/*|*/\n}\n",
        &[],
    );
    has(
        "package p\nfunc f() {\n\tfor i := 0; /*|*/i < 3; i++ {\n\t}\n}\n",
        &["i"],
    );
}

#[test]
fn range_over_slice_map_int_chan_func() {
    for (expr_text, key_kind) in [
        ("items", BindingKind::RangeKey),
        ("m", BindingKind::RangeKey),
        ("10", BindingKind::RangeKey),
        ("ch", BindingKind::RangeKey),
        ("seq(yield)", BindingKind::RangeKey),
    ] {
        let s = scope(&format!(
            "package p\nfunc f() {{\n\tfor k, v := range {expr_text} {{\n\t\t/*|*/\n\t}}\n}}\n"
        ));
        let k = s.get("k").unwrap();
        let v = s.get("v").unwrap();
        assert_eq!(k.kind, key_kind);
        assert_eq!(v.kind, BindingKind::RangeValue);
        assert_eq!(
            k.origin,
            Origin::RangeOver {
                text: expr_text.to_owned(),
                key: true
            }
        );
        assert_eq!(
            v.origin,
            Origin::RangeOver {
                text: expr_text.to_owned(),
                key: false
            }
        );
    }
}

#[test]
fn range_single_name_and_int() {
    let s = scope("package p\nfunc f() {\n\tfor i := range 10 {\n\t\t/*|*/\n\t}\n}\n");
    assert_eq!(s.names(), ["i"]);
    assert_eq!(
        s.get("i").unwrap().origin,
        Origin::RangeOver {
            text: "10".into(),
            key: true
        }
    );
}

#[test]
fn range_names_not_visible_in_range_expression() {
    has(
        "package p\nfunc f(xs []int) {\n\tfor i := range /*|*/xs {\n\t}\n}\n",
        &["xs"],
    );
}

#[test]
fn range_with_assign_declares_nothing() {
    has(
        "package p\nfunc f() {\n\tvar i int\n\tfor i = range xs {\n\t\t/*|*/\n\t}\n}\n",
        &["i"],
    );
}

#[test]
fn switch_init_and_clause_scope() {
    let src = "package p\nfunc f() {\n\tswitch x := g(); x {\n\tcase 1:\n\t\ta := 1\n\t\t/*|*/\n\tcase 2:\n\t\tb := 2\n\t\t_ = b\n\t}\n}\n";
    has(src, &["x", "a"]);
    let other = "package p\nfunc f() {\n\tswitch x := g(); x {\n\tcase 1:\n\t\ta := 1\n\t\t_ = a\n\tcase 2:\n\t\t/*|*/\n\t}\n}\n";
    has(other, &["x"]);
    let after = "package p\nfunc f() {\n\tswitch x := g(); x {\n\tcase 1:\n\t}\n\t/*|*/\n}\n";
    has(after, &[]);
}

#[test]
fn switch_default_clause() {
    has(
        "package p\nfunc f() {\n\tswitch {\n\tcase a:\n\t\tx := 1\n\t\t_ = x\n\tdefault:\n\t\ty := 2\n\t\t/*|*/\n\t}\n}\n",
        &["y"],
    );
}

#[test]
fn type_switch_single_type_case() {
    let s = scope(
        "package p\nfunc f(x any) {\n\tswitch v := x.(type) {\n\tcase A:\n\t\t/*|*/\n\tcase B, *C:\n\t}\n}\n",
    );
    let v = s.get("v").unwrap();
    assert_eq!(v.kind, BindingKind::TypeSwitch);
    assert_eq!(
        v.origin,
        Origin::TypeSwitch {
            subject: "x".into(),
            case_types: vec!["A".into()]
        }
    );
}

#[test]
fn type_switch_multi_type_and_default() {
    let multi = scope(
        "package p\nfunc f(x any) {\n\tswitch v := x.(type) {\n\tcase A:\n\tcase B, *C:\n\t\t/*|*/\n\t}\n}\n",
    );
    assert_eq!(
        multi.get("v").unwrap().origin,
        Origin::TypeSwitch {
            subject: "x".into(),
            case_types: vec!["B".into(), "*C".into()]
        }
    );
    let default = scope(
        "package p\nfunc f(x any) {\n\tswitch v := x.(type) {\n\tcase A:\n\tdefault:\n\t\t/*|*/\n\t}\n}\n",
    );
    assert_eq!(
        default.get("v").unwrap().origin,
        Origin::TypeSwitch {
            subject: "x".into(),
            case_types: vec![]
        }
    );
}

#[test]
fn type_switch_binding_scoped_to_clause() {
    has(
        "package p\nfunc f(x any) {\n\tswitch v := x.(type) {\n\tcase A:\n\t\t_ = v\n\t}\n\t/*|*/\n}\n",
        &["x"],
    );
}

#[test]
fn type_switch_without_binding() {
    has(
        "package p\nfunc f(x any) {\n\tswitch x.(type) {\n\tcase A:\n\t\t/*|*/\n\t}\n}\n",
        &["x"],
    );
}

#[test]
fn select_comm_clauses() {
    let src = "package p\nfunc f(ch chan int) {\n\tselect {\n\tcase v, ok := <-ch:\n\t\t/*|*/\n\tcase w := <-ch:\n\t\t_ = w\n\tdefault:\n\t}\n}\n";
    let s = scope(src);
    assert_eq!(s.names(), ["ch", "v", "ok"]);
    assert_eq!(s.get("v").unwrap().origin, expr("<-ch", 0, 2));
    assert_eq!(s.get("ok").unwrap().origin, expr("<-ch", 1, 2));
    has(
        "package p\nfunc f(ch chan int) {\n\tselect {\n\tcase v := <-ch:\n\t\t_ = v\n\tdefault:\n\t\t/*|*/\n\t}\n}\n",
        &["ch"],
    );
}

#[test]
fn closure_sees_outer_and_own_params() {
    let src = "package p\nfunc f(a int) {\n\tb := 1\n\tfn := func(c string) {\n\t\td := 2\n\t\t/*|*/\n\t}\n}\n";
    has(src, &["a", "b", "c", "d"]);
}

#[test]
fn closure_variable_not_visible_in_its_own_literal() {
    has(
        "package p\nfunc f() {\n\tfn := func() {\n\t\t/*|*/\n\t}\n}\n",
        &[],
    );
    let var = scope("package p\nfunc f() {\n\tfn := func() {}\n\t/*|*/\n}\n");
    assert_eq!(var.get("fn").unwrap().origin, expr("func() {}", 0, 1));
}

#[test]
fn nested_func_literals() {
    let src = "package p\nfunc f(a int) {\n\tg := func(b int) func(int) {\n\t\treturn func(c int) {\n\t\t\t/*|*/\n\t\t}\n\t}\n}\n";
    has(src, &["a", "b", "c"]);
}

#[test]
fn closure_params_not_visible_outside() {
    has(
        "package p\nfunc f() {\n\tfn := func(c int) { _ = c }\n\t/*|*/\n}\n",
        &["fn"],
    );
}

#[test]
fn closure_result_names() {
    let s = scope("package p\nfunc f() {\n\tfn := func() (n int) {\n\t\t/*|*/\n\t}\n}\n");
    assert_eq!(s.get("n").unwrap().kind, BindingKind::Result);
}

#[test]
fn generics_type_params_and_params() {
    let s = scope("package p\nfunc Map[T, U any](s []T, f func(T) U) []U {\n\t/*|*/\n}\n");
    assert_eq!(s.names(), ["T", "U", "s", "f"]);
    assert_eq!(s.get("T").unwrap().kind, BindingKind::TypeParam);
    assert_eq!(s.get("U").unwrap().origin, declared("any"));
    assert_eq!(s.get("s").unwrap().origin, declared("[]T"));
    assert_eq!(s.get("f").unwrap().origin, declared("func(T) U"));
}

#[test]
fn generic_constraint_text() {
    let s =
        scope("package p\nfunc Sum[N int | float64, K comparable](xs map[K]N) N {\n\t/*|*/\n}\n");
    assert_eq!(s.get("N").unwrap().origin, declared("int | float64"));
    assert_eq!(s.get("K").unwrap().origin, declared("comparable"));
}

#[test]
fn method_with_receiver() {
    let s = scope("package p\nfunc (r Repo) Find(id int) (*User, error) {\n\t/*|*/\n}\n");
    assert_eq!(s.names(), ["r", "id"]);
    assert_eq!(s.get("r").unwrap().origin, declared("Repo"));
}

#[test]
fn unnamed_receiver() {
    has(
        "package p\nfunc (Repo) Find(id int) {\n\t/*|*/\n}\n",
        &["id"],
    );
}

#[test]
fn labels_visible_in_function() {
    let s =
        scope("package p\nfunc f() {\n\tgoto /*|*/done\n\tx := 1\n\t_ = x\ndone:\n\treturn\n}\n");
    assert_eq!(s.get("done").unwrap().kind, BindingKind::Label);
    assert!(s.get("x").is_none());
}

#[test]
fn labels_not_inherited_by_closures() {
    has(
        "package p\nfunc f() {\nouter:\n\tfor {\n\t\tfn := func() {\n\t\t\t/*|*/\n\t\t}\n\t\tbreak outer\n\t}\n}\n",
        &[],
    );
    has(
        "package p\nfunc f() {\n\tfn := func() {\n\tinner:\n\t\tfor {\n\t\t\tbreak inner\n\t\t\t/*|*/\n\t\t}\n\t}\n}\n",
        &["inner"],
    );
}

#[test]
fn label_and_variable_may_share_a_name() {
    let s = scope("package p\nfunc f() {\n\tx := 1\nx:\n\tfor {\n\t\t/*|*/\n\t}\n}\n");
    assert_eq!(s.get("x").unwrap().kind, BindingKind::ShortVar);
    assert_eq!(s.bindings.len(), 2);
}

#[test]
fn cursor_in_middle_of_identifier() {
    has(
        "package p\nfunc f() {\n\tfoo := 1\n\t_ = fo/*|*/o\n}\n",
        &["foo"],
    );
    has("package p\nfunc f() {\n\tfo/*|*/o := 1\n}\n", &[]);
}

#[test]
fn cursor_at_end_of_declaring_statement() {
    has("package p\nfunc f() {\n\tx := 1/*|*/\n}\n", &[]);
    has("package p\nfunc f() {\n\tx := 1\n/*|*/}\n", &["x"]);
}

#[test]
fn cursor_between_blocks_and_before_body() {
    has("package p\nfunc f(a int) /*|*/{\n\tb := 1\n}\n", &[]);
    has("package p\nfunc f(a int) {/*|*/\n\tb := 1\n}\n", &["a"]);
    has("package p\nfunc f(a int) {\n\tb := 1\n}/*|*/\n", &[]);
}

#[test]
fn syntax_error_incomplete_selector_keeps_bindings() {
    let src = "package p\nfunc f(a int) {\n\tb := g()\n\tc, d := h()\n\tb.\n\t/*|*/\n}\n";
    has(src, &["a", "b", "c", "d"]);
    let inline = "package p\nfunc f(a int) {\n\tb := g()\n\tb./*|*/\n}\n";
    has(inline, &["a", "b"]);
}

#[test]
fn syntax_error_missing_closing_braces() {
    let src = "package p\nfunc f(a int) {\n\tb := g()\n\tif b {\n\t\tc := 1\n\t\tb./*|*/\n";
    has(src, &["a", "b", "c"]);
}

#[test]
fn syntax_error_in_earlier_statement() {
    let src = "package p\nfunc f(a int) {\n\tb := g(\n\tc := 2\n\td := 3\n\t/*|*/\n}\n";
    let got = names(src);
    assert!(got.contains(&"a".to_owned()), "{got:?}");
    assert!(got.contains(&"d".to_owned()), "{got:?}");
}

#[test]
fn syntax_error_in_second_function_does_not_leak() {
    let src = "package p\nfunc f(a int) {\n\tb := 1\n}\nfunc g( {\n\t/*|*/\n}\n";
    assert!(!names(src).contains(&"b".to_owned()));
}

#[test]
fn unterminated_function_at_end_of_file() {
    has("package p\nfunc f(a int) {\n\tb := 1\n\t/*|*/", &["a", "b"]);
}

#[test]
fn scope_end_byte_is_block_end() {
    let (src, at) = fixture("package p\nfunc f() {\n\tif true {\n\t\tx := 1\n\t\t/*|*/\n\t}\n}\n");
    let s = scope_at(&parse(&src), &src, at);
    let x = s.get("x").unwrap();
    assert_eq!(&src[x.scope_end_byte - 1..x.scope_end_byte], "}");
    assert_eq!(x.scope_end_byte, src.find("\t}\n}").unwrap() + 2);
    assert_eq!(&src[x.decl_byte..x.decl_byte + 1], "x");
}

#[test]
fn param_scope_end_is_function_end() {
    let (src, at) = fixture("package p\nfunc f(a int) {\n\t/*|*/\n}\n");
    let s = scope_at(&parse(&src), &src, at);
    assert_eq!(
        s.get("a").unwrap().scope_end_byte,
        src.rfind('}').unwrap() + 1
    );
}

#[test]
fn utf16_columns() {
    let s = scope("package p\nfunc f() {\n\t/* 😀 */ ünï := 1\n\t/*|*/\n}\n");
    let b = s.get("ünï").unwrap();
    // Tab, "/* ", surrogate pair (2 units), " */ " = 1 + 3 + 2 + 4 = 10.
    assert_eq!((b.line, b.column, b.end_column), (2, 10, 13));
}

#[test]
fn block_scopes() {
    has(
        "package p\nfunc f() {\n\t{\n\t\ta := 1\n\t\t/*|*/\n\t}\n}\n",
        &["a"],
    );
    has(
        "package p\nfunc f() {\n\t{\n\t\ta := 1\n\t}\n\t/*|*/\n}\n",
        &[],
    );
}

#[test]
fn bindings_ordered_innermost_last() {
    let s = scope("package p\nfunc f(a int) {\n\tb := 1\n\t{\n\t\tc := 2\n\t\t/*|*/\n\t}\n}\n");
    assert_eq!(s.names(), ["a", "b", "c"]);
}

#[test]
fn cursor_past_end_of_source_is_clamped() {
    let src = "package p\nfunc f(a int) {\n\tb := 1\n";
    let tree = parse(src);
    let s = scope_at(&tree, src, 10_000);
    assert!(s.get("a").is_some());
    assert!(enclosing_func(&tree, src, 10_000).is_some());
}

#[test]
fn empty_and_garbage_input() {
    for src in ["", "}}}", "func", "func (", "func f() {", "{{{{", "\u{0}"] {
        let tree = parse(src);
        for at in 0..=src.len() {
            if src.is_char_boundary(at) {
                let _ = scope_at(&tree, src, at);
                let _ = enclosing_func(&tree, src, at);
            }
        }
    }
}

#[test]
fn every_position_of_a_rich_file_is_safe() {
    let src = "package p\nimport \"fmt\"\ntype T[K any] struct{ a K }\nfunc (t *T[K]) M(xs ...int) (n int, err error) {\n\tfor i, x := range xs {\n\t\tswitch v := any(x).(type) {\n\t\tcase int, string:\n\t\t\tfmt.Println(v)\n\t\tdefault:\n\t\t}\n\t}\n\tselect {\n\tcase r, ok := <-ch:\n\t\t_, _ = r, ok\n\t}\n\tgo func() { y := 1; _ = y }()\nL:\n\tgoto L\n}\n";
    let tree = parse(src);
    for at in 0..=src.len() {
        if src.is_char_boundary(at) {
            let s = scope_at(&tree, src, at);
            let mut seen = std::collections::HashSet::new();
            for b in s.bindings.iter().filter(|b| b.kind != BindingKind::Label) {
                assert!(seen.insert(b.name.clone()), "dup {} at {at}", b.name);
            }
        }
    }
}

#[test]
fn enclosing_func_declaration() {
    let f = func("package p\n\nfunc Add(a, b int) (int, error) {\n\treturn /*|*/a + b, nil\n}\n")
        .unwrap();
    assert_eq!(f.name, "Add");
    assert_eq!(f.receiver_type, None);
    assert!(!f.is_method && !f.is_literal);
    assert_eq!((f.start_line, f.end_line), (2, 4));
    assert_eq!(f.result_types, ["int", "error"]);
}

#[test]
fn enclosing_func_method() {
    let f = func("package p\nfunc (s *Server[T]) Run() error {\n\t/*|*/\n}\n").unwrap();
    assert_eq!(f.name, "Run");
    assert_eq!(f.receiver_type.as_deref(), Some("Server"));
    assert!(f.is_method);
    assert_eq!(f.result_types, ["error"]);
}

#[test]
fn enclosing_func_literal_is_innermost() {
    let src = "package p\nfunc Outer() int {\n\tfn := func() (a, b string) {\n\t\t/*|*/\n\t}\n}\n";
    let f = func(src).unwrap();
    assert!(f.is_literal);
    assert_eq!(f.name, "");
    assert_eq!(f.result_types, ["string", "string"]);
    let after = func("package p\nfunc Outer() int {\n\tfn := func() {}\n\t/*|*/\n}\n").unwrap();
    assert_eq!(after.name, "Outer");
    assert!(!after.is_literal);
}

#[test]
fn enclosing_func_none_outside() {
    assert!(func("package p\nvar x = 1/*|*/\n").is_none());
    assert!(func("package p\nfunc f() {}\n/*|*/\nfunc g() {}\n").is_none());
}

#[test]
fn enclosing_func_with_syntax_error() {
    let f = func("package p\nfunc f() {\n\tx.\n\t/*|*/\n").unwrap();
    assert_eq!(f.name, "f");
}

#[test]
fn label_text_check_accepts_real_labels_and_rejects_lookalikes() {
    use super::decl::may_end_label;
    let ok = |text: &str| {
        let at = text.find(':').unwrap();
        may_end_label(text.as_bytes(), at)
    };
    assert!(ok("{\n\tLoop:\n"));
    assert!(ok("x()\n\tretry:\n"));
    assert!(ok("}\nDone: "));
    assert!(!ok("a := 1"));
    assert!(!ok("switch {\ndefault:\n"));
    assert!(!ok("f(a, Addr: 1)"));
    // Keyed literals after `{` pass: the filter only rules out, never in.
    assert!(ok("Server{Addr: 1}"));
    assert!(!ok("m[k]: v"));
}

#[test]
fn indexed_lookup_matches_plain_lookup_at_every_byte() {
    let rich = "package p\nimport \"fmt\"\ntype T[K any] struct{ a K }\nfunc (t *T[K]) M(xs ...int) (n int, err error) {\n\tfor i, x := range xs {\n\t\tswitch v := any(x).(type) {\n\t\tcase int, string:\n\t\t\tfmt.Println(v)\n\t\tdefault:\n\t\t}\n\t}\n\tselect {\n\tcase r, ok := <-ch:\n\t\t_, _ = r, ok\n\t}\n\tvar a, b = 1, 2\n\tconst c = 3\n\tif q := a; q > 0 {\n\t\tz := q\n\t\t_ = z\n\t}\n\tgo func() { y := 1; _ = y }()\nL:\n\tgoto L\n}\n\nfunc other() {\n\tk := 1\n\t_ = k\n}\n";
    let broken = [
        "package p\nfunc f() {\n\tx := 1\n\tif x > 0 {\n\t\ty := 2\n\n\tz := 3\n",
        "package p\nfunc f(a int) {\n\tfor {\n\t\tb := a\n\t\tswitch {\n\t\tcase b > 1:\n\t\t\tc := b\n",
        "package p\nfunc f() {\n\ta := 1; b := 2\n\tc, d := a, b\n}\nfunc g(",
    ];
    for src in std::iter::once(rich).chain(broken) {
        let tree = parse(src);
        let mut index = ScopeIndex::default();
        // Ascending, as a scan reads identifiers, then backwards, so the
        // index is also right when asked out of order.
        let all: Vec<usize> = (0..=src.len())
            .filter(|b| src.is_char_boundary(*b))
            .collect();
        for at in all.iter().chain(all.iter().rev()) {
            let plain = scope_at(&tree, src, *at);
            let fast = scope_with(&tree, src, *at, Some(&mut index));
            assert_eq!(plain.bindings, fast.bindings, "byte {at} of {src:?}");
        }
    }
}

#[test]
fn parameters_survive_a_method_that_parses_as_loose_tokens() {
    // An unfinished `a.` after the comma of a call makes Tree-sitter give up
    // on the whole declaration and emit its tokens at the top of the file.
    let src = "package repository\nfunc (r *absenRepository) AddAbsen(ctx context.Context, a domain.Absen) error {\n\tresult, err := r.db.ExecContext(ctx, \"INSERT INTO tb_absen (staff_id, shift_id, type, time_absen, limit_absen, date, overtime, remarks) VALUES (?, ?, ?, ?, ?, ?, ?, ?)\",\n\t\ta.\nfunc (r *absenRepository) GetAll(ctx context.Context, date []string, staffID int) ([]domain.Absen, error) {\n\tvar absens []domain.Absen\n";
    let tree = parse(src);
    assert!(
        tree.root_node()
            .children(&mut tree.root_node().walk())
            .any(|n| n.kind() == "func"),
        "the fixture no longer parses as loose tokens: {}",
        tree.root_node().to_sexp()
    );
    let at = src.find("a.\n").unwrap() + 2;
    let names = scope_at(&tree, src, at).names().join(",");
    assert_eq!(names, "r,ctx,a");
}

#[test]
fn loose_header_does_not_leak_into_a_later_function() {
    let (src, at) = fixture(
        "package p\n\nfunc (r *R) Add(ctx context.Context, a domain.Absen) error {\n\tx := r.db.Exec(ctx, \"x\",\n\t\ta.\n\tif x {\n\t}\n}\n\nfunc other(b int) {\n\t/*|*/\n}\n",
    );
    let tree = parse(&src);
    let names = scope_at(&tree, &src, at).names().join(",");
    assert_eq!(names, "b");
}
