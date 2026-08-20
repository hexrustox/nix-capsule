---
name: choosing-test-type
description: Decide which kind of test to write — unit, integration, property, or none — via a language-agnostic decision tree. Use when writing or adding tests, choosing a test type for a behavior, or reviewing whether a suite covers the behaviors that can break.
---

Decide the test types for a behavior. Run the tree once per behavior — writing a new test, or auditing a suite (each existing test should sit on the branch(es) its behavior falls on). The branches answer independently, so a behavior may earn more than one type; skip a type whose risk another chosen type already covers, and let the no-test branch win only when nothing else does.

## The tree

1. **Is the risk in the boundary?** Does the behavior's correctness depend on wiring to a real database, network service, filesystem, clock, or external process — where a stub would void what can break? If yes, add an **integration test**.
   - Assert the contract against the real boundary: the query against the schema, the request against the service's semantics, the values carried through the wiring. Done when the integration proves the boundary holds that no fake could.

2. **Is the risk in the input space?** Is the behavior a transformation over a wide or unknown set of inputs where you can state an invariant that must hold for every input — a round-trip (`decode(encode(x)) == x`), idempotence, sortedness, a bound, never-throws? If yes, add a **property test**.
   - State the invariant in one line first, then generate inputs and assert it. A failing run shrinks to the minimal counterexample. Done when the invariant holds across generated inputs.

3. **Is the unit's own logic the risk?** Is the behavior the isolated logic of a function or module, callable with collaborators stubbed and inputs pinned? If yes, add a **unit test**.
   - Stub the collaborators and pin concrete examples as assertions. Done when every distinct path the logic can take is pinned by an example.

4. **Did no branch above answer?** Then is the path thin — single-use glue or throwaway scaffold that cannot change an outcome? Write **no test** — and default toward writing, so this branch has to earn itself. Done when you can name the outcome it cannot change.

A behavior answers more than one branch when each holds a distinct risk. A transformation over a wide input space earns both a property test (branch 2) and a unit test (branch 3): the invariant guards the space, the unit test pins the logic's paths. A boundary wired through logic earns both an integration test (branch 1) and a unit test (branch 3).

The guard against doubling up: a type earns place only for the risk it alone can break. A boundary that merely passes data through is fake-able — it carries no risk of its own, so its risk is the transformation and it belongs on branches 2–3, not on a redundant integration test. Likewise, when a property test already ranges over the inputs, a unit test only earns its place if it pins a path the property run would not catch; when a unit test already pins every path, no further type earns its place for that same risk.