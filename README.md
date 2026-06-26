# nix-capsule

Containerized dev shells with transparent binary execution.

`nix-capsule` runs your project's commands inside a long-lived container with a fully reproducible Nix devshell — a drop-in for your existing workflow. Type `ncap cargo build` on the host and it transparently executes in the container, bridging stdio, exit codes, and working directory over a Unix socket; LSPs, REPLs, and long-lived processes work the same way. A direnv integration skips re-evaluating the devshell when Nix inputs haven't changed, so iteration stays fast. The result: one consistent, hermetic environment for every contributor, on every machine, without leaving the terminal.

Built with Rust, Tokio, Nix flakes, and Podman.
