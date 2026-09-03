//! Project-name derivation from the project root's basename.

/// Sanitize a basename into a project name: every non-ASCII-alphanumeric
/// character joins the surrounding run into a single `-`, leading and
/// trailing `-` are stripped. `None` when nothing survives — the caller
/// turns that into the hard "set `project`" error.
pub fn sanitize(basename: &str) -> Option<String> {
    let mut name = String::with_capacity(basename.len());
    let mut run = false;
    for ch in basename.chars() {
        if ch.is_ascii_alphanumeric() {
            if run && !name.is_empty() {
                name.push('-');
            }
            name.push(ch);
            run = false;
        } else {
            run = true;
        }
    }
    let trimmed = name.trim_matches('-').to_owned();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn a_clean_basename_passes_through() {
        assert_eq!(sanitize("hello").as_deref(), Some("hello"));
    }

    #[test]
    fn non_alphanumeric_runs_collapse_to_one_dash() {
        assert_eq!(sanitize("my_project").as_deref(), Some("my-project"));
        assert_eq!(sanitize("a..b").as_deref(), Some("a-b"));
        assert_eq!(sanitize("a.-.b").as_deref(), Some("a-b"));
        assert_eq!(sanitize("v1.2").as_deref(), Some("v1-2"));
    }

    #[test]
    fn edges_are_stripped() {
        assert_eq!(sanitize("-lead-").as_deref(), Some("lead"));
        assert_eq!(sanitize(".dot.").as_deref(), Some("dot"));
    }

    #[test]
    fn case_is_preserved() {
        assert_eq!(sanitize("My.App").as_deref(), Some("My-App"));
    }

    #[test]
    fn non_ascii_letters_are_not_alphanumeric() {
        assert_eq!(sanitize("münchen").as_deref(), Some("m-nchen"));
    }

    #[test]
    fn nothing_surviving_is_none() {
        assert_eq!(sanitize("###"), None);
        assert_eq!(sanitize(""), None);
        assert_eq!(sanitize("---"), None);
    }
}
