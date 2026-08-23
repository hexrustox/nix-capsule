# proptest: strategies, combinators, config, runner

Load this file when you need to build a strategy expression (`in strategy`, `prop_compose!`, `#[strategy = ...]`), pick the right strategy type for a generated input, or tune how proptest runs. Everything here is reachable through `proptest::prelude::*` (the crate itself as `prop`, plus the names listed in each section).

## How a strategy works

A `Strategy` knows how to generate values of a type and how to shrink them. Two concepts to keep straight:

- **Strategy** — the generator you name in `in strategy` expressions. `Strategy` requires `Debug` on the strategy itself (a `prop_flat_map` constraint).
- **ValueTree** — a generated value plus its shrinking path; the strategy's associated type `Tree`. Shrinking steps toward simpler values.

`any::<T>()` returns `T`'s *canonical* strategy via `Arbitrary`; pick a bespoke strategy when you want a specific distribution, range, or shape rather than the canonical one. A strategy must be deterministic: given a seeded runner it must produce the same tree every run, or failure persistence breaks.

## Combinators

Chain these to refine a strategy. They compose on one line or across several; prefer the more-specific one where listed.

- `.prop_map(f)` — transform generated values (`fn(Value) -> NewValue`). Shrinking stays in terms of the source value, so `f` must be deterministic and `Fn` (not `FnMut`). Used constantly (building structs, mapping enum variants).
- `.prop_map_into::<O>()` — transform via `Into<O>`; prefer it over `prop_map` when `Value: Into<O>` (clearer and faster).
- `.prop_filter(whence, f)` — keep values where `f(&Value)` is true, else reject and regenerate; `whence` names where/why the rejection happened (shown in `local rejects`). Naive rejection sampling: costly, and filters can block shrinking. Prefer it over `prop_assume!` in the body, but prefer a built-in strategy (range, regex) over filtering.
- `.prop_filter_map(whence, f)` — `f(Value) -> Option<O>`: map the kept values, reject `None`. Better than `.prop_map(..).prop_filter(..)`.
- `.prop_flat_map(f)` — generate a value, then a *new strategy* from it (`fn(Value) -> impl Strategy`). The primitive behind `prop_compose!`'s three-list form and `.prop_recursive`. Shrinking searches the derived strategy for new failures, so nested flat-maps risk exponential shrink time; a shared `max_flat_map_regens` counter bounds the search.
- `.prop_ind_flat_map(f)` / `.prop_ind_flat_map2(f)` — flat-map variants that do *not* shrink the input that derived the inner strategy. For most cases where an invariant must survive shrinking, `prop_flat_map` is right (see the trait docs' worked comparison).
- `.prop_perturb(f)` — `fn(Value, TestRng) -> Value`, e.g. jittering; the RNG is deterministic during shrinking.
- `.prop_shuffle()` — shuffle a `Vec`/slice strategy's values, then de-shuffle during shrinking (useful for testing permutation-insensitive code).
- `.no_shrink()` — suppress shrinking entirely; for linear-approximation-style properties where shrinking to the failure boundary hides the magnitude of the real problem.
- `.boxed()` — erase to `BoxedStrategy<V>` (cheap `Arc` clone); `.sboxed()` keeps `Send + Sync` as `SBoxedStrategy<V>`. Use when heterogeneous strategies must share one type (e.g. stored in a vec, or the recursive knot in `prop_recursive`).
- `.prop_union(other)` — pick uniformly from two strategies of the same type; if a value from `other` can shrink no further, it switches to `self`. Chain with `.or()` (the `Union` method) rather than nesting calls, which skews the distribution. `prop_oneof!` is the ergonomic front-end.
- `.prop_recursive(depth, desired_size, expected_branch_size, recurse)` — recursive data (trees, JSON ASTs): `recurse` maps a `BoxedStrategy<Value>` at nesting depth n to a strategy at depth n+1. `depth` hard-limits branch levels, `desired_size` targets total elements, `expected_branch_size` bounds each collection (too small → oversized trees).
- `.prop_map_into`, `.prop_map`, `.prop_filter` etc. compose with `Just` to pin constants in `prop_oneof!`.

## Strategy catalog

The `Strategy`-producing constructors, by module. Prefix each with the module path (`prop::num::i32::ANY`, `prop::collection::vec(...)`, `prop::option::of(...)`); regexes are written as string literals directly in strategy position.

### Numbers — `prop::num`

- Ranges: `0..10`, `0i32..100`, `-10..10` are strategies over the integer type of the literal, biased toward edge values; floats work too (`0.0f64..10.0`), `..=` inclusive forms included.
- `prop::num::i32::ANY` (and `u32`, `i64`, `f64`, `usize`, ...) — the full range, uniform. Use when a range misses values your property cares about. The per-type submodules also give the exact shrink strategy (`BinarySearch` toward 0).
- `prop::num::sample_uniform_incl(low, high)` — uniform inclusive, for when a range's edge-bias is unwanted.

### Bools — `prop::bool`

- `prop::bool::ANY` — uniform `true`/`false`.
- `prop::bool::weighted(p)` — `true` with probability `p` (`1.0` = always true), useful for sparse structures and option-like trees.

### Chars — `prop::char`

- `prop::char::any()` — any Unicode scalar, with bias toward hard-to-handle characters.
- `prop::char::range('a', 'z')` — inclusive range.
- `prop::char::ranges(vec![('a','z'), ('A','Z')])` — union of inclusive ranges, for custom alphabets.
- `prop::char::select_char(...)` — internal selection helper (rarely used directly).

### Strings — `prop::string` (feature `std`)

A string literal in strategy position is a regular-expression strategy: `"[0-9]{4}-[0-9]{2}-[0-9]{2}"`, `".*"`, `"\\PC*"` (any non-control characters). For regexes built at runtime, use the module functions (they validate and return `Result`):

- `prop::string::string_regex("...")` — `String` matching the regex; `string_regex_parsed(...)` takes a pre-parsed `regex_syntax` expression.
- `prop::string::bytes_regex("...")` — `Vec<u8>` matching the regex; `bytes_regex_parsed(...)` likewise.
- The default `any::<String>()` / `any::<Vec<u8>>()` is the `StringParam` default: strings of non-control characters.

### Collections — `prop::collection`

Every constructor takes an element strategy and a *size*: an integer (exact size), a range (`0..10`), or a strategy. Key points:

- `vec(elem, size)` — `Vec<T>`; also `vec_deque`, `linked_list`, `binary_heap`, `btree_set`, `btree_map`, `hash_set` (`std`), `hash_map` (`std`). BTree/Hash *maps* take separate key and value strategies.
- Set/map strategies need unique keys; generation retries on collisions. For a `Vec` of non-unique elements keep `vec`.
- `size_range(...)` builds a `SizeRange` from a convertible value; `SizeRange` bounds a collection's length. A size of `0` is valid — guard properties that assume non-empty.

### Option / Result

- `prop::option::of(strategy)` — `Option<T>`, `None` and `Some` equally.
- `prop::option::weighted(prob, strategy)` — `Some` with probability `prob`, built via `prop::option::prob(p)` (a `Probability` in `[0.0, 1.0]`, default 0.5).
- `prop::result::maybe_ok(ok, err)` — `Result` mixing both; `maybe_err` is the same API with the shrink direction flipped (`maybe_ok` shrinks `Ok` → `Err` and suits code that bails on error; `maybe_err` shrinks `Err` → `Ok` and suits code that must back out). Weighted variants `maybe_ok_weighted` / `maybe_err_weighted` take a `Probability` first.
- Always-`Ok`: `base.prop_map(Ok)`.

### Tuples / arrays

- Tuples combine automatically: `(a, b, c)` where each element is a strategy is itself a strategy of tuples, up to 12 elements.
- `prop::array::uniform([T; N], strategy)` (and `uniform1`..`uniform32`) — fixed arrays drawn from one element strategy; arrays of up to 32 per-element strategies also work directly. Use `vec(elem, n)` if the length should shrink.

### Bits — `prop::bits`

Strategies for integers used as *bit fields* (the sensible shrink of `64` is `0`, clearing one bit — not `63`). Per-integer-type submodules:

- `prop::bits::u32::ANY` — all bits may be set.
- `prop::bits::u32::masked(0b1101)` — only bits in the mask may be set.
- `prop::bits::u32::between(0..8)` — bits in the index range may be set.
- `prop::bits::u32::sampled(...)` — a bounded number of bits set (for flags/bitfields).
- The `bitset` / `bool_vec` submodules cover `BitSet` and `Vec<bool>`.

### Paths — `prop::path` (feature `std`)

`PathBuf` implements `Arbitrary`; `PathParams` is its parameter type, so generate paths via `any::<PathBuf>()` or `any_with::<PathBuf>(PathParams::default())`. Keep real-filesystem tests inside a `TempDir` rather than trusting arbitrary paths.

### Sampling — `prop::sample`

Strategies over *fixed* collections (the collection is not itself a strategy):

- `prop::sample::select(&[...])` — pick one value uniformly.
- `prop::sample::subsequence(&[...], size)` — a uniformly-chosen subsequence of the given size range.
- `any::<prop::sample::Index>()` — an `Index`: a stand-in for `0..N` usable against a collection whose length is known only at test time. Materialize with `.index(len)`, `.get(&slice)`, or `.get_mut(&mut slice)`; it produces the same position no matter how many times it is used, and shrinks by binary search. Cannot be used directly as a slice index (`slice[index]`).
- `prop::sample::Selector` — generate random picks out of iterators.

### Range subsets — `prop::range_subset` (feature `std`)

- `prop::range_subset::range_subset(size, range)` — a `RangeSubset`: a `Vec` of subranges sampled from a fixed index range (e.g. "line numbers in a file").

### Constants

- `Just(value)` — a strategy yielding exactly one value (e.g. `Just(vec![1, 2, 3])`); combine with `prop_map`/`prop_oneof!` to pin a branch.

## `Arbitrary` and `any`

`Arbitrary` supplies a type's canonical strategy. Use `any::<T>()` (or `arbitrary()` where inference does the work) in `in strategy` position when the default distribution is fine. When `T`'s `Arbitrary` takes parameters — collections take a size — use `any_with::<T>(args)` (or `arbitrary_with`). Example: `any_with::<Vec<u8>>((0..10, ()))` (a `(SizeRange, T::Parameters)` tuple). Name parameter/strategy types with the `ParamsFor<T>` / `StrategyFor<T>` aliases rather than spelling out associated types.

Implement `Arbitrary` for your own types when `any::<MyType>()` should be the canonical choice: implement `fn arbitrary_with(args: Self::Parameters) -> Self::Strategy`, with `type Parameters` (defaults to `()`) and `type Strategy: Strategy<Value = Self>`. It must be the *canonical* strategy — a representative, well-distributed choice — not a one-off for a single test.

## `ProptestConfig` and the runner

Defaults: 256 cases, generous reject budgets, shrinking on. Override per-test with `#![proptest_config(...)]`, the closure-form first argument, or `#[property_test(config = "...")]`; globally via env vars (`std` feature only). Key fields of `ProptestConfig` (= `Config`):

- `cases` — successful cases required to pass (default 256). One-field shortcut: `ProptestConfig::with_cases(n)`.
- `max_local_rejects` — strategy-level rejects within one input distribution (default 65536).
- `max_global_rejects` — whole-input rejects (`prop_assume!`) before the test fails (default 1024).
- `max_shrink_iters` — cap on shrinking iterations (default `usize::MAX`).
- `max_flat_map_regens` — regeneration budget shared by all flat-map shrink searches (default 1000000).
- `verbose` — 0 silent (default), 1 log each case, 2 log shrinking too.
- `timeout` — per-case timeout in ms; requires `fork: true` (feature `fork`) — run each case in a forked process so a hang or abort in the test can't kill the runner.
- `failure_persistence` — where regressions are stored: `FileFailurePersistence::default()` (the `proptest-regressions/` directory), `Off`, or other layouts.
- `source_file` / `test_name` — how the regression file is named and keyed (set automatically by the macros).
- `rng_algorithm` — `RngAlgorithm::ChaCha` (default), `XorShift`, or `Seed` (with a seed, for reproducible runs).

Env vars read at runner start: `PROPTEST_CASES`, `PROPTEST_MAX_LOCAL_REJECTS`, `PROPTEST_MAX_GLOBAL_REJECTS`, `PROPTEST_MAX_SHRINK_ITERS`, `PROPTEST_MAX_FLAT_MAP_REGENS`, `PROPTEST_VERBOSE`, `PROPTEST_TIMEOUT`, `PROPTEST_FORK`, `PROPTEST_DISABLE_FAILURE_PERSISTENCE`, `PROPTEST_SEED`, `PROPTEST_RNG_ALGORITHM`.

### Direct `TestRunner` use

The macros wrap a `TestRunner`; use it directly when the strategy and runner must be separated from a test function:

```rust
use proptest::prelude::*;

let mut runner = TestRunner::new(ProptestConfig::with_cases(1000));
let result = runner.run(&(0..100, 0..100), |(a, b)| {
    prop_assert!(a + b <= 200);
    Ok(())
});
assert!(result.is_ok());
```

`runner.run(&strategy, closure)` runs the closure over generated inputs; the closure returns `Result<(), TestCaseError>`. `TestRng` is the seedable RNG handed to `prop_perturb`; `RngAlgorithm` picks the algorithm, and a seeded runner is what makes failure persistence and reruns reproducible.
