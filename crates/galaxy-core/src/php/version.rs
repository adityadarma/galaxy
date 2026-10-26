use std::path::Path;

/// PHP releases whose official `php-src` declarations are bundled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PhpVersion {
    V80,
    V81,
    V82,
    V83,
    V84,
    V85,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionSelection {
    Supported(PhpVersion),
    Unsupported,
}

impl PhpVersion {
    pub const ALL: [Self; 6] = [
        Self::V80,
        Self::V81,
        Self::V82,
        Self::V83,
        Self::V84,
        Self::V85,
    ];
}

/// Selects the newest bundled PHP version permitted by Composer. `platform`
/// takes precedence because it is Composer's explicit dependency target.
pub fn for_project(root: &Path) -> VersionSelection {
    let Ok(text) = std::fs::read_to_string(root.join("composer.json")) else {
        return VersionSelection::Supported(PhpVersion::V85);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return VersionSelection::Supported(PhpVersion::V85);
    };
    let constraint = value["config"]["platform"]["php"]
        .as_str()
        .or_else(|| value["require"]["php"].as_str());
    match constraint {
        Some(constraint) => PhpVersion::ALL
            .into_iter()
            .rev()
            .find(|version| permits(constraint, *version))
            .map(VersionSelection::Supported)
            .unwrap_or(VersionSelection::Unsupported),
        None => VersionSelection::Supported(PhpVersion::V85),
    }
}

fn permits(constraint: &str, version: PhpVersion) -> bool {
    // Composer constraints are expressive; this covers version comparisons,
    // wildcards, caret/tilde ranges, and `||`, without guessing PHP 7.x.
    constraint.split("||").any(|part| {
        part.split(',')
            .all(|term| permits_term(term.trim(), version))
    })
}

fn permits_term(term: &str, version: PhpVersion) -> bool {
    if term.is_empty() || term == "*" {
        return true;
    }
    let (operator, raw) = [">=", "<=", ">", "<", "^", "~", "="]
        .into_iter()
        .find_map(|operator| term.strip_prefix(operator).map(|raw| (operator, raw)))
        .unwrap_or(("=", term));
    let Some((major, minor)) = parse_minor(raw) else {
        return false;
    };
    let current = match version {
        PhpVersion::V80 => (8, 0),
        PhpVersion::V81 => (8, 1),
        PhpVersion::V82 => (8, 2),
        PhpVersion::V83 => (8, 3),
        PhpVersion::V84 => (8, 4),
        PhpVersion::V85 => (8, 5),
    };
    let target = (major, minor);
    match operator {
        ">=" => current >= target,
        ">" => current > target,
        "<=" => current <= target,
        "<" => current < target,
        "^" => current >= target && current.0 == target.0,
        "~" => current >= target && current.0 == target.0 && current.1 == target.1,
        _ if raw.contains('*') || raw.contains('x') || raw.contains('X') => {
            current.0 == target.0 && current.1 == target.1
        }
        _ => current == target,
    }
}

fn parse_minor(raw: &str) -> Option<(u8, u8)> {
    let mut parts = raw.trim().trim_start_matches('v').split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts
        .next()
        .unwrap_or("0")
        .trim_matches(|c| c == '*' || c == 'x' || c == 'X')
        .parse()
        .ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_latest_permitted_baseline() {
        assert_eq!(
            PhpVersion::ALL
                .into_iter()
                .rev()
                .find(|v| permits("^8.1", *v)),
            Some(PhpVersion::V85)
        );
        assert_eq!(
            PhpVersion::ALL
                .into_iter()
                .rev()
                .find(|v| permits("^8.5", *v)),
            Some(PhpVersion::V85)
        );
        assert_eq!(
            PhpVersion::ALL
                .into_iter()
                .rev()
                .find(|v| permits("~8.2", *v)),
            Some(PhpVersion::V82)
        );
        assert_eq!(
            PhpVersion::ALL
                .into_iter()
                .rev()
                .find(|v| permits("^7.4", *v)),
            None
        );
    }

    #[test]
    fn reads_composer_platform_before_require() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("composer.json"),
            r#"{"require":{"php":"^8.5"},"config":{"platform":{"php":"8.2.29"}}}"#,
        )
        .unwrap();
        assert_eq!(
            for_project(dir.path()),
            VersionSelection::Supported(PhpVersion::V82)
        );

        std::fs::write(
            dir.path().join("composer.json"),
            r#"{"require":{"php":"^7.4"}}"#,
        )
        .unwrap();
        assert_eq!(for_project(dir.path()), VersionSelection::Unsupported);
    }
}
