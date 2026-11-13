//! Small built-in snippet catalog and VS Code-style placeholder expansion.
//!
//! The catalog intentionally stays compact. It supplies useful fallbacks next
//! to the built-in language engines, which provide richer items.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub prefix: &'static str,
    pub description: &'static str,
    pub body: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    pub index: u32,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    pub text: String,
    pub stops: Vec<Stop>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExpansionContext {
    pub filename: String,
    pub selected_text: String,
    pub current_year: String,
}

impl ExpansionContext {
    fn variable(&self, name: &str) -> Option<String> {
        match name {
            "TM_FILENAME" => Some(self.filename.clone()),
            "TM_FILENAME_BASE" => Some(
                self.filename
                    .rsplit_once('.')
                    .map(|(base, _)| base.to_owned())
                    .unwrap_or_else(|| self.filename.clone()),
            ),
            "TM_SELECTED_TEXT" | "SELECTION" => Some(self.selected_text.clone()),
            "CURRENT_YEAR" => Some(self.current_year.clone()),
            _ => None,
        }
    }
}

pub fn language_for_path(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".blade.php") {
        return Some("blade");
    }
    let name = Path::new(&lower).file_name()?.to_str()?;
    if matches!(name, "dockerfile" | "containerfile") {
        return Some("shell");
    }
    match Path::new(name).extension()?.to_str()? {
        "rs" => Some("rust"),
        "go" => Some("go"),
        "py" => Some("python"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "jsx" => Some("jsx"),
        "ts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "php" => Some("php"),
        "dart" => Some("dart"),
        "java" => Some("java"),
        "kt" | "kts" => Some("kotlin"),
        "swift" => Some("swift"),
        "c" | "h" => Some("c"),
        "cc" | "cpp" | "cxx" | "hpp" => Some("cpp"),
        "cs" => Some("csharp"),
        "rb" => Some("ruby"),
        "html" | "htm" => Some("html"),
        "css" | "scss" => Some("css"),
        "sh" | "bash" | "zsh" | "fish" => Some("shell"),
        "sql" => Some("sql"),
        _ => None,
    }
}

fn base(language: &str) -> &'static [Snippet] {
    match language {
        "rust" => &[
            Snippet {
                prefix: "fn",
                description: "Function",
                body: "fn ${1:name}(${2:args}) {\n\t$0\n}",
            },
            Snippet {
                prefix: "test",
                description: "Test module",
                body: "#[test]\nfn ${1:name}() {\n\t$0\n}",
            },
            Snippet {
                prefix: "struct",
                description: "Struct",
                body: "struct ${1:Name} {\n\t$0\n}",
            },
            Snippet {
                prefix: "println",
                description: "Print line",
                body: "println!(\"${1}\");$0",
            },
            Snippet {
                prefix: "print",
                description: "Print",
                body: "print!(\"${1}\");$0",
            },
            Snippet {
                prefix: "eprintln",
                description: "Print error line",
                body: "eprintln!(\"${1}\");$0",
            },
            Snippet {
                prefix: "eprint",
                description: "Print error",
                body: "eprint!(\"${1}\");$0",
            },
            Snippet {
                prefix: "dbg",
                description: "Debug value",
                body: "dbg!(${1:value});$0",
            },
        ],
        "go" => &[
            Snippet {
                prefix: "func",
                description: "Function",
                body: "func ${1:name}(${2:args}) ${3:error} {\n\t$0\n}",
            },
            Snippet {
                prefix: "iferr",
                description: "Error check",
                body: "if err != nil {\n\treturn $0\n}",
            },
            Snippet {
                prefix: "struct",
                description: "Struct",
                body: "type ${1:Name} struct {\n\t$0\n}",
            },
        ],
        "python" => &[
            Snippet {
                prefix: "def",
                description: "Function",
                body: "def ${1:name}(${2:args}):\n\t$0",
            },
            Snippet {
                prefix: "class",
                description: "Class",
                body: "class ${1:Name}:\n\t$0",
            },
            Snippet {
                prefix: "main",
                description: "Main guard",
                body: "if __name__ == \"__main__\":\n\t$0",
            },
        ],
        "javascript" | "jsx" | "typescript" | "tsx" => &[
            Snippet {
                prefix: "log",
                description: "Console log",
                body: "console.log(${1:value});$0",
            },
            Snippet {
                prefix: "func",
                description: "Function",
                body: "function ${1:name}(${2:args}) {\n\t$0\n}",
            },
            Snippet {
                prefix: "arrow",
                description: "Arrow function",
                body: "const ${1:name} = (${2:args}) => {\n\t$0\n};",
            },
            Snippet {
                prefix: "try",
                description: "Try/catch",
                body: "try {\n\t$1\n} catch (${2:error}) {\n\t$0\n}",
            },
        ],
        "php" => &[
            Snippet {
                prefix: "class",
                description: "PHP class",
                body: "class ${1:Name} {\n\t$0\n}",
            },
            Snippet {
                prefix: "function",
                description: "PHP function",
                body: "function ${1:name}(${2:args}): ${3:void}\n{\n\t$0\n}",
            },
            Snippet {
                prefix: "route",
                description: "Laravel route",
                body: "Route::get('${1:/path}', [${2:Controller}::class, '${3:index}']);$0",
            },
            Snippet {
                prefix: "model",
                description: "Laravel model",
                body: "class ${1:Model} extends Model\n{\n\tprotected $fillable = [${2}];\n\n\t$0\n}",
            },
        ],
        "blade" => &[
            Snippet {
                prefix: "bif",
                description: "Blade if",
                body: "@if (${1:condition})\n\t$0\n@endif",
            },
            Snippet {
                prefix: "bforeach",
                description: "Blade foreach",
                body: "@foreach (${1:items} as ${2:item})\n\t$0\n@endforeach",
            },
            Snippet {
                prefix: "bsection",
                description: "Blade section",
                body: "@section('${1:name}')\n\t$0\n@endsection",
            },
        ],
        "dart" => &[
            Snippet {
                prefix: "stless",
                description: "Flutter StatelessWidget",
                body: "class ${1:Name} extends StatelessWidget {\n\tconst ${1:Name}({super.key});\n\n\t@override\n\tWidget build(BuildContext context) => $0;\n}",
            },
            Snippet {
                prefix: "stful",
                description: "Flutter StatefulWidget",
                body: "class ${1:Name} extends StatefulWidget {\n\tconst ${1:Name}({super.key});\n\n\t@override\n\tState<${1:Name}> createState() => _${1:Name}State();\n}\n\nclass _${1:Name}State extends State<${1:Name}> {\n\t@override\n\tWidget build(BuildContext context) => $0;\n}",
            },
            Snippet {
                prefix: "main",
                description: "Dart main",
                body: "void main() {\n\t$0\n}",
            },
        ],
        "java" => &[
            Snippet {
                prefix: "psvm",
                description: "Main method",
                body: "public static void main(String[] args) {\n\t$0\n}",
            },
            Snippet {
                prefix: "class",
                description: "Class",
                body: "public class ${1:Name} {\n\t$0\n}",
            },
            Snippet {
                prefix: "sout",
                description: "Print line",
                body: "System.out.println(${1:value});$0",
            },
        ],
        "kotlin" => &[
            Snippet {
                prefix: "fun",
                description: "Function",
                body: "fun ${1:name}(${2:args}): ${3:Unit} {\n\t$0\n}",
            },
            Snippet {
                prefix: "data",
                description: "Data class",
                body: "data class ${1:Name}(\n\t${2:value}: ${3:String}\n)$0",
            },
            Snippet {
                prefix: "main",
                description: "Main function",
                body: "fun main() {\n\t$0\n}",
            },
        ],
        "swift" => &[
            Snippet {
                prefix: "func",
                description: "Function",
                body: "func ${1:name}(${2:args}) {\n\t$0\n}",
            },
            Snippet {
                prefix: "struct",
                description: "Struct",
                body: "struct ${1:Name} {\n\t$0\n}",
            },
            Snippet {
                prefix: "guard",
                description: "Guard",
                body: "guard let ${1:value} else { return }\n$0",
            },
        ],
        "c" | "cpp" => &[
            Snippet {
                prefix: "main",
                description: "Main function",
                body: "int main() {\n\t$0\n}",
            },
            Snippet {
                prefix: "for",
                description: "For loop",
                body: "for (int ${1:i} = 0; ${1:i} < ${2:n}; ++${1:i}) {\n\t$0\n}",
            },
            Snippet {
                prefix: "include",
                description: "Include",
                body: "#include <${1:stdio.h}>$0",
            },
        ],
        "csharp" => &[
            Snippet {
                prefix: "class",
                description: "Class",
                body: "public class ${1:Name}\n{\n\t$0\n}",
            },
            Snippet {
                prefix: "cw",
                description: "Console write",
                body: "Console.WriteLine(${1:value});$0",
            },
            Snippet {
                prefix: "prop",
                description: "Property",
                body: "public ${1:string} ${2:Name} { get; set; }$0",
            },
        ],
        "ruby" => &[
            Snippet {
                prefix: "def",
                description: "Method",
                body: "def ${1:name}(${2:args})\n\t$0\nend",
            },
            Snippet {
                prefix: "class",
                description: "Class",
                body: "class ${1:Name}\n\t$0\nend",
            },
            Snippet {
                prefix: "each",
                description: "Each block",
                body: "${1:items}.each do |${2:item}|\n\t$0\nend",
            },
        ],
        "html" => &[
            Snippet {
                prefix: "html5",
                description: "HTML5 document",
                body: "<!doctype html>\n<html lang=\"${1:en}\">\n<head>\n\t<meta charset=\"UTF-8\">\n\t<title>${2:Document}</title>\n</head>\n<body>\n\t$0\n</body>\n</html>",
            },
            Snippet {
                prefix: "div",
                description: "Div",
                body: "<div class=\"${1:class}\">$0</div>",
            },
            Snippet {
                prefix: "link",
                description: "Link",
                body: "<a href=\"${1:#}\">${2:text}</a>$0",
            },
        ],
        "css" => &[
            Snippet {
                prefix: "flex",
                description: "Flex center",
                body: "display: flex;\nalign-items: center;\njustify-content: center;$0",
            },
            Snippet {
                prefix: "media",
                description: "Media query",
                body: "@media (min-width: ${1:768px}) {\n\t$0\n}",
            },
            Snippet {
                prefix: "class",
                description: "CSS class",
                body: ".${1:name} {\n\t$0\n}",
            },
        ],
        "shell" => &[
            Snippet {
                prefix: "shebang",
                description: "Bash shebang",
                body: "#!/usr/bin/env bash\nset -euo pipefail\n$0",
            },
            Snippet {
                prefix: "if",
                description: "Shell if",
                body: "if [[ ${1:condition} ]]; then\n\t$0\nfi",
            },
            Snippet {
                prefix: "for",
                description: "Shell loop",
                body: "for ${1:item} in \"${2:\\$@}\"; do\n\t$0\ndone",
            },
        ],
        "sql" => &[
            Snippet {
                prefix: "select",
                description: "Select query",
                body: "SELECT ${1:*}\nFROM ${2:table}\nWHERE ${3:condition};$0",
            },
            Snippet {
                prefix: "insert",
                description: "Insert query",
                body: "INSERT INTO ${1:table} (${2:columns})\nVALUES (${3:values});$0",
            },
            Snippet {
                prefix: "join",
                description: "Join query",
                body: "JOIN ${1:table} ON ${2:condition}$0",
            },
        ],
        _ => &[],
    }
}

pub fn snippets_for_path(path: &str) -> Vec<Snippet> {
    let Some(language) = language_for_path(path) else {
        return Vec::new();
    };
    let mut result = base(language).to_vec();
    if language == "typescript" || language == "tsx" {
        result.extend_from_slice(base("javascript"));
    }
    if language == "jsx" || language == "tsx" {
        result.extend_from_slice(&[
            Snippet {
                prefix: "component",
                description: "React component",
                body: "function ${1:Component}() {\n\treturn <${2:div}>$0</${2:div}>;\n}",
            },
            Snippet {
                prefix: "useState",
                description: "React state hook",
                body: "const [${1:value}, set${2:Value}] = useState(${3:initial});$0",
            },
        ]);
    }
    result
}

pub fn matching(path: &str, query: &str) -> Vec<Snippet> {
    if query.is_empty() {
        return Vec::new();
    }
    let query = query.to_ascii_lowercase();
    snippets_for_path(path)
        .into_iter()
        .filter(|s| s.prefix.to_ascii_lowercase().starts_with(&query))
        .collect()
}

pub fn expand(body: &str, base_indent: &str, indent_unit: &str) -> Expansion {
    expand_with_context(body, base_indent, indent_unit, &ExpansionContext::default())
}

pub fn expand_with_context(
    body: &str,
    base_indent: &str,
    indent_unit: &str,
    context: &ExpansionContext,
) -> Expansion {
    let mut parser = Parser {
        chars: body.chars().collect(),
        pos: 0,
        out: String::new(),
        stops: BTreeMap::new(),
        defaults: HashMap::new(),
        base_indent: base_indent.to_owned(),
        indent_unit: indent_unit.to_owned(),
        at_line_start: false,
        context: context.clone(),
    };
    parser.parse_until(None);
    let mut stops = parser
        .stops
        .into_iter()
        .flat_map(|(index, ranges)| {
            ranges
                .into_iter()
                .map(move |(start, end)| Stop { index, start, end })
        })
        .collect::<Vec<_>>();
    stops.sort_by_key(|s| (s.index == 0, s.index, s.start));
    if !stops.iter().any(|s| s.index == 0) {
        let end = parser.out.chars().count();
        stops.push(Stop {
            index: 0,
            start: end,
            end,
        });
    }
    Expansion {
        text: parser.out,
        stops,
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    out: String,
    stops: BTreeMap<u32, Vec<(usize, usize)>>,
    defaults: HashMap<u32, String>,
    base_indent: String,
    indent_unit: String,
    at_line_start: bool,
    context: ExpansionContext,
}
impl Parser {
    fn len(&self) -> usize {
        self.out.chars().count()
    }
    fn push(&mut self, c: char) {
        self.out.push(c);
        self.at_line_start = c == '\n';
        if self.at_line_start {
            self.out.push_str(&self.base_indent);
            self.at_line_start = false;
        }
    }
    fn push_text(&mut self, text: &str) {
        for c in text.chars() {
            if c == '\t' {
                self.out.push_str(&self.indent_unit);
            } else {
                self.push(c);
            }
        }
    }
    fn parse_until(&mut self, end: Option<char>) {
        while self.pos < self.chars.len() {
            let c = self.chars[self.pos];
            if Some(c) == end {
                self.pos += 1;
                return;
            }
            if c == '\\' && self.pos + 1 < self.chars.len() {
                self.pos += 1;
                self.push(self.chars[self.pos]);
                self.pos += 1;
            } else if c == '$' {
                if !self.dollar() {
                    self.push(c);
                    self.pos += 1;
                }
            } else {
                self.push(c);
                self.pos += 1;
            }
        }
    }
    fn dollar(&mut self) -> bool {
        let start = self.pos;
        self.pos += 1;
        if self.pos >= self.chars.len() {
            self.pos = start;
            return false;
        }
        if self.chars[self.pos].is_ascii_digit() {
            let index = self.number();
            let a = self.len();
            if let Some(text) = self.defaults.get(&index).cloned() {
                self.push_text(&text);
            }
            let b = self.len();
            self.stops.entry(index).or_default().push((a, b));
            return true;
        }
        if self.chars[self.pos].is_ascii_alphabetic() || self.chars[self.pos] == '_' {
            let name = self.identifier();
            self.push_text(&self.context.variable(&name).unwrap_or_default());
            return true;
        }
        if self.chars[self.pos] != '{' {
            self.pos = start;
            return false;
        }
        self.pos += 1;
        if self.pos < self.chars.len()
            && (self.chars[self.pos].is_ascii_alphabetic() || self.chars[self.pos] == '_')
        {
            let name = self.identifier();
            let value = if self.pos < self.chars.len() && self.chars[self.pos] == ':' {
                self.pos += 1;
                let before = self.len();
                self.parse_until(Some('}'));
                let fallback: String = self.out.chars().skip(before).collect();
                self.truncate_chars(before);
                self.context
                    .variable(&name)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(fallback)
            } else {
                if self.pos < self.chars.len() && self.chars[self.pos] == '}' {
                    self.pos += 1;
                }
                self.context.variable(&name).unwrap_or_default()
            };
            self.push_text(&value);
            return true;
        }
        let index = self.number();
        if self.pos >= self.chars.len() {
            self.pos = start;
            return false;
        }
        let a = self.len();
        match self.chars[self.pos] {
            '}' => {
                self.pos += 1;
                if let Some(text) = self.defaults.get(&index).cloned() {
                    self.push_text(&text);
                }
            }
            ':' => {
                self.pos += 1;
                let before = self.len();
                self.parse_until(Some('}'));
                let text: String = self.out.chars().skip(before).collect();
                self.defaults.entry(index).or_insert(text);
            }
            '|' => {
                self.pos += 1;
                let start_choice = self.pos;
                while self.pos < self.chars.len() && self.chars[self.pos] != '|' {
                    self.pos += 1;
                }
                let choice: String = self.chars[start_choice..self.pos].iter().collect();
                let selected = choice.split(',').next().unwrap_or_default();
                self.push_text(selected);
                self.defaults
                    .entry(index)
                    .or_insert_with(|| selected.to_owned());
                if self.pos < self.chars.len() {
                    self.pos += 1;
                }
                if self.pos < self.chars.len() && self.chars[self.pos] == '}' {
                    self.pos += 1;
                }
            }
            _ => {
                self.pos = start;
                return false;
            }
        }
        let b = self.len();
        self.stops.entry(index).or_default().push((a, b));
        true
    }
    fn number(&mut self) -> u32 {
        let start = self.pos;
        while self.pos < self.chars.len() && self.chars[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        self.chars[start..self.pos]
            .iter()
            .collect::<String>()
            .parse()
            .unwrap_or(0)
    }

    fn identifier(&mut self) -> String {
        let start = self.pos;
        while self.pos < self.chars.len()
            && (self.chars[self.pos].is_ascii_alphanumeric() || self.chars[self.pos] == '_')
        {
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect()
    }

    fn truncate_chars(&mut self, count: usize) {
        let byte = self
            .out
            .char_indices()
            .nth(count)
            .map_or(self.out.len(), |(byte, _)| byte);
        self.out.truncate(byte);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expands_placeholders_and_final_stop() {
        let e = expand("fn ${1:name}() {\n\t$0\n}", "    ", "    ");
        assert!(e.text.contains("fn name"));
        assert_eq!(e.stops[0].index, 1);
        assert_eq!(e.stops.last().unwrap().index, 0);
    }
    #[test]
    fn maps_common_paths() {
        assert_eq!(language_for_path("x.blade.php"), Some("blade"));
        assert_eq!(language_for_path("main.tsx"), Some("tsx"));
        assert!(matching("main.py", "de").iter().any(|s| s.prefix == "def"));
    }

    #[test]
    fn rust_println_snippet_matches_typed_prefix() {
        let snippets = matching("main.rs", "printl");
        let println = snippets
            .iter()
            .find(|snippet| snippet.prefix == "println")
            .unwrap();
        assert_eq!(println.body, "println!(\"${1}\");$0");
    }

    #[test]
    fn expands_variables_and_defaults() {
        let context = ExpansionContext {
            filename: "User.blade.php".into(),
            selected_text: "picked".into(),
            current_year: "2026".into(),
        };
        let e = expand_with_context(
            "${TM_FILENAME_BASE} ${TM_SELECTED_TEXT} ${CURRENT_YEAR} ${UNKNOWN:fallback}",
            "",
            "    ",
            &context,
        );
        assert_eq!(e.text, "User.blade picked 2026 fallback");
    }

    #[test]
    fn keeps_escaped_dollar_and_nested_placeholder() {
        let e = expand(r#"\$HOME ${1:outer ${2:inner}}"#, "", "    ");
        assert_eq!(e.text, "$HOME outer inner");
        assert!(e.stops.iter().any(|stop| stop.index == 2));
    }

    #[test]
    fn every_builtin_snippet_expands_to_valid_ranges() {
        let paths = [
            "main.rs",
            "main.go",
            "main.py",
            "main.tsx",
            "main.php",
            "view.blade.php",
            "main.dart",
            "Main.java",
            "Main.kt",
            "main.swift",
            "main.cpp",
            "main.cs",
            "main.rb",
            "index.html",
            "main.css",
            "run.sh",
            "query.sql",
        ];
        for path in paths {
            for snippet in snippets_for_path(path) {
                assert!(!snippet.prefix.is_empty(), "empty prefix for {path}");
                let expansion = expand(snippet.body, "    ", "    ");
                let length = expansion.text.chars().count();
                for stop in expansion.stops {
                    assert!(
                        stop.start <= stop.end && stop.end <= length,
                        "invalid stop in {}",
                        snippet.prefix
                    );
                }
            }
        }
    }

    #[test]
    fn expands_choice_and_mirrored_placeholder() {
        let expansion = expand("${1|red,green|} ${1}", "", "    ");
        assert_eq!(expansion.text, "red red");
        assert_eq!(
            expansion
                .stops
                .iter()
                .filter(|stop| stop.index == 1)
                .count(),
            2
        );
    }
}
