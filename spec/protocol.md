# Wire protocol

The Socket is the sole channel between Client and Server: one Unix socket, one
Connection per command, byte-stream framing. This file is the single source of
truth for framing, frame types, and the guarantees both ends rely on;
spec/client.md § Request construction and spec/server.md § Connection handling describe each end's behavior on top of it.

## Framing

Every frame is: 1 tag byte, a 4-byte big-endian payload length, then the
payload.

- Struct frames (`Request`, `Exit`, `Error`, `Version`, `Signal`): payload is
  JSON.
- Stream frames (`Stdin`, `Stdout`, `Stderr`): payload is raw bytes. Producers
  chunk ~8 KiB; framing is chunk-agnostic — receivers make no assumption about
  chunk boundaries.
- Payload length is capped at **16 MiB**. A frame declaring more is a
  transport violation: the receiver treats the connection as failed (the
  Server sends `Error` first when it can) and the Client exits `1`. An unknown
  tag byte is the same transport violation.

## Frames

| Tag | Frame | Direction | Payload |
| --- | --- | --- | --- |
| `0x01` | `Request` | client → server | `{ "command": str, "args": [str], "cwd": str, "env": ["KEY=VALUE"], "version": str? }` |
| `0x02` | `Stdin` | client → server | raw bytes; an empty payload marks stdin EOF |
| `0x03` | `Stdout` | server → client | raw bytes |
| `0x04` | `Stderr` | server → client | raw bytes |
| `0x05` | `Exit` | server → client | `{ "code": u8?, "signal": u8? }` — exactly one set in practice; unset fields are omitted |
| `0x06` | `Error` | server → client | `{ "message": str }` |
| `0x07` | `ServerStopping` | server → client | empty |
| `0x08` | `Version` | server → client | `{ "version": str }` — tag pinned for backward compatibility |
| `0x09` | `Signal` | client → server | `{ "signal": u8 }` (POSIX signal number, e.g. 2, 9, 15) |

JSON conventions:

- Receivers tolerate unknown JSON fields and a `Request` without `version`.
- Receivers reject a `ServerStopping` frame with a non-empty payload and an
  `Exit` frame with both `code` and `signal` set. (`None`, `None` is the
  unknowable-status exception, spec/server.md § Connection handling.)

## Connection lifecycle

1. Client connects, sends `Request` (carrying its version).
2. Server replies `Version` with its own.
3. Both directions then stream: `Stdin` from the client; `Stdout`/`Stderr`
   from the server.
4. The Server sends exactly one terminal frame — `Exit` on Child completion
   (exec-failure codes included), `Error` on any other failure — and nothing
   after it. `ServerStopping` is not a terminal frame; it may precede one.
   Split semantics: `ServerStopping` is terminal for the Client — the Client
   bails immediately per spec/client.md § Exit codes — but non-terminal
   server-side, where the bridge keeps running so a Child that finishes
   inside the drain grace still delivers its terminal frame
   (spec/server.md § Shutdown).
5. Either side may then close.

## Guarantees

- **Version is advisory.** Client and Server ship from the same package and
  are always in lockstep; a mismatch (or a missing version) is a warning — on
  the Client's stderr and in the Server's log — never a rejection. Comparison
  is exact string equality. The Client compares the `Version` frame against
  its own and warns when no `Version` frame arrived before the terminal
  frame; the Server compares `Request.version` against its own and warns in
  its log.
- **Ordering:** per-stream FIFO is guaranteed; interleaving between `Stdout`
  and `Stderr` is not (independent forwarding).
- **EOF:** there is no EOF frame type. stdin EOF travels as one empty `Stdin`
  frame — a Client write-half close means the same to the Server; either way
  the Server drops the Child's stdin pipe, giving the Child EOF, and the
  Connection stays open so a later `Signal` frame still flows.
- **Disconnect before the terminal frame:** a failed send is the Server's
  only disconnect notice — on it the Child's process group is TERMed and
  reaped (spec/server.md § Disconnect before the terminal frame). A read-side
  close means stdin EOF, never a disconnect; a silent Child of a vanished
  Client runs to completion (accepted limitation).
- **ServerStopping:** terminal for the Client, non-terminal for the Server.
  The Client stops streaming and exits `143` on receipt (spec/client.md
  § Exit codes) and never processes a later terminal frame on that
  Connection; the Server keeps the bridge running through the drain grace
  so a finishing Child still delivers its terminal frame
  (spec/server.md § Shutdown).
- **Signals:** the Client never sends bytes that behave like terminal
  signals; host Ctrl-C arrives at the Client's own signal handler and travels
  as a `Signal` frame (spec/client.md § Signals).
- **Exec failures:** spawning the Child failing with `ENOENT` ⇒ terminal
  `Exit { "code": 127 }`, `EACCES` ⇒ terminal `Exit { "code": 126 }` — no
  `Error` frame; the Client synthesizes the stderr line — see
  spec/client.md § Exit codes. Any other spawn failure ⇒ terminal
  `Error { message }`.
- **Misdirected frames:** once a Connection is established, a frame arriving
  from the side that never sends it is ignored — never fatal. (The
  first-frame rule is stricter: spec/server.md § Connection handling.)
- **Path translation:** none — host paths are valid inside the container only
  because the project root is bind-mounted at the same absolute path
  (spec/ctl.md § Mounts); anything not mounted is invisible.
- **Encoding:** `command`, `args`, `env`, `cwd` travel as UTF-8 JSON strings;
  non-UTF-8 host bytes convert lossily (`U+FFFD`). Accepted limitation.
