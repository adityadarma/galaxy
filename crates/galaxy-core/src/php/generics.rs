//! Types with generic arguments, kept as strings: `Base<A,B>`.
//!
//! `Base` is a class name, [`STATIC_TYPE`] or `%T` (the template parameter
//! `T` of a class or method). An argument that is not known is empty, so
//! `Collection<,User>` is a collection of users with unknown keys.

use std::collections::HashMap;

use super::parse::STATIC_TYPE;

/// `Base<A,B>` as the base and its arguments.
pub fn split(ty: &str) -> (&str, Vec<&str>) {
    match ty.find('<') {
        Some(open) if ty.ends_with('>') => (&ty[..open], split_top(&ty[open + 1..ty.len() - 1])),
        _ => (ty, Vec::new()),
    }
}

/// The type without its generic arguments.
pub fn base(ty: &str) -> &str {
    split(ty).0
}

/// Splits at commas outside `<>`, `()`, `[]` and `{}`.
pub fn split_top(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (index, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(text[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    out.push(text[start..].trim());
    out
}

/// Joins a base and arguments; arguments that are all unknown are dropped.
pub fn join(base: &str, args: &[String]) -> String {
    if args.iter().all(String::is_empty) {
        base.to_owned()
    } else {
        format!("{base}<{}>", args.join(","))
    }
}

/// Replaces template parameters by what `env` binds them to, and
/// `static`/`$this` by `this`. Unbound parameters become empty.
pub fn subst(ty: &str, env: &HashMap<String, String>, this: &str) -> String {
    let (base, args) = split(ty);
    if let Some(name) = base.strip_prefix('%') {
        return env.get(name).cloned().unwrap_or_default();
    }
    if base == STATIC_TYPE {
        return this.to_owned();
    }
    if base.is_empty() {
        return String::new();
    }
    let args: Vec<String> = args.iter().map(|arg| subst(arg, env, this)).collect();
    join(base, &args)
}

/// Binds template names to arguments by position.
pub fn bind(templates: &[String], args: Vec<String>) -> HashMap<String, String> {
    templates
        .iter()
        .enumerate()
        .map(|(index, name)| (name.clone(), args.get(index).cloned().unwrap_or_default()))
        .collect()
}

/// The type at the start of `text`: up to the first space outside brackets.
pub fn take_type(text: &str) -> &str {
    let mut depth = 0i32;
    for (index, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            c if c.is_whitespace() && depth <= 0 => return &text[..index],
            _ => {}
        }
    }
    text
}

/// Splits a union or intersection at `|` and `&` outside brackets.
pub fn split_union(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (index, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            '|' | '&' if depth == 0 => {
                out.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    out.push(&text[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_nested_arguments() {
        let (base, args) = split("A\\Coll<,B\\Item<x,y>>");
        assert_eq!(base, "A\\Coll");
        assert_eq!(args, ["", "B\\Item<x,y>"]);
        assert_eq!(split("Plain"), ("Plain", vec![]));
    }

    #[test]
    fn substitutes_templates_and_this() {
        let env = bind(
            &["T".to_owned(), "U".to_owned()],
            vec!["App\\User".to_owned()],
        );
        assert_eq!(subst("%T", &env, ""), "App\\User");
        assert_eq!(subst("%U", &env, ""), "");
        assert_eq!(
            subst("Rel<%T,static>", &env, "App\\Order"),
            "Rel<App\\User,App\\Order>"
        );
        assert_eq!(subst("Rel<%U>", &env, ""), "Rel");
    }

    #[test]
    fn takes_types_with_spaces_inside_brackets() {
        assert_eq!(take_type("A<B, C> some words"), "A<B, C>");
        assert_eq!(split_union("A<B|C>|null"), ["A<B|C>", "null"]);
    }
}
