//! One-line declaration text for hover and signature help, built from the
//! parsed declarations.

use std::collections::HashMap;

use crate::go::parse::{FuncDecl, Param, TypeDecl, TypeKind, TypeParam, ValueDecl};
use crate::go::types::Ty;

/// Type parameter name to the type text written in its place.
pub(super) type Subst = HashMap<String, String>;

/// `text` with the identifiers in `map` replaced, except after a dot.
pub(super) fn subst_text(text: &str, map: &Subst) -> String {
    if map.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let Some(c) = text[i..].chars().next() else {
            break;
        };
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while let Some(ch) = text[i..].chars().next() {
                if ch.is_alphanumeric() || ch == '_' {
                    i += ch.len_utf8();
                } else {
                    break;
                }
            }
            let word = &text[start..i];
            match map.get(word) {
                Some(rep) if !text[..start].ends_with('.') => out.push_str(rep),
                _ => out.push_str(word),
            }
        } else {
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

fn param_text(p: &Param, map: &Subst) -> String {
    let ty = subst_text(&p.ty, map);
    let ty = if p.variadic && !ty.starts_with("...") {
        format!("...{ty}")
    } else {
        ty
    };
    if p.name.is_empty() {
        ty
    } else {
        format!("{} {ty}", p.name)
    }
}

pub(super) fn params_of(decl: &FuncDecl, map: &Subst) -> Vec<String> {
    decl.params.iter().map(|p| param_text(p, map)).collect()
}

pub(super) fn results_of(decl: &FuncDecl, map: &Subst) -> Vec<String> {
    decl.results.iter().map(|p| param_text(p, map)).collect()
}

/// ` T`, ` (a, b)` or nothing.
pub(super) fn results_suffix(results: &[String], named: bool) -> String {
    match results {
        [] => String::new(),
        [one] if !named => format!(" {one}"),
        many => format!(" ({})", many.join(", ")),
    }
}

fn type_params_text(list: &[&TypeParam]) -> String {
    if list.is_empty() {
        return String::new();
    }
    let mut items: Vec<String> = Vec::new();
    for (i, t) in list.iter().enumerate() {
        let same_next = list
            .get(i + 1)
            .is_some_and(|n| n.constraint == t.constraint);
        if t.constraint.is_empty() || same_next {
            items.push(t.name.clone());
        } else {
            items.push(format!("{} {}", t.name, t.constraint));
        }
    }
    format!("[{}]", items.join(", "))
}

/// `func (c *Client) Do(req *Request) (*Response, error)`. Type parameters
/// found in `map` are replaced and dropped from the list.
pub(super) fn func_sig(decl: &FuncDecl, map: &Subst) -> String {
    let unbound: Vec<&TypeParam> = decl
        .type_params
        .iter()
        .filter(|t| !map.contains_key(&t.name))
        .collect();
    let mut out = String::from("func ");
    if let Some(r) = &decl.receiver {
        if r.name.is_empty() {
            out.push_str(&format!("({}) ", r.ty));
        } else {
            out.push_str(&format!("({} {}) ", r.name, r.ty));
        }
    }
    out.push_str(&decl.name);
    out.push_str(&type_params_text(&unbound));
    out.push('(');
    out.push_str(&params_of(decl, map).join(", "));
    out.push(')');
    let named = decl.results.iter().any(|r| !r.name.is_empty());
    out.push_str(&results_suffix(&results_of(decl, map), named));
    out
}

/// `type User struct`, `type Strs []string`, `type A = B`.
pub(super) fn type_sig(decl: &TypeDecl) -> String {
    let all: Vec<&TypeParam> = decl.type_params.iter().collect();
    let head = format!("type {}{}", decl.name, type_params_text(&all));
    match decl.kind {
        TypeKind::Struct => format!("{head} struct"),
        TypeKind::Interface => format!("{head} interface"),
        TypeKind::Alias => format!("{head} = {}", decl.underlying),
        TypeKind::Named => format!("{head} {}", decl.underlying),
    }
}

fn short_value(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 60 {
        let cut: String = flat.chars().take(57).collect();
        format!("{cut}...")
    } else {
        flat
    }
}

/// `const StatusOK = 200`, `var ErrX error`.
pub(super) fn value_sig(decl: &ValueDecl, ty: &Ty) -> String {
    let declared = decl.ty.as_ref().or(decl.group_type.as_ref());
    if decl.is_const {
        let mut out = format!("const {}", decl.name);
        if let Some(t) = declared {
            out.push(' ');
            out.push_str(t);
        }
        if let Some(v) = &decl.value {
            out.push_str(" = ");
            out.push_str(&short_value(v));
        }
        return out;
    }
    if let Some(t) = declared {
        return format!("var {} {t}", decl.name);
    }
    if !ty.is_unknown() {
        return format!("var {} {}", decl.name, ty.display());
    }
    match &decl.value {
        Some(v) => format!("var {} = {}", decl.name, short_value(v)),
        None => format!("var {}", decl.name),
    }
}

/// Hard-coded declarations of the predeclared functions that help is shown for.
pub(super) fn builtin_label(name: &str) -> Option<&'static str> {
    Some(match name {
        "append" => "func append(slice []T, elems ...T) []T",
        "make" => "func make(t Type, size ...IntegerType) Type",
        "new" => "func new(Type) *Type",
        "len" => "func len(v Type) int",
        "cap" => "func cap(v Type) int",
        "copy" => "func copy(dst, src []T) int",
        "delete" => "func delete(m map[K]V, key K)",
        "panic" => "func panic(v any)",
        _ => return None,
    })
}

/// Splits `text` at commas outside brackets.
pub(super) fn split_top(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in text.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    let mut items: Vec<String> = out.into_iter().map(|s| s.trim().to_owned()).collect();
    // `a, b int` splits into `a` and `b int`: share the type.
    let mut shared: Option<String> = None;
    for item in items.iter_mut().rev() {
        match top_space(item) {
            Some(at) => shared = Some(item[at + 1..].trim().to_owned()),
            None => {
                if let Some(ty) = &shared
                    && !item.starts_with("...")
                {
                    *item = format!("{item} {ty}");
                }
            }
        }
    }
    items
}

/// Index of the first space outside brackets.
fn top_space(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in text.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ' ' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Params and results of a signature text such as
/// `(a int, b ...string) (string, error)`.
pub(super) fn split_signature(sig: &str) -> (Vec<String>, Vec<String>) {
    let sig = sig.trim();
    let Some(rest) = sig.strip_prefix('(') else {
        return (Vec::new(), Vec::new());
    };
    let mut depth = 1i32;
    let mut close = None;
    for (i, c) in rest.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else {
        return (split_top(rest), Vec::new());
    };
    let params = split_top(&rest[..close]);
    let tail = rest[close + 1..].trim();
    let results = if tail.is_empty() {
        Vec::new()
    } else if let Some(inner) = tail.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
        split_top(inner)
    } else {
        vec![tail.to_owned()]
    };
    (params, results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_whole_words_only() {
        let mut map = Subst::new();
        map.insert("T".into(), "int".into());
        assert_eq!(subst_text("[]T", &map), "[]int");
        assert_eq!(subst_text("func(T) Tx", &map), "func(int) Tx");
        assert_eq!(subst_text("pkg.T", &map), "pkg.T");
    }

    #[test]
    fn splits_signatures() {
        let (p, r) = split_signature("(a, b int, rest ...string) (string, error)");
        assert_eq!(p, ["a int", "b int", "rest ...string"]);
        assert_eq!(r, ["string", "error"]);
        let (p, r) = split_signature("(int, func(a, b int)) error");
        assert_eq!(p, ["int", "func(a, b int)"]);
        assert_eq!(r, ["error"]);
        let (p, r) = split_signature("()");
        assert!(p.is_empty() && r.is_empty());
    }
}
