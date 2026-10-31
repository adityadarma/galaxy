//! Inline parameter names before call arguments, as VS Code's
//! inline-parameters extension shows them: `greet(name: "Ada")`.
//!
//! Each language engine finds its call sites and the callee's parameters;
//! this module pairs them up and decides which arguments get a label.

/// A label drawn before the argument at `line`/`column` (UTF-16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterHint {
    pub line: u32,
    pub column: u32,
    pub label: String,
}

/// One declared parameter. An empty `name` (a destructured pattern) keeps
/// its position but gets no label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub name: String,
    /// `...args` / `...$args`: it takes this and every later argument.
    pub rest: bool,
}

/// One argument at a call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argument {
    pub line: u32,
    pub column: u32,
    /// The name the argument passes (`name`, `$name`, `user.name`), used to
    /// hide labels that would repeat it.
    pub name: Option<String>,
    /// `...xs`: later positions are unknown.
    pub spread: bool,
    /// PHP named argument (`name: 1`): already labelled, and positions after
    /// it no longer line up.
    pub named: bool,
}

/// Calls looked at per request, so a huge viewport stays cheap.
pub const MAX_CALLS: usize = 400;

/// Labels for `arguments` passed to a callee declaring `parameters`.
pub fn hints(parameters: &[Parameter], arguments: &[Argument]) -> Vec<ParameterHint> {
    let mut out = Vec::new();
    for (index, argument) in arguments.iter().enumerate() {
        if argument.spread || argument.named {
            break;
        }
        let Some(parameter) = parameters.get(index) else {
            break;
        };
        if !parameter.name.is_empty() && !repeats(argument.name.as_deref(), &parameter.name) {
            let dots = if parameter.rest { "..." } else { "" };
            out.push(ParameterHint {
                line: argument.line,
                column: argument.column,
                label: format!("{dots}{}:", parameter.name),
            });
        }
        if parameter.rest {
            break;
        }
    }
    out
}

/// `greet(name)` needs no `name:` label.
fn repeats(argument: Option<&str>, parameter: &str) -> bool {
    let normalize = |name: &str| name.trim_start_matches('$').to_ascii_lowercase();
    argument.is_some_and(|argument| normalize(argument) == normalize(parameter))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argument(column: u32, name: Option<&str>) -> Argument {
        Argument {
            line: 0,
            column,
            name: name.map(str::to_owned),
            spread: false,
            named: false,
        }
    }

    fn parameter(name: &str, rest: bool) -> Parameter {
        Parameter {
            name: name.to_owned(),
            rest,
        }
    }

    #[test]
    fn labels_positional_arguments_and_skips_repeated_names() {
        let parameters = [parameter("name", false), parameter("age", false)];
        let found = hints(
            &parameters,
            &[argument(6, Some("name")), argument(12, None)],
        );
        assert_eq!(
            found,
            vec![ParameterHint {
                line: 0,
                column: 12,
                label: "age:".into()
            }]
        );
    }

    #[test]
    fn rest_parameter_labels_only_its_first_argument() {
        let parameters = [parameter("format", false), parameter("values", true)];
        let found = hints(
            &parameters,
            &[argument(1, None), argument(5, None), argument(9, None)],
        );
        let labels: Vec<_> = found.iter().map(|hint| hint.label.as_str()).collect();
        assert_eq!(labels, ["format:", "...values:"]);
    }

    #[test]
    fn stops_at_spread_named_and_extra_arguments() {
        let parameters = [parameter("a", false), parameter("", false)];
        let mut spread = argument(4, None);
        spread.spread = true;
        assert_eq!(hints(&parameters, &[spread]), Vec::new());
        // Destructured parameter: no label, but the next position still counts.
        let found = hints(
            &parameters,
            &[argument(1, None), argument(3, None), argument(5, None)],
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            hints(&[parameter("$id", false)], &[argument(1, Some("$id"))]),
            Vec::new()
        );
    }
}
