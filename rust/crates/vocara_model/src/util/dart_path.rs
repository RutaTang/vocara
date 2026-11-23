//! The POSIX context of Dart's `package:path` (what `import 'package:path/
//! path.dart' as p` means on macOS), for the functions the ported code uses.
//!
//! These differ from `std::path` in ways the stored data and the user-facing
//! labels depend on: `extension('.bashrc')` is empty, `extension('a.')` is
//! `.`, `dirname('a')` is `.`, trailing separators are ignored, and `join`
//! drops everything before an absolute part.

/// One parsed path: an optional root (`/`) and the parts between separators,
/// each with the separator before it (the last entry is the trailing one).
struct ParsedPath {
    root: Option<String>,
    parts: Vec<String>,
    separators: Vec<String>,
}

impl ParsedPath {
    fn parse(path: &str) -> ParsedPath {
        let (root, rest) = match path.strip_prefix('/') {
            Some(rest) => (Some("/".to_string()), rest),
            None => (None, path),
        };
        let mut parts = Vec::new();
        let mut separators = Vec::new();
        let bytes = rest.as_bytes();
        let mut start = 0;
        if !bytes.is_empty() && bytes[0] == b'/' {
            separators.push("/".to_string());
            start = 1;
        } else {
            separators.push(String::new());
        }
        let mut i = start;
        while i < bytes.len() {
            if bytes[i] == b'/' {
                parts.push(rest[start..i].to_string());
                separators.push("/".to_string());
                start = i + 1;
            }
            i += 1;
        }
        if start < bytes.len() {
            parts.push(rest[start..].to_string());
            separators.push(String::new());
        }
        ParsedPath {
            root,
            parts,
            separators,
        }
    }

    fn remove_trailing_separators(&mut self) {
        while self.parts.last().is_some_and(|p| p.is_empty()) {
            self.parts.pop();
            self.separators.pop();
        }
        if let Some(last) = self.separators.last_mut() {
            last.clear();
        }
    }

    fn split_extension(&self) -> (String, String) {
        let Some(file) = self.parts.iter().rev().find(|p| !p.is_empty()) else {
            return (String::new(), String::new());
        };
        if file == ".." {
            return ("..".to_string(), String::new());
        }
        // No dot, or a leading one ('.bashrc'): no extension.
        match file.rfind('.') {
            Some(dot) if dot > 0 => (file[..dot].to_string(), file[dot..].to_string()),
            _ => (file.clone(), String::new()),
        }
    }

    fn render(&self) -> String {
        let mut out = String::new();
        if let Some(root) = &self.root {
            out.push_str(root);
        }
        for (i, part) in self.parts.iter().enumerate() {
            out.push_str(&self.separators[i]);
            out.push_str(part);
        }
        if let Some(last) = self.separators.last() {
            out.push_str(last);
        }
        out
    }
}

/// `p.basename`: the last part, trailing separators ignored (`/` for the
/// root, empty for an empty path).
pub fn basename(path: &str) -> String {
    let mut parsed = ParsedPath::parse(path);
    parsed.remove_trailing_separators();
    match parsed.parts.last() {
        Some(last) => last.clone(),
        None => parsed.root.unwrap_or_default(),
    }
}

/// `p.basenameWithoutExtension`.
pub fn basename_without_extension(path: &str) -> String {
    ParsedPath::parse(path).split_extension().0
}

/// `p.extension`: from the last dot of the last part, dot included; empty
/// when there is none or it starts the name.
pub fn extension(path: &str) -> String {
    ParsedPath::parse(path).split_extension().1
}

/// `p.dirname`: everything before the last part (`.` for a bare relative
/// name, `/` under the root).
pub fn dirname(path: &str) -> String {
    let mut parsed = ParsedPath::parse(path);
    parsed.remove_trailing_separators();
    if parsed.parts.len() <= 1 {
        return parsed.root.unwrap_or_else(|| ".".to_string());
    }
    parsed.parts.pop();
    parsed.separators.pop();
    parsed.remove_trailing_separators();
    parsed.render()
}

/// `p.isAbsolute`.
pub fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

/// `p.join` / `p.joinAll`: empty parts are skipped, an absolute part
/// discards everything before it, and a separator is added only where the
/// previous part does not end with one.
pub fn join_all<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::new();
    let mut needs_separator = false;
    for part in parts.into_iter().filter(|p| !p.is_empty()) {
        if is_absolute(part) {
            out.clear();
            out.push_str(part);
        } else {
            if part.starts_with('/') {
                // Unreachable on POSIX (that is absolute), kept for shape.
            } else if needs_separator {
                out.push('/');
            }
            out.push_str(part);
        }
        needs_separator = !part.ends_with('/');
    }
    out
}

/// `p.join(a, b)`.
pub fn join(a: &str, b: &str) -> String {
    join_all([a, b])
}

/// The lexically normalized parts of [path], made absolute against the
/// current directory when relative (what `p.isWithin` compares).
fn absolute_parts(path: &str) -> Vec<String> {
    let full = if is_absolute(path) {
        path.to_string()
    } else {
        let cwd = std::env::current_dir()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "/".to_string());
        join(&cwd, path)
    };
    let mut out: Vec<String> = Vec::new();
    for part in full.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p.to_string()),
        }
    }
    out
}

/// `p.isWithin(parent, child)`: whether [child] lies strictly inside
/// [parent], comparing the normalized paths (`.`/`..` resolved lexically,
/// repeated separators ignored), never the file system.
pub fn is_within(parent: &str, child: &str) -> bool {
    let p = absolute_parts(parent);
    let c = absolute_parts(child);
    c.len() > p.len() && c[..p.len()] == p[..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_and_extension_follow_package_path() {
        assert_eq!(basename("/a/b/c.wav"), "c.wav");
        assert_eq!(basename("/a/b/"), "b");
        assert_eq!(basename("/"), "/");
        assert_eq!(basename(""), "");
        assert_eq!(extension("/a/b/c.tar.gz"), ".gz");
        assert_eq!(extension("~/.bashrc"), "");
        assert_eq!(extension("a."), ".");
        assert_eq!(extension("/a/b.d/"), ".d");
        assert_eq!(basename_without_extension("/a/b/c.wav"), "c");
        assert_eq!(basename_without_extension("/a/.notes"), ".notes");
    }

    #[test]
    fn dirname_and_join_follow_package_path() {
        assert_eq!(dirname("link.wav"), ".");
        assert_eq!(dirname("/a/b/c"), "/a/b");
        assert_eq!(dirname("/a"), "/");
        assert_eq!(dirname("/a/b/"), "/a");
        assert_eq!(dirname("a/b"), "a");
        assert_eq!(join(".", "made.wav"), "./made.wav");
        assert_eq!(join("/a/", "b"), "/a/b");
        assert_eq!(join("/a", "/b"), "/b");
        assert_eq!(join("", "b"), "b");
    }

    #[test]
    fn is_within_is_strict_and_lexical() {
        assert!(is_within("/a/b", "/a/b/c"));
        assert!(is_within("/a/b/", "/a/b/c/d"));
        assert!(!is_within("/a/b", "/a/b"));
        assert!(!is_within("/a/b", "/a/bc"));
        assert!(!is_within("/a/b", "/a/b/../c"));
        assert!(is_within("/a/./b", "/a/b//c"));
    }
}
