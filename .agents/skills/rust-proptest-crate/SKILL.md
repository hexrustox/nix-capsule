---
name: rust-proptest-crate
description: "Write Rust property-based tests with the `proptest` crate — the `proptest!` macro, non-panicking assertions, input filtering, strategy unions, custom strategies, and the `#[property_test]` attribute macro. Use when writing, adding, or reviewing proptest tests, choosing or building a strategy for a generated input, or interpreting a proptest failure (shrinking, regressions)."
---

# proptest

Property-based testing for Rust: generate inputs from strategies, assert a property holds for every input, and shrink the first failing input to a minimal counterexample. One strategy per input, unlike QuickCheck's one per type.

All of proptest's macros live at the crate root and re-export through the prelude: `proptest!`, `prop_assert!`, `prop_assert_eq!`, `prop_assert_ne!`, `prop_assume!`, `prop_oneof!`, `prop_compose!`, and the `#[property_test]` attribute macro (feature `attr-macro`).

## Setup

Add to `Cargo.toml` dev-dependencies and import the prelude in every file that uses proptest:

```toml
[dev-dependencies]
proptest = "1"
```

```rust
use proptest::prelude::*;
```

The prelude brings in all seven function-like macros, `any` / `any_with`, the `Arbitrary` trait, the `Strategy` trait, `Just`, `ProptestConfig` (a.k.a. `Config`), `TestCaseError`, and the whole crate as `prop` (so `prop::collection::vec(...)` works with no extra import). Enable the `attr-macro` feature only if you use `#[property_test]`:

```toml
[dev-dependencies]
proptest = { version = "1", features = ["attr-macro"] }
```

## Worked example

A property test for a `parse_date`-style function. The shape is always the same: an `in` binding names a strategy, and the body asserts the property with a `prop_assert*` macro.

```rust
fn parse_date(s: &str) -> Option<(u32, u32, u32)> {
    // ... parses "YYYY-MM-DD" ...
}

proptest! {
    #[test]
    fn parses_all_valid_dates(s in "[0-9]{4}-[0-9]{2}-[0-9]{2}") {
        parse_date(&s).unwrap();
    }

    #[test]
    fn parses_date_back_to_original(y in 0u32..10000,
                                    m in 1u32..13, d in 1u32..32) {
        let (y2, m2, d2) = parse_date(
            &format!("{:04}-{:02}-{:02}", y, m, d)).unwrap();
        prop_assert_eq!((y, m, d), (y2, m2, d2));
    }
}
```

On a failure proptest prints the *minimal* counterexample, then the shrinking steps that led to it:

```text
Test failed: assertion failed: `(left == right)` (left: `(0, 10, 1)`,
  right: `(0, 0, 1)`) at tests/date.rs:46; minimal failing input: y = 0,
  m = 10, d = 1
successes: 2
local rejects: 0
global rejects: 0
```

The failure is written to a `proptest-regressions/` file keyed by the test's source file. Add these to version control (`git add proptest-regressions`) so regressions re-run on CI; a recorded case is replayed before random generation. A persisted failure is worth pinning as a plain unit test too — it is a hand-crafted edge case proptest bought you.

## The `proptest!` macro

The workhorse. Write test functions inside `proptest! { ... }`; each needs its own `#[test]` attribute and may have any number of parameters. One block can hold several tests.

**Block form:**

```rust
proptest! {
    #[test]
    fn addition_is_commutative(a: u8, b: u8) {
        prop_assert_eq!(a as u16 + b as u16, b as u16 + a as u16);
    }
}
```

### Parameters

Two binding styles, freely mixable in one function:

- `pattern in strategy_expr` — the strategy on the right must evaluate to a `Strategy`; the pattern on the left is destructured against the generated value (`ref s in ".*"`, `(a, b) in (0..10, 0..10)`).
- `name: Type` — the type must implement `Arbitrary`; proptest calls `any::<Type>()` for you. Due to a `macro_rules!` limitation this form fails in some positions — fall back to `(pattern): Type` when it does.

```rust
proptest! {
    #[test]
    fn mixed(s in ".*", n: i32) {
        let cat = format!("{}{}", s, n);
        prop_assert_eq!(s.len() + 1, cat.len());
    }
}
```

### Body semantics

The body is wrapped so its tail value becomes `Ok(())` — the test "fails" only when the body returns an error. Two ways to fail:

- `prop_assert!` / `prop_assert_eq!` / `prop_assert_ne!` — assert and return `Err(TestCaseError)` on failure (clean: the only output on a failing case is the final minimal panic).
- Return `Result<_, TestCaseError>` from a helper and `?` it — helper macros compose like this.
- Plain `assert!` / `panic!` also fail the test, but Rust prints each intermediate panic to stderr, so failures get noisy on the way to the minimal case. Prefer `prop_assert*`.

```rust
fn check_sorted_after_append(v: &mut Vec<i32>, x: i32) -> Result<(), TestCaseError> {
    v.push(x);
    prop_assert!(v.windows(2).all(|w| w[0] <= w[1]));
    Ok(())
}

proptest! {
    #[test]
    fn append_keeps_sorted(mut v in prop::collection::vec(0..100, 0..10), x in 0..100) {
        v.sort();
        check_sorted_after_append(&mut v, x)?;
    }
}
```

### Per-block configuration

Start the block with `#![proptest_config(expr)]` where `expr` is a `ProptestConfig` (or a reference to one):

```rust
proptest! {
    #![proptest_config(ProptestConfig {
        cases: 99, ..ProptestConfig::default()
    })]
    #[test]
    fn adds(a in 0..1000, b in 0..1000) {
        prop_assert!(a + b <= 2000);
    }
}
```

### Closure form

For tests needing one expensive setup shared across several runs (e.g. building a fixture), `proptest!` takes a closure: `proptest!(|(x in 0..42, y in 0..100)| { ... })`. Note the extra parentheses around the arguments. `move` closures are supported, and a `ProptestConfig` may be passed as the first argument. The `fork` and `timeout` config options are **not** supported in closure form.

```rust
#[test]
fn my_test() {
    let big_struct = very_expensive_function();
    proptest!(|(x in 0u32..42, y in 1000u32..100000u32)| {
        // uses big_struct
    });
    proptest!(move |(x in 0u32..42)| {
        // consumes big_struct
    });
    proptest!(ProptestConfig::with_cases(1000), |(x: i32)| {
        // custom config
    });
}
```

## Assertions: `prop_assert!`, `prop_assert_eq!`, `prop_assert_ne!`

`assert!` / `assert_eq!` / `assert_ne!` that return a test failure (`Result<_, TestCaseError>`) instead of panicking. Use them anywhere a function returns `Result<_, TestCaseError>` — inside `proptest!` bodies and in helpers it calls. Optional trailing format string and args become the failure message (appended after the default message for the `_eq`/`_ne` forms).

```rust
proptest! {
    #[test]
    fn triangle_inequality(a in 0.0f64..10.0, b in 0.0f64..10.0) {
        prop_assert!((a * a + b * b).sqrt() <= a + b);
        prop_assert!(
            (a * a + b * b).sqrt() <= a + b,
            "Triangle inequality failed for ({}, {})", a, b
        );
        assert_from_other_function(a, b)?;
    }
}

fn assert_from_other_function(a: f64, b: f64) -> Result<(), TestCaseError> {
    prop_assert!((a * a + b * b).sqrt() <= a + b);
    Ok(())
}
```

`prop_assert_eq!` / `prop_assert_ne!` accept a custom message too:

```rust
prop_assert_eq!(a.len() + b.len(), cat.len(), "a = {:?}, b = {:?}", a, b);
```

## `prop_assume!`

Reject the current input when an assumption is false: `Err(TestCaseError::Reject)`. A rejected input does not count as a case and does not fail the test — used to skip inputs the property doesn't apply to. Format string and args optional.

```rust
proptest! {
    #[test]
    fn square_root_magnitude(x in 0i32..100) {
        prop_assume!(x > 0, "zero has no meaningful magnitude");
        prop_assert!((x as f64).sqrt() >= 0.0);
    }
}
```

Prefer building the constraint into the strategy (`prop_filter`, ranges, regex) over `prop_assume!` where you can: a rejected input is wasted generation, and too many rejects trips `max_local_rejects` and fails the test. `prop_assume!` is for assumptions that are hard to express as a strategy — usually ones depending on *multiple* generated values at once.

## `prop_oneof!`

A strategy that picks one of several strategies. Uniform when strategies are listed bare; weighted with a `u32 =>` before each strategy. Values shrink across elements toward the *earlier* ones, so list strategies in ascending complexity. The concrete type returned varies with the number of items (a single item is passed through unmodified) — don't rely on it.

```rust
#[derive(Clone, Copy, Debug)]
enum MyEnum {
    Big(u64),
    Medium(u32),
    Little(i16),
}

let my_enum_strategy = prop_oneof![
    prop::num::i16::ANY.prop_map(MyEnum::Little),
    prop::num::u32::ANY.prop_map(MyEnum::Medium),
    prop::num::u64::ANY.prop_map(MyEnum::Big),
];

let my_weighted_strategy = prop_oneof![
    1 => prop::num::i16::ANY.prop_map(MyEnum::Little),
    2 => prop::num::u32::ANY.prop_map(MyEnum::Medium),
    1 => prop::num::u64::ANY.prop_map(MyEnum::Big),
];
```

## `prop_compose!`

Define a function that produces a new strategy from existing ones — proptest's answer to building strategies for structs.

**Two argument lists** (sugar over a tuple + `prop_map`): the first list is the function's real parameters; the second uses `in strategy` syntax and generates the remaining inputs; the body builds the value from both lists.

```rust
#[derive(Clone, Debug)]
struct MyStruct {
    integer: u32,
    string: String,
}

prop_compose! {
    fn my_struct_strategy(max_integer: u32)
                         (integer in 0..max_integer, string in ".*")
                         -> MyStruct {
        MyStruct { integer, string }
    }
}
```

**Three argument lists** (sugar over `prop_flat_map` + `prop_map`): the third list can depend on values from the first two, for strategies that depend on other strategies. The body does **not** see the second list — pass values through explicitly.

```rust
prop_compose! {
    fn nearby_numbers()(centre in -1000..1000)
                     (a in centre - 10..centre + 10,
                      b in centre - 10..centre + 10)
                     -> (i32, i32) {
        (a, b)
    }
}

prop_compose! {
    fn vec_and_index
        (max_length: usize)
        (vec in prop::collection::vec(1..10, 1..max_length))
        (index in 0..vec.len(), vec in Just(vec))
        -> (Vec<i32>, usize)
    {
        (vec, index)
    }
}
```

Add visibility and other modifiers (`pub`, `pub(crate)`, `unsafe`) — non-visibility modifiers in brackets after the visibility, before `fn`:

```rust
prop_compose! {
    pub(crate) [unsafe] fn pointer()(v in prop::num::usize::ANY) -> *const () {
        v as *const ()
    }
}
```

Limits: you cannot filter inside `prop_compose!` (use `prop_filter` on the result), and you cannot nest more than two strategy layers or run arbitrary logic between them (hand-roll with `prop_flat_map` when you need either).

## `#[property_test]`

Attribute macro (feature `attr-macro`) that turns a plain test function into a property test: each parameter is generated from its type's `Arbitrary` impl, exactly as if written `param in any::<Type>()` inside `proptest!`.

```rust
#[property_test]
fn foo(x: i32) {
    assert_eq!(x, x);
}
```

is roughly equivalent to:

```rust
proptest! {
    #[test]
    fn foo(x in any::<i32>()) {
        assert_eq!(x, x);
    }
}
```

**Custom strategies per argument** — one `#[strategy = <expr>]` per argument:

```rust
#[property_test]
fn foo(#[strategy = "[0-9]*"] s: String) {
    for c in s.chars() {
        assert!(c.is_numeric());
    }
}
```

**Custom config** — a `config` attribute with an expression evaluating to a `ProptestConfig`:

```rust
#[property_test(config = "ProptestConfig { cases: 100, ..ProptestConfig::default() }")]
fn foo(x: i32) {
    assert_eq!(x, x);
}
```

This macro generates a helper struct whose exact name, fields, and even existence are implementation details with no semver guarantee.

## Pointers

- Read `references/strategies.md` when building a strategy expression (`in strategy`, `prop_compose!`, `#[strategy = ...]`) or tuning the runner — it catalogs every strategy module, the `Strategy` combinator suite, `ProptestConfig` fields and env vars, and direct `TestRunner` use.
- API docs: https://docs.rs/proptest/latest/proptest/
- Tutorial: https://proptest-rs.github.io/proptest/proptest/index.html
