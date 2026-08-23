---
name: writing-error-messages
description: "Write one-voice error messages — the message states what is wrong, one sentence, tokens backticked, advice carried as its own piece outside the message text. Use when writing, editing, reviewing, or context-wrapping (chaining) error text in any language: messages, exception strings, diagnostics, CLI errors."
---

# Error messages

Every error message in a codebase speaks one voice. These rules govern *how*
the message reads — what errors exist and when they fire comes from the code's
specs. Run them over every message you write or touch.

## Rules

1. **One lowercase fragment, no period.**
   The message is one sentence fragment, starting lowercase, ending without a
   period.

   - good: `` missing `name` field ``
   - bad: `The name field was not provided.`

2. **Backtick the tokens.**
   Quote what the message names — keywords, flags, option names, keys,
   filenames, mode names, format names; a `{field}` placeholder backticks too
   when it names a token. Leave raw data unquoted: numbers, counts.

   - good: `` missing `name` field ``
   - good: `` unknown option `--recursive` ``
   - bad: `` list index `3` out of range `` — `3` is data, not a token

3. **One sentence states the subject and the facts.**
   Name the subject the error is about and the facts needed to act, in one
   sentence. To add context, chain it as a context message hanging off the
   failing operation — never a colon-joined clause:

   ```text
   you write:      context   "cannot write `out.yaml`"
   library adds:   "destination directory does not exist"   (the underlying cause)
   rendered:       cannot write `out.yaml`: destination directory does not exist
   ```

   - bad: `cannot write `out.yaml`: destination directory does not exist`

4. **The message states what is wrong; advice lives outside it.**
   The message describes the error, not the fix. Prescriptive advice is
   carried on the diagnostic structure the error is rendered through — a
   positioned note marking the offending location, a separate advice message,
   or a field on the error type — all optional, never concatenated into the
   message.

   - message: `` missing `name` field ``
   - note: `add `name` where the record is built`

5. **Positioned notes for located errors, plain advice for the rest.**
   When the error has a real source location (parse/render errors, config
   positions), the diagnostic can point at it — an underline or marker on the
   span, rendered as its own piece. When there is no location (process- or
   CLI-level errors) or for general guidance, the advice rides as an
   unpositioned note. How each renders is the library's job; the message text
   stays clear of both.

6. **Rendering.**
   Errors go to stderr; one top-level handler at the program's entry point
   renders the report once (errors return up the stack; nothing prints
   mid-task), and color/unicode decorations turn off when stderr is not a
   terminal.

7. **Internal errors.**
   The message still states only what is wrong. If the error may be an
   internal bug rather than a user mistake, say so on the separate advice
   piece (the unpositioned note), never in the message text.

   - note: `this is likely an internal error`

## Good vs bad

| good | bad |
| --- | --- |
| `` cannot open `conf.yaml` `` | `The configuration file could not be opened.` |
| `` missing `name` field `` | `The name field was not provided.` |
| `` destination directory does not exist `` + chained context `` cannot write `out.yaml` `` | `cannot write `out.yaml`: destination directory does not exist` |
| message: `list index 3 out of range` (data unquoted) | `error: you must use a valid index` (prescribes, states no facts) |
