# nix-capsule

Run your Nix devshell in a container, transparently.

`nix-capsule` wraps your project's commands in a long-lived container backed by a reproducible Nix devshell. Type `ncap cargo build` on the host and it executes inside the container, bridging stdio, exit codes, and working directory over a Unix socket. LSPs, REPLs, and long-lived processes work the same way. A direnv integration caches the devshell evaluation so iteration stays fast.

## About

Reproducible dev environments are solved — Nix flakes handle that. But running those environments consistently across machines, isolating system dependencies, and avoiding the overhead of re-evaluating the devshell on every shell entry are not.

`nix-capsule` solves this by running a persistent container (Podman by default) with your Nix devshell pre-loaded. The host client (`ncap`) forwards commands to the container over a Unix socket. The container stays alive, so startup is instant after the first `init`. Unlike `nix develop`, you never re-evaluate the devshell unless your flake inputs change.

## Features

- **Transparent execution** — `ncap <cmd>` runs inside the container with full stdio bridging, exit code propagation, and working directory passthrough
- **Long-lived container** — the server stays alive as PID 1, accepting concurrent connections from multiple clients
- **direnv integration** — caches the devshell evaluation and skips re-evaluation when Nix inputs haven't changed
- **LSP and REPL support** — long-lived processes work because the connection stays open until the child exits
- **Hermetic environment** — the container inherits the devshell, not the host environment
- **Configurable isolation** — optional hardening flags (`--cap-drop=all`, `--security-opt=no-new-privileges`)

## Quick Start

Add `nix-capsule` to your `flake.nix`:

```nix
{
  inputs.nix-capsule.url = "github:hexrustox/nix-capsule";

  outputs = { nix-capsule, ... }: {
    devShells.x86_64-linux.default = nix-capsule.lib { inherit pkgs; }.mkShell {
      image = "alpine:latest";
      devShell = "container";
      socketPath = "/tmp/nix-capsule/ncap-socket";
      containerName = "my-project";
      wrappers = [ "cargo" "rust-analyzer" ];
    };
  };
}
```

Create a `.envrc`:

```bash
watch_file flake.nix flake.lock
eval "$(nix run .#)"
use flake .
```

Now commands run in the container:

```bash
ncap cargo build
ncap rust-analyzer
cargo build  # if "cargo" is in wrappers
```

## How It Works

```
Host                              Container
────                              ─────────
ncap ──[Request]──► socket ──► ncap-server
     ◄─[Stdout]─── socket ◄── child process
     ◄─[Exit]──── socket ◄── child process
```

1. `ncap-ctl init` evaluates the devshell with `nix print-dev-env` and caches the output
2. The container starts with the cached devshell sourced, running `ncap-server` as PID 1
3. Each `ncap <cmd>` connects to the Unix socket, sends a request, and bridges I/O until the child exits
4. The container stays alive for subsequent commands

## Configuration

`lib.mkShell` accepts these options:

| Option | Description | Default |
|--------|-------------|---------|
| `image` | Container image (required) | — |
| `devShell` | Nix devshell attribute path | — |
| `socketPath` | Unix socket path | — |
| `containerName` | Container name | — |
| `runtime` | Container runtime | `"podman"` |
| `logDir` | Server log directory | socket parent dir |
| `extraOptions` | Additional runtime options | `[]` |
| `extraPackages` | Additional packages in devShell | `[]` |
| `harden` | Enable hardening flags | `false` |
| `autoStart` | Run `ncap-ctl init` in shellHook | `true` |
| `timeout` | Server drain timeout (seconds) | `10` |
| `wrappers` | Commands to wrap with `ncap` | `[]` |
| `preShellHook` | Shell code before auto-start | `""` |
| `postShellHook` | Shell code after auto-start | `""` |

### Wrappers

Each entry in `wrappers` creates a script that runs `ncap <cmd> "$@"`:

```nix
wrappers = [
  "cargo"                    # string: creates `cargo` wrapper
  { name = "ra"; value = "rust-analyzer"; }  # attrset: custom name
];
```

## direnv Integration

`ncap-direnv` is the default flake app. When you run `use flake` in `.envrc`, direnv invokes `ncap-direnv`, which:

1. Reads `DIRENV_WATCHES` to get file modification times
2. Compares against stored mtimes in `.ncap-cache/direnv-mtimes.json`
3. Sets `NCAP_CACHE=1` if inputs haven't changed

When `NCAP_CACHE=1`, `ncap-ctl init` skips re-evaluating the devshell and just starts the container. This makes shell entry instant after the first evaluation.

See `spec/direnv.md` for details.
