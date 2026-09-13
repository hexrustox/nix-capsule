//! Shell quoting for bash commands that the ctl renders as strings: the
//! server launch command, the interactive escape hatch, and the `setup-env`
//! export script.

/// Single-quote-escape a value so it is safe inside single-quoted bash text:
/// each `'` becomes `'\''`, closing and reopening the quotes around it.
pub fn shell_escape(value: &str) -> String {
    value.replace('\'', "'\\''")
}

#[cfg(test)]
mod tests {
    use super::shell_escape;
    use test_case::test_case;

    #[test_case("plain" => "plain" ; "no_special_chars_pass_through")]
    #[test_case("" => "" ; "empty_is_empty")]
    #[test_case("/nix/store/xyz/bin/bash" => "/nix/store/xyz/bin/bash" ; "path_passes_through")]
    #[test_case("a b c" => "a b c" ; "spaces_pass_through_inside_quotes")]
    #[test_case("it's" => "it'\\''s" ; "embedded_quote_closes_reopens")]
    #[test_case("'" => "'\\''" ; "lone_quote_closes_reopens")]
    #[test_case("'a'" => "'\\''a'\\''" ; "surrounding_quotes_each_escape")]
    fn escape_cases(value: &str) -> String {
        shell_escape(value)
    }
}
