---
name: writing-test-cases
description: Write test cases for a behavior — derive expected outcomes from the spec before the implementation, one case per distinct path across equivalence classes and boundaries, assert the outcome not the implementation, isolate for determinism. Use when writing or adding tests, choosing cases, or reviewing whether a suite catches a regression.
---

This skill designs the *cases* within a chosen test type, any language. If the test types for a behavior are undecided, see Pointers.

Design the cases for one behavior at a time. A case is an input and the outcome it must pin. A set is good when every distinct path the behavior can take is pinned by exactly one case, and each assertion is strong enough to fail if the behavior broke.

## 1. Derive the contract first, spec-first

State the behavior as a one-line contract *before* reading the implementation: given this input, the outcome is this value, this state change, or this error. Expected outcomes come from the contract, never from the code. Deriving from the code encodes the code — a test that pins today's bug.

Done when you can write a case's expected outcome with the implementation out of view.

## 2. Map the case space

Enumerate the behavior's distinct paths: the happy path, the boundaries (empty, min, max, first, last, zero, off-by-one, overflow), the invalid and error inputs (one per failure path), and the state transitions where the behavior is stateful. Group inputs into equivalence classes — values that take the same path. One case per class; permuting values within a class adds no coverage.

Done when every distinct path has exactly one case, and no two cases share a path.

## 3. Pin each case with a strong assertion

Assert the outcome as a fact from the contract: the value that matters, the state change, the error variant. A weak assertion — `is_ok()` on the payload, no assertion, a tautology — cannot fail for the behavior and earns nothing. Assert the contract, not the implementation: no internals, serialization, or ordering the contract does not specify.

Done when each case would fail if the contract were violated, and nothing pins an implementation detail.

## 4. Isolate for determinism

Each case standalone: no shared mutable state with other cases, no dependence on run order, and control of time, clock, network, filesystem, and randomness. Same input, same pass or fail, every run.

Done when each case passes alone, in any order, repeatedly.

## 5. Name for diagnosis

Name each case as behavior + pinned outcome, so a failing run names the contract broken. In Rust, see Pointers for the naming rules.

Done when each case name states what it tests and what it pins.

## Reviewing a suite

Audit existing tests through the same steps in reverse: name the contract each test pins (a test pinning no contract is dead), check it maps to one distinct path (duplicates add nothing), check the assertion is strong enough to fail for the behavior, check isolation.

## Pointers

- When the test types for a behavior are undecided, run the `choosing-test-type` skill and follow it.
- When naming cases in Rust, run the `rust-code-style` skill and follow it.
