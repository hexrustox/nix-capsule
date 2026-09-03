# nix-capsule — wire protocol

One Unix socket, one connection per command, byte-stream framing.

## Framing

Every frame is: 1 tag byte, a 4-byte big-endian payload length, then the payload.

- Struct frames (`Request`, `Exit`, `Error`, `Version`, `Signal`): payload is JSON.
- Stream frames (`Stdin`, `Stdout`, `Stderr`): payload is raw bytes (producer chunks ~8 KiB; framing is chunk-agnostic).
- Payload length is capped at **16 MiB**. A frame declaring more is a transport violation: the receiver treats the connection as failed (the server sends `Error` first when it can) and the client exits `1`.

## Frames

| Tag | Frame | Direction | Payload |
| --- | --- | --- | --- |
| `0x01` | `Request` | client → server | `{ "command": str, "args": [str], "cwd": str, "env": ["KEY=VALUE"], "version": str? }` |
| `0x02` | `Stdin` | client → server | raw bytes; an empty payload marks stdin EOF |
| `0x03` | `Stdout` | server → client | raw bytes |
| `0x04` | `Stderr` | server → client | raw bytes |
| `0x05` | `Exit` | server → client | `{ "code": u8?, "signal": u8? }` — exactly one set in practice |
| `0x06` | `Error` | server → client | `{ "message": str }` |
| `0x07` | `ServerStopping` | server → client | empty |
| `0x08` | `Version` | server → client | `{ "version": str }` — tag pinned for backward compatibility |
| `0x09` | `Signal` | client → server | `{ "signal": u8 }` (POSIX signal number, e.g. 2, 9, 15) |

## Connection lifecycle

1. Client connects, sends `Request` (optionally carrying its version).
2. Server replies `Version` with its own.
3. Both directions then stream: `Stdin` from the client; `Stdout`/`Stderr` from the server.
4. The server sends exactly one terminal frame — `Exit` on child completion (exec-failure codes included), `Error` on any other failure — and nothing after it.
5. Either side may then close.

## Guarantees and conventions

- **Version is advisory.** Client and server ship from the same package and are always in lockstep; a mismatch (or a missing version) is a warning on the client's stderr / server's log, never a rejection. Comparison is exact string equality.
- **Ordering:** per-stream FIFO is guaranteed; interleaving between stdout and stderr is not (independent forwarding tasks).
- **EOF:** there is no EOF frame type. stdin EOF travels as one empty `Stdin` frame — a client write-half close means the same to the server; either way the server drops the child's stdin pipe, giving the child EOF, and the connection stays open so a later `Signal` frame still flows.
- **Disconnect before the terminal frame:** a failed send is the server's only disconnect notice — on it the child's process group is TERMed and reaped (see `server.md`). A read-side close means stdin EOF, never a disconnect; a silent child of a vanished client runs to completion (accepted limitation).
- **Signals:** the client never sends bytes that behave like terminal signals; host Ctrl-C arrives at the client's own signal handler and travels as a `Signal` frame (see `client.md`).
- **Exec failures:** spawning the child failing with `ENOENT` ⇒ terminal `Exit { "code": 127 }`, `EACCES` ⇒ terminal `Exit { "code": 126 }` — no `Error` frame; the client synthesizes the stderr line. Any other spawn failure ⇒ terminal `Error { message }`.
- **Encoding:** `command`, `args`, `env`, `cwd` travel as UTF-8 JSON strings; non-UTF-8 host bytes convert lossily (`U+FFFD`). Accepted limitation.
