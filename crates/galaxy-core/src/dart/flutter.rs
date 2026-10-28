//! Flutter-specific static semantic navigation layered on the Dart resolver.

use std::path::Path;

use crate::lang::ItemKind;

use super::{Declaration, first_ident, utf8_at, utf16};

pub(super) fn semantic_target(
    path: &Path,
    text: &str,
    line: u32,
    name: &str,
) -> Option<Declaration> {
    if name == "context" {
        let before = text
            .lines()
            .take(line as usize + 1)
            .collect::<Vec<_>>()
            .join("\n");
        let parameter = before.rfind("BuildContext context")?;
        return declaration_at(path, text, "context", parameter + "BuildContext ".len());
    }
    if name == "widget" && text[..line_start(text, line)?].contains("extends State<") {
        let state = text[..line_start(text, line)?].rfind("extends State<")?;
        let start = state + "extends State<".len();
        let end = text[start..].find('>')? + start;
        return declaration_at(path, text, text[start..end].trim(), start);
    }
    provider_target(path, text, line, name)
}

pub(super) fn route_target(path: &Path, text: &str, line: u32, column: u32) -> Option<Declaration> {
    let source = text.lines().nth(line as usize)?;
    let route = quoted_at(source, utf8_at(source, column))?;
    let routes = text.find("routes:")?;
    let route_entry = text[routes..].find(&format!("'{route}'"))? + routes;
    let constructor = text[route_entry..].split_once("=>")?.1.trim_start();
    declaration_at(path, text, &first_ident(constructor)?, 0)
}

fn provider_target(path: &Path, text: &str, line: u32, name: &str) -> Option<Declaration> {
    let source = text.lines().nth(line as usize)?;
    let type_start = source.find(name)?;
    if ![
        "context.read<",
        "context.watch<",
        "Provider.of<",
        "BlocProvider.of<",
        "Get.find<",
    ]
    .iter()
    .any(|call| source[..type_start].contains(call))
    {
        return None;
    }
    let provider = [
        "Provider<",
        "BlocProvider<",
        "RepositoryProvider<",
        "ChangeNotifierProvider<",
    ]
    .iter()
    .filter_map(|kind| text.rfind(&format!("{kind}{name}>")))
    .max()?;
    let value = &text[provider..];
    value.find("create:").or_else(|| value.find("value:"))?;
    declaration_at(path, text, name, 0)
}

fn declaration_at(path: &Path, text: &str, name: &str, from: usize) -> Option<Declaration> {
    let offset = text[from..].find(name)? + from;
    let line = text[..offset].lines().count().saturating_sub(1) as u32;
    let start = text[..offset].rfind('\n').map_or(0, |at| at + 1);
    Some(Declaration {
        name: name.to_owned(),
        path: path.to_path_buf(),
        line,
        column: utf16(&text[start..offset]),
        kind: ItemKind::Class,
        signature: name.to_owned(),
        owner: None,
    })
}

fn quoted_at(source: &str, byte: usize) -> Option<String> {
    let left = &source[..byte];
    let (start, quote) = left
        .char_indices()
        .rev()
        .find(|(_, c)| matches!(c, '\'' | '"'))?;
    let end = source[byte..].find(quote)? + byte;
    Some(source[start + quote.len_utf8()..end].to_owned())
}

fn line_start(text: &str, line: u32) -> Option<usize> {
    let mut start = 0;
    for _ in 0..line {
        start += text[start..].find('\n')? + 1;
    }
    Some(start)
}
