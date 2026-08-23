# nix-capsule

Containerized dev shells with transparent binary execution.

[![License: GPLv3](https://img.shields.io/badge/license-GPLv3-blue.svg)](LICENSE)

**nix-capsule** runs your dev tools inside a Podman container with a Nix devshell — but `stdin`, `stdout`, `stderr`, exit codes, and working directory all pass through transparently. It feels native.

## Why nix-capsule?

| Approach | Issue |
|----------|-------|
| `docker exec` / `podman run --rm ... -- <cmd>` | Manual boilerplate for every invocation. Broken stdin for interactive tools. |
| `nix develop --command <cmd>` | Evaluates the devshell on every invocation — slow. No container isolation. |
| **devenv** / **Devbox** (container mode) | One-shot container — `devenv container run` or `devbox generate dockerfile` gives you an interactive shell inside the container. Host tools and editors can't transparently invoke commands across the boundary. |
| **nix-capsule** | Long-lived container + binary protocol over a Unix socket. `ncap <cmd>` from the host bridges stdin/stdout/stderr/exit codes. LSP servers work across the boundary without devcontainers. |

### Key features

- **Transparent I/O** — `stdin`, `stdout`, `stderr`, exit codes, and `$CWD` pass through unmodified. LSP servers just work.
- **Cached devshell** — `nix print-dev-env` runs once and caches. Subsequent sessions reuse the result.
- **direnv integration** — `ncap-direnv` compares file mtimes to skip re-eval when Nix inputs haven't changed.
- **Wrappers** — Auto-generate shell scripts so `cargo`, `rust-analyzer`, `nixd`, etc. automatically route through the container.
- **Graceful shutdown** — Server drains connections on `SIGTERM`, notifying clients before exit.
- **Hardening** — Optional `--cap-drop=all` and `--security-opt=no-new-privileges`.

## Quick start

The canonical source is on [GitLab](https://gitlab.com/codnixus/nix-capsule); the [GitHub repository](https://github.com/hexrustox/nix-capsule) is a mirror used for CI releases.

Add this to `flake.nix`:

```nix
{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-26.05";
    nix-capsule.url = "gitlab:codnixus/nix-capsule";
  };

  outputs = { self, nixpkgs, nix-capsule }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ nix-capsule.overlays.default ];
      };
      capsule = nix-capsule.lib { inherit pkgs; };
    in
    {
      devShells.${system}.container = pkgs.mkShellNoCC {
        packages = with pkgs; [ hello cowsay ];
      };

      devShells.${system}.default = capsule.mkShell {
        image = "alpine:latest";
        devShell = "container";
        socketPath = "/tmp/nix-capsule/ncap-socket";
        containerName = "ncap";
        wrappers = [ "hello" "cowsay" ];
      };
    };
}
```

```sh
nix develop            # enter the shell (starts container)
hello                  # runs inside the container
ncap hello             # same as `hello` — wrappers invoke `ncap hello`
cowsay "hi there"      # runs inside the container
ncap-ctl status        # check container status
```

## direnv integration

direnv integration is optional — plain `nix develop` works without it. To enable:

1. Add `apps.${system}.default = capsule.app;` to your flake outputs.
2. Create a `.envrc` with:
```sh
watch_file flake.nix flake.lock
eval "$(nix run .#)"
use flake .
```

This skips re-evaluation when Nix inputs haven't changed, making shells nearly instant.

## Binaries

The `ncap` package provides four binaries:

| Binary | Description |
|--------|-------------|
| `ncap` | Host CLI client — connects to the server and bridges I/O |
| `ncap-server` | Long-lived server inside the capsule container |
| `ncap-ctl` | Capsule container lifecycle management (`init`, `start`, `stop`, `restart`, `enter`, `status`, `log`, `clean`) |
| `ncap-direnv` | direnv integration for cache validation |

## Configuration

`lib.mkShell` accepts these options:

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `image` | string | *(required)* | Container image |
| `devShell` | string | *(required)* | Nix devshell attribute path |
| `socketPath` | string | *(required)* | Unix socket path |
| `containerName` | string | *(required)* | Container name |
| `runtime` | string | `podman` | Container runtime (`podman` or `docker`) |
| `harden` | bool | `false` | Enable `--cap-drop=all`, `no-new-privileges` |
| `autoStart` | bool | `true` | Run `ncap-ctl init` in `shellHook` |
| `timeout` | int | `10` | Server drain timeout (seconds) |
| `wrappers` | list | `[]` | Commands to wrap with `ncap` |
| `extraOptions` | list | `[]` | Additional container runtime options |
| `extraPackages` | list | `[]` | Additional packages in devShell |
| `logDir` | string | socket dir | Server log directory |
| `preShellHook` | string | `""` | Shell code before auto-start |
| `postShellHook` | string | `""` | Shell code after auto-start |

## Contributing

Contributions are welcome. See [`AGENTS.md`](AGENTS.md) for development setup, build commands, and test patterns. The [`spec/`](spec/) directory contains detailed design documents.

## License

[GPLv3](LICENSE)
