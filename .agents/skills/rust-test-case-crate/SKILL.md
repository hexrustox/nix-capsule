---
name: rust-test-case-crate
description: Write parametrized Rust tests with the `test-case` crate's `#[test_case(...)]` macro — inputs, expected-output matchers, `ignore`/`inconclusive` modifiers, comments, async, and extra attributes. Use when writing, adding, or reviewing `#[test_case]` tests, or choosing an output matcher.
---

# test-case

Parametrize one test function across many cases with the `test_case` macro. One `#[test_case(...)]` attribute per case, stacked above the function.

Grammar: `#[test_case(inputs (=> modifiers output_matcher)? (; "comment")?)]`

## Setup

Add to `Cargo.toml` dev-dependencies and import the macro:

```toml
[dev-dependencies]
test-case = "3"
```

```rust
use test_case::test_case;
```

The stdlib ships its own `test_case` attribute, so always import or fully-qualify `test_case::test_case` — never rely on it being in scope.

## Inputs — simple cases

Inputs are comma-separated and match function arguments by position; the type comes from the argument at that position, so each input must be implicitly castable to it. Use `_` for an argument a case does not exercise. Without an output part the case is *simple*: assertions live in the body, or the function returns `Result<(), E>` and `?`-propagates like a plain `#[test]`.

```rust
#[test_case(1 ; "one")]
#[test_case(3 ; "three")]
fn is_odd(i: i32) {
    assert_eq!(i % 2, 1);
}

#[test_case("input" ; "five_chars")]
fn is_5_chars_long(s: &str) -> Result<(), Box<dyn std::error::Error>> {
    if s.len() == 5 {
        Ok(())
    } else {
        Err("Isn't 5 characters long".to_string().into())
    }
}
```

## Output matchers

An output part makes the case *complex*: `=>` followed by optional modifiers then a matcher. Pick the matcher by what the function returns and how strict the check must be.

**Equality** — compare with `assert_eq!`:

```rust
#[test_case(2 => true)]
#[test_case(0 => ignore true)]
fn is_natural(number: i32) -> bool {
    number >= 0
}
```

**`matches`** — pattern-match a non-`Eq` return (results, enums); an `if` guard narrows the arm:

```rust
#[test_case(3 => matches Ok(3))]
#[test_case(4 => matches Ok(_))]
#[test_case(5 => matches Ok(v) if v == 8)]
#[test_case(-1 => matches Err(_))]
fn fib(seq_idx: i32) -> Result<i32, ()> {
    if seq_idx < 0 { Err(()) } else if seq_idx < 2 { Ok(1) }
    else { Ok(fib(seq_idx - 1).unwrap() + fib(seq_idx - 2).unwrap()) }
}
```

**`panics`** — the case must panic; a string argument is the panic-message substring (`#[should_panic(expected = ...)]`). Always pass the string — `panics` alone can fail to compile with "trait bound `(): std::process::Termination` is not satisfied"; use `panics ""` when any panic message is fine:

```rust
#[test_case(2.0, 0.0 => panics "")]
#[test_case(2.0, -0.0 => panics "Division by zero")]
#[test_case(2.0, 1.0 => 2.0)]
fn div(dividend: f32, divisor: f32) -> f32 {
    if divisor.abs() < f32::EPSILON {
        panic!("Division by zero")
    }
    dividend / divisor
}
```

**`with` closure** — a closure over the actual value that asserts; it must contain an assertion and return `()`:

```rust
#[test_case(2.0 => 0.0)]
#[test_case(0.0 => with |i: f64| assert!(i.is_nan()))]
fn test_division(i: f64) -> f64 {
    0.0 / i
}
```

**`using` function** — a validator path, or an expression returning a closure accepting the return type:

```rust
fn simple_validate(actual: u64) {
    assert_eq!(actual, 2)
}

#[test_case(2 => using simple_validate)]
#[test_case(1 => using |actual: u64| assert_eq!(actual, 1))]
fn id(input: u64) -> u64 {
    input
}
```

**`it` / `is` matchers** — human-readable checks, no body assertion needed:

- Numbers: `is eq`/`equal_to`, `is lt`/`less_than`, `is gt`/`greater_than`, `is leq`/`less_or_equal_than`, `is geq`/`greater_or_equal_than`, `is almost X precision P` (tolerance).
- Filesystem: `is existing_path`, `is file`, `is dir`/`directory`.
- Containers: `it contains X`, `it contains_in_order [a, b]`.
- Logic: combine with `not`, `and`, `or`; the parser has no precedence, so parenthesize whenever `and` and `or` mix.

```rust
#[test_case(1.0 => is greater_or_equal_than 1.0 ; "geq")]
#[test_case(1.0 => is almost_equal_to 2.1 precision 0.15 ; "almost")]
fn times_two(input: f64) -> f64 { input * 2.0 }

#[test_case("Cargo.toml" => is existing_path ; "exists")]
#[test_case("src/" => is dir ; "dir")]
fn path_from_str(val: &str) -> std::path::PathBuf { std::path::PathBuf::from(val) }

#[test_case(vec![1, 2, 3, 4] => it contains 2 and contains_in_order [3, 4])]
#[test_case(vec![1, 2, 3] => it (contains 1 or contains 4) and not contains 7)]
fn passthrough(items: Vec<u64>) -> Vec<u64> { items }
```

## Modifiers

`ignore` / `inconclusive` (synonyms) skip the case — the per-case `#[ignore]`:

```rust
#[test_case(9 => ignore 3)]
#[test_case(4 => inconclusive["not implemented"] 2)]
fn sqrt(number: u64) -> u64 { number.isqrt() }
```

## Comment and test names

The trailing `; "text"` is the case's name in `cargo test` output — `parent::fn::text`. Without it the name is generated from the arguments and expected output. Keep the string snake_case and behavior + pinned outcome.

## Extra attributes and async

Additional attributes — `#[tokio::test]`, `#[allow(...)]`, and the rest — must come *after* the `#[test_case(...)]` lines:

```rust
#[test_case(1 => 1)]
#[tokio::test]
#[allow(clippy::non_camel_case_types)]
async fn async_case(i: u32) -> u32 { i }
```

## Reference

- Syntax: https://github.com/frondeus/test-case/wiki/Syntax
- API docs: https://docs.rs/test-case/latest/test_case/attr.test_case.html
- Test names: https://github.com/frondeus/test-case/wiki/Test-Names
- Additional attributes & async: https://github.com/frondeus/test-case/wiki/Additional-Attributes-%26-Async
