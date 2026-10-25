# Ideas

Ideas for future exploration, not committed implementation plans.

## Proxmox: start an offline machine from Herdr

When a machine such as `mypowers` is powered off, starting it currently requires
opening the Proxmox web interface. A Herdr plugin could offer **Start via
Proxmox** from the machine's entry, then reconnect once the machine is ready.
The initial scope is starting machines; a stop action is unnecessary for this
workflow.

### Setup

- Create a dedicated Proxmox user and API token with privilege separation.
- Grant both the user and token a role containing `VM.Audit` and `VM.PowerMgmt`
  only on the intended guests, such as `/vms/100`.
- A plugin setup command should ask for the Proxmox HTTPS address, token ID,
  and secret, hiding secret input. Proxmox displays the secret only once.
- Store credentials on the machine running the plugin, under
  `HERDR_PLUGIN_CONFIG_DIR`, with owner-only file access (`0600` on Unix).
  Never store real credentials in the plugin source or this repository.
- Map each Herdr machine to a Proxmox connection, node, guest type (VM or LXC),
  and VMID in the plugin's configuration.

`VM.PowerMgmt` also permits stopping guests. A start-only plugin interface does
not restrict the token itself to starting them.

### Interaction

1. The user selects **Start via Proxmox** for a configured machine.
2. The plugin checks the guest's state through the Proxmox API. Herdr's
   `offline` connection status alone does not establish that a guest is stopped.
3. If stopped, the plugin requests startup and reports progress or failure.
4. After startup, Herdr retries the connection, with a bounded wait for the
   guest and its connection service to become available.

The plugin must run on a reachable host, normally the local computer, because
the target guest cannot execute it while powered off. Starting a guest assumes
the Proxmox host itself is running and reachable.

### Herdr extension needed

Current plugin action contexts cover global, workspace, tab, pane, and selection
actions. They do not expose a machine context or a machine menu extension.

- An initial plugin could use a global action or keybinding to select and start
  a configured guest.
- The intended experience needs a generic machine action extension that works
  for offline targets and supplies the selected machine's identity.
- Keep Proxmox API calls, credentials, and guest mappings in the plugin. Herdr
  should own the generic action surface and connection retry behavior.

### References

- [Herdr plugin documentation](docs/next/website/src/content/docs/plugins.mdx)
- [Proxmox users, API tokens, and permissions](https://github.com/proxmox/pve-docs/blob/master/pveum.adoc)
- [Proxmox API reference](https://pve.proxmox.com/pve-docs/api-viewer/)
