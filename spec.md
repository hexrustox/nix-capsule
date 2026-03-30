# nix-capsule Specification

## Overview

`nix-capsule` provides explicit host commands that execute inside a long-lived Podman container, within a real Nix devshell already entered inside that container.

Example usage:

```sh
ncap cargo build
ncap nil
ncap bash
```

Goals:

- containerized execution of dev tools
- correct Nix devshell environment fidelity
- low per-command overhead
- host/editor access to tools available only inside the container

## Architecture

### Components

- **`ncap`**: host CLI client; sends execution requests over a Unix socket
- **`ncap-server`**: long-lived server inside the container; started from within a chosen Nix devshell; spawns requested commands as child processes
- **Nix library**: exposes `mkShell` and provides `ncap` plus lifecycle scripts in the host-facing shell

### Runtime

`start-container` must run:

```sh
podman ${finalOpts} ${image} -- nix develop .#${devShell} -- ncap-server --socket ${socketPath}
```

Where:

- `image`, `devShell`, and `socketPath` are configured through `lib.mkShell`
- `finalOpts = defaultOpts - removeOpts + opts`

After startup:

- `ncap-server` remains alive inside the realized devshell
- `ncap <cmd> ...` connects to `${socketPath}`
- the server executes `<cmd>` inside that devshell and bridges stdio back to the client

## Configuration

The library exposes:

```nix
lib.mkShell {
  image = ...;
  devShell = "...";
  socketPath = "...";
  opts = [ ... ];
  removeOpts = [ ... ];
}
```

Meaning:

- `image`: container image reference or equivalent
- `devShell`: name of the user-defined devshell to run inside the container
- `socketPath`: Unix socket path shared by client and server
- `opts`: Podman arguments appended to the defaults
- `removeOpts`: Podman arguments removed from the defaults before applying `opts`

The user project may define any devshell name; the implementation must not assume a fixed name such as `container`.

Example:

```nix
devShells = {
  default = lib.mkShell {
    devShell = "toolchain";
  };

  toolchain = pkgs.mkShell {
    packages = [ pkgs.gcc ];
  };
};
```

`lib.mkShell` produces the host-facing shell and includes:

- `ncap`
- `start-container`
- `stop-container`
- `restart-container`
- optional `enter-container`

The actual development environment is the selected user devshell.

## Requirements

### Container lifecycle

The system must provide `start-container`, `stop-container`, and `restart-container`.

The library must define a set of `defaultOpts` containing the minimum Podman configuration required to function.

These defaults must include bind mounts needed for:

- project files
- `/nix`
- `socketPath`
- any minimal runtime paths needed for `nix develop`

The bind mount for `socketPath` must be included in `defaultOpts`.

Effective Podman arguments are computed as:

```text
finalOpts = defaultOpts - removeOpts + opts
```

This allows users to override or remove default behavior.

### Server behavior

`ncap-server` must:

- run inside `nix develop .#${devShell}`
- remain long-lived
- spawn each requested command as a child process
- never replace itself with the executed command
- support multiple concurrent client connections

Each child process must inherit the realized devshell environment.

### Client/server behavior

For each request, `ncap` sends:

- command
- args
- cwd
- optional env overrides
- optional execution flags

`ncap-server` must:

- execute the command in the devshell
- bridge stdin/stdout/stderr to the requesting client
- return the child exit status

The protocol is implementation-defined.

### Execution semantics

`ncap <command> [args...]` must preserve:

- arguments
- stdin
- stdout
- stderr
- exit code
- working directory

This must work for long-lived stdio processes such as LSP servers.

### Editor support

Editors must be able to launch language servers via:

```sh
ncap <lsp>
```

The resulting process must run inside the selected devshell in the container and have access to project files.

### Socket

Communication uses a Unix socket at `socketPath`.

The socket path is configured through `lib.mkShell`, and its bind mount into the container is part of `defaultOpts`.

The socket must be reachable by both:

- host-side `ncap`
- in-container `ncap-server`

It should be isolated per project or session as appropriate.

## Security model

`nix-capsule` is a convenience and isolation layer, not a full sandbox.

Default behavior should prioritize functionality. Stronger isolation is configured by the user through Podman options.

## Performance goals

The implementation should avoid per-command `nix develop` by reusing:

- one long-lived container
- one long-lived `ncap-server`
- mounted `/nix`

## Non-goals

Not required:

- direct execution as `cargo` instead of `ncap cargo`
- per-tool wrapper script generation
- shell alias/function/prompt replication
- perfect emulation of arbitrary interactive shell behavior
- host GUI/socket/agent integration unless explicitly configured
- formal sandbox guarantees
