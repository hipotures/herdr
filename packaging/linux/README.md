# Herdr Linux package

This archive installs the Herdr binary built for Linux x86-64. The archive
contains `BUILD_INFO.json`, which records the package builder's repository
revision, the binary SHA-256, and the highest glibc symbol version required by
the binary. The recorded repository revision describes the source tree used by
the packager; it does not claim that an arbitrary `--binary` was built from
that tree.

Extract the archive and run:

```bash
bash herdr-focus/install.sh
```

The installer verifies the binary checksum and runs `herdr --version` before
atomically replacing `~/.local/bin/herdr`. Use `--bin-dir PATH` for another
installation directory. Existing Herdr processes keep running because the
replacement is an atomic rename.

For this patched build, use this archive instead of `herdr update` or
`herdr update --handoff`, which download official binaries and can overwrite
the patches. Replacing the file alone does not update an already running server.

Live handoff is explicit:

Before using it with a supervised server, check the supervisor configuration.
A systemd service with `ExitType=main` and `KillMode=control-group` terminates
the replacement server when the old main process exits, even after Herdr reports
a successful handoff. Configure and test suitable supervision before updating
live sessions; do not restart the live service to apply changes. Remove any
automatic `herdr update` command from its `ExecStartPre` to preserve this fork's
binary on subsequent service starts.

```bash
bash herdr-focus/install.sh --handoff
bash herdr-focus/install.sh --handoff --session SESSION_NAME
```

The optional, experimental handoff transfers live panes to the newly installed
server. The installer never runs `server stop`; if handoff fails, it reports
the failure and does not retry through another command. Select each running
session that needs the new server code separately. Start a new client to use
updated client code as well.

If the new binary is already installed, hand off the default session directly:

```bash
~/.local/bin/herdr server live-handoff --import-exe "$HOME/.local/bin/herdr"
~/.local/bin/herdr status server
```

For another session, add `--session SESSION_NAME` before `server` and `status`.
This uses your local binary without downloading an official release. Always
specify `--import-exe`: after atomic replacement, the old server's executable
path may end in `(deleted)` and cannot be started again by that path.
`herdr --version` reports the installed binary; `herdr status server` inspects
the running server. Do not use `server stop` or `session stop` as an update
fallback, because they end pane processes.

Build this package from the repository with:

```bash
python3 scripts/package_focus.py
```

Use `--binary PATH` to package an existing binary and `--output-dir PATH` to
choose the output directory. The resulting archive is always named
`herdr-focus-linux-x86_64.tar.gz` and extracts into `herdr-focus/`.
