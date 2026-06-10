# Container Lifecycle (`ncap-ctl`)

See [`main.md`](main.md) for the wire protocol reference.

All subcommands read configuration from env vars set by `lib.mkShell`.

## CLI

```
ncap-ctl <SUBCOMMAND>
```

| Subcommand | Description |
|------------|-------------|
| `init` | Evaluate devshell, cache it, start/restart the container |
| `start` | Start the container from a cached devshell |
| `stop` | Stop the running container |
| `restart` | Stop and restart the container |
| `enter` | Interactive shell inside the container |
| `show-options` | Print expanded runtime arguments |
| `clean` | Remove all cached dev environments and nix profiles |
| `status` | Show container status (running/stopped, id, runtime, socket) |
| `log` | Print the latest server log |
| `completions <SHELL>` | Generate shell completions |

### `init`

1. Determine project root via `git rev-parse --show-toplevel` (fallback: `pwd`).
2. Create cache directory if needed.
3. If cache is invalid (`NCAP_CACHE != 1` or env cache file missing):
   - Run `nix print-dev-env --profile <profile> <devshell>`, cache stdout to `.ncap-cache/<devshell>/env`.
   - Run `nix profile wipe-history --profile <profile>`.
   - `restart` the container.
4. If valid: `start` the container.

### `start`

1. Check if container is running via `<runtime> inspect`. If so, do nothing.
2. Verify the env cache file exists.
3. Warn if socket directory is non-empty; create it otherwise.
4. Run the container: `<runtime> run -d [options] -- <image> <bash> -c "source <env> && exec <server_bin> --socket <socket> --log-dir <log_dir> --timeout <timeout>"`.

### `stop`

Run `<runtime> stop <container>`. The server (PID 1) receives SIGTERM and triggers graceful shutdown.

### `restart`

`stop` (failures non-fatal) followed by `start`.

### `enter`

1. Verify the env cache file exists.
2. Run `<runtime> exec -it <container> <bash> -c "source <env>; exec <bash>"`.
3. Forward the interactive shell's exit code.

### `show-options`

Print each expanded runtime argument on its own line.

### `clean`

1. `stop` the container (non-fatal).
2. Remove the `.ncap-cache/` directory.

### `status`

If running: print container id, name, runtime, and socket path.
If stopped: print "not running" with a hint (`ncap-ctl start` or `ncap-ctl init`).

### `log`

1. List `ncap-server-*.log` files in the log directory.
2. Pick the one with the largest numeric timestamp (most recent).
3. If `$PAGER` is set, pipe through it (args are split on whitespace). Otherwise probe for `less -R`. Fall back to printing to stdout.

### `completions`

Generate shell completions for bash, elvish, fish, powershell, or zsh.
