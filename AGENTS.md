# AGENTS.md

## Vocabulary

- Use `CONTEXT.md` when writing or editing code, specs, or docs: it fixes this
  repo's shared vocabulary (Host shell vs Container shell, Client, Server, Ctl,
  Wrapper, Runtime adapter, …) and lists the synonyms to avoid. Done when new
  text uses the CONTEXT.md terms.

## Behavior

- Use `spec/<component>.md` when changing how a component behaves: `client.md`,
  `server.md`, `ctl.md`, `protocol.md`, `env.md`, `runtime.md`, `paths.md`,
  `flake-api.md`. What exists and when it fires is defined in spec, not in code
  — change the spec with the code. Done when the spec matches the change.

## Style guides

- Use `docs/agents/error.md` when writing or editing user-visible error
  messages.
- Use `docs/agents/log.md` when adding or changing Server log lines.

## Agent skills

### Issue tracker

Issues and specs are tracked in this repo's GitHub Issues via the `gh` CLI.
See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical triage roles: `needs-triage`, `needs-info`,
`ready-for-agent`, `ready-for-human`, `wontfix`. See
`docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` at the repo root plus `docs/adr/`. See
`docs/agents/domain.md`.
