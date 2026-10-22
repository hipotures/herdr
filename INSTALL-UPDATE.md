# Install and update the patched Herdr build

This fork builds a custom Linux x86-64 binary. Use the archive built from this
checkout to keep its patches. `herdr update` and `herdr update --handoff`
download official releases and can replace the patched binary. Changing the
update channel can also install an official binary.

## Build the installation archive

After synchronizing the source and preserving the fork patches, run:

```bash
just build
just package-focus
```

The output is `dist/herdr-focus-linux-x86_64.tar.gz`, with a `.sha256` file
alongside it. `BUILD_INFO.json` inside the archive records the source revision,
binary checksum, architecture, and required glibc version. Check that glibc
requirement before moving a locally built archive to another Linux machine.

## First installation

Copy the archive and its checksum file to the target machine, then run:

```bash
sha256sum --check herdr-focus-linux-x86_64.tar.gz.sha256
tar -xzf herdr-focus-linux-x86_64.tar.gz
bash herdr-focus/install.sh
~/.local/bin/herdr --version
~/.local/bin/herdr
```

The installer verifies the packaged binary and atomically replaces
`~/.local/bin/herdr`. It does not download another binary. Ensure
`~/.local/bin` is on `PATH` when using the shorter `herdr` command.

## Update a running server without stopping pane processes

### Check service supervision first

If systemd or another supervisor runs Herdr, verify its process-lifetime policy
before requesting live handoff. A successful Herdr handoff does not guarantee
that the supervisor will keep the replacement process alive.

For a user service named `herdr.service`, inspect:

```bash
systemctl --user show herdr.service \
  -p Type -p ExitType -p KillMode -p ExecStart -p ExecStartPre
```

Use `systemctl` without `--user` for a system service. With `ExitType=main`
and `KillMode=control-group`, systemd considers the service stopped when the
old main process exits and terminates remaining processes in its cgroup,
including the replacement server and panes. This occurred on `intelitex`:
handoff imported three panes successfully, then the replacement received
`SIGTERM` immediately after the original server exited.

Do not run either handoff procedure below under that configuration. Configure
and verify supervision that supports the replacement process first.
`ExitType=cgroup` (systemd 250 or newer) keeps a service running while processes
remain in its cgroup; validate the intended service configuration in a disposable
test before using it with live panes. Do not restart a live service to apply
changes: restarting it terminates pane processes.

Also inspect `ExecStartPre`: a command such as
`ExecStartPre=-%h/.local/bin/herdr update` can overwrite the patched binary
whenever the service starts. Remove that automatic updater from the service
used for this fork, while preserving any other required preparation commands.

For a unit whose only `ExecStartPre` command runs `herdr update`, the following
drop-in at `~/.config/systemd/user/herdr.service.d/90-patched-herdr-handoff.conf`
keeps the service alive across main-process replacement and clears that updater:

```ini
[Service]
ExitType=cgroup
ExecStartPre=
```

After saving the drop-in, reload the unit definitions and verify the effective
settings with the `systemctl --user show` command above:

```bash
systemctl --user daemon-reload
```

This reload does not restart the server. It also does not move an existing
unmanaged server into the service; leave its live sessions running and let the
corrected service handle the next normal startup when no other server is active.
Update the service template in your provisioning tool as well, so future
deployments retain these settings.

See the systemd documentation for
[service lifetime](https://www.freedesktop.org/software/systemd/man/261/systemd.service.html#ExitType=)
and [process termination](https://www.freedesktop.org/software/systemd/man/261/systemd.kill.html).

### Replace the binary and hand off the selected session

Client and server use the same executable file, but replacing that file leaves
the existing server process running its previous code. `herdr --version`
reports the installed binary's version; `herdr status server` inspects the
running server. Replacing a build with another build of the same version can
still require a handoff to activate changed server code.

For a running default session, extract the new archive and run:

```bash
bash herdr-focus/install.sh --handoff
~/.local/bin/herdr status server
```

For a named session, select it explicitly:

```bash
bash herdr-focus/install.sh --handoff --session work
~/.local/bin/herdr --session work status server
```

The installer first replaces the binary, then requests experimental live
handoff with the installed file explicitly selected as `--import-exe`. This
transfers live panes and their processes to a new local server when supported
and successful. It handles one session per invocation; repeat for other running
sessions that need the new server code. Start a new client to use updated
client code as well.

If handoff fails, inspect the reported error and server log. The new binary
remains installed; the installer never retries by stopping the server. Do not
use `server stop` or `session stop` as an update fallback: those commands end
pane processes.

## The new binary is already installed, but the server is still old

Check the service-supervision requirements above before running these commands.

Use the installed patched binary directly:

```bash
~/.local/bin/herdr server live-handoff --import-exe "$HOME/.local/bin/herdr"
~/.local/bin/herdr status server
```

For a named session:

```bash
~/.local/bin/herdr --session work server live-handoff --import-exe "$HOME/.local/bin/herdr"
~/.local/bin/herdr --session work status server
```

These commands use the local executable and do not run the release updater or
download an official binary. A local handoff affects the selected local session;
it does not upgrade saved SSH machines.

### Why `(deleted)` can appear

After an atomic replacement, the old server can still run from the removed
executable inode. On Linux, its executable path can then appear as:

```text
/home/user/.local/bin/herdr (deleted)
```

Without `--import-exe`, `server live-handoff` defaults to the running server's
own executable path. Trying to start that path can fail with
`failed to spawn handoff import server ... (deleted): No such file or directory`.
Pass the absolute path to the newly installed binary with `--import-exe`, as
shown above. This is why the archive installer always supplies that argument.

`herdr --handoff` alone is not a valid invocation.
`herdr server handoff --binary ...` is also not a supported command. Use
`server live-handoff --import-exe ...` for an already installed custom binary.
