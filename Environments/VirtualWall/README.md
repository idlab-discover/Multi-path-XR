# Virtual Wall Environment

This crate implements the repository with the SLICES Virtual Wall infrastructure. It automates the lifecycle of bare-metal resources through the SLICES CLI, keeps local state so the controller can recover after restarts, and exposes a CLI wrapper (`vw`) for ad-hoc usage.

## Prerequisites

1. Install the [SLICES CLI](https://doc.slices-ri.eu/SupportingServices/slicescli.html) and verify you can authenticate (`slices auth login`) and select a project (`slices project use <project>`).
2. Make sure the CLI can access a project that grants Basic Infrastructure (BI) permissions for bare-metal nodes (e.g. `be-gent1-bi-baremetal1`).
3. Optionally export helper variables so the automation can re-use them:

```bash
export SLICES_PROJECT=my-project
export SLICES_EXPERIMENT=my-experiment
export SLICES_BI_SITE_ID=be-gent1-bi-baremetal1
export SLICES_BI_INFRA_ID=be-gent1-bi-baremetal1 # preferred name; falls back to SLICES_BI_SITE_ID
export VIRTUAL_WALL_SSH_USERNAME=root
export VIRTUAL_WALL_SSH_KEY=$HOME/.ssh/id_ed25519
```

## Configuration

`virtual_wall.toml` contains sane defaults and can be duplicated per environment. The loader honours (in order):

1. `--config <path>` passed to the CLI
2. `VIRTUAL_WALL_CONFIG` environment variable
3. `virtual_wall.private.toml`, `virtual_wall.toml` or `virtualwall.toml` in the current directory or `Environments/VirtualWall`
4. Reasonable fallbacks (searching `PATH` for `slices`, storing state in XDG config dir)

Relevant keys:

- `site_id`, `image`, `flavor`: default request settings for bare-metal nodes.
- `ssh_username`, `ssh_private_key`: used by the controller when executing commands through SSH.
- `resource_prefix`: prepended to generated friendly names.
- `cloud_init_template`: optional path to a cloud-init file ready to be passed to SLICES (`src/cloudinit/user-data.yaml.tmpl` is a simple example).
- `resource_spec_template`: use an existing JSON/YAML spec instead of the generated one.

## CLI usage

You can exercise the automation without running the controller via the helper script:

```bash
./Environments/VirtualWall/scripts/vw.sh start --nodes 3
./Environments/VirtualWall/scripts/vw.sh status
./Environments/VirtualWall/scripts/vw.sh exec node-1 -- ip addr
./Environments/VirtualWall/scripts/vw.sh ping-all
./Environments/VirtualWall/scripts/vw.sh stop
```

The script simply runs the `vw` binary defined in this crate (see `src/bin/vw.rs`). Pass `--config <file>` to override the configuration location.

## Controller integration

`Controller::VirtualWallHandler` loads the manager lazily on the first request, ensuring:

- Experiments are created (or re-used) automatically.
- Resource specifications are generated and persisted under `$STATE_DIR/specs`.
- Allocations are stored in a local state file (`state.json`) so a controller restart can resume operations or release nodes cleanly.
- All responses (`nodes`, `status`, `ping_all`, `exec`) are delegated to the shared manager so the CLI and controller stay in sync.

If the handler cannot initialise (missing CLI, misconfigured project, etc.), the controller returns an actionable error message with the root cause.

## Cloud-init and specs

`src/cloudinit/user-data.yaml.tmpl` illustrates how to bootstrap nodes (install packages, register a service, etc.). Reference it from `cloud_init_template` to automatically inject cloud-init user-data when requesting resources.

`resource_spec.rs` provides a helper to generate simple bare-metal clusters. For more advanced setups you can author a full resource specification (JSON/YAML) and point `resource_spec_template` to it.

## Notes on networking

The default generator only allocates nodes; it does not declare inter-node links yet. You can extend `ResourceSpecFactory` to emit the desired topology (see the SLICES documentation for link schemas) or supply a custom template.

## Troubleshooting

- All SLICES commands are executed through the CLI. Use `tracing` (enable `RUST_LOG=info`) to inspect the raw CLI output when debugging.
- Persistent state lives in the directory configured by `state_dir`. Delete the `state.json` file if you need to reset the automation manually.
- When running `cargo check` in the sandboxed environment network access might be blocked; install dependencies in an environment with access to `index.crates.io` if compilation fails for that reason.

