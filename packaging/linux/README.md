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

Live handoff is explicit:

```bash
bash herdr-focus/install.sh --handoff
bash herdr-focus/install.sh --handoff --session SESSION_NAME
```

The optional, experimental handoff transfers live panes to the newly installed
server. The installer never runs `server stop`; if handoff fails, it reports
the failure and does not retry through another command. Run the installer separately on
each remote Herdr machine whose agents should be reachable from the GNOME
Codex Status widget.

Build this package from the repository with:

```bash
python3 scripts/package_focus.py
```

Use `--binary PATH` to package an existing binary and `--output-dir PATH` to
choose the output directory. The resulting archive is always named
`herdr-focus-linux-x86_64.tar.gz` and extracts into `herdr-focus/`.
