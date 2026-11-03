export interface SetupInfo {
  /** One short sentence. */
  summary: string;
  /** Config line to add, shown in a copyable block. */
  snippet?: string;
  /** Where the snippet goes, e.g. `agent.toml`. */
  file?: string;
}

/** Setup instructions shared across pages. */
export const SETUP = {
  history: {
    summary: 'Enable metrics history on the agent.',
    file: 'agent.toml',
    snippet: '[history]\nenabled = true',
  },
  events: {
    summary: 'Update the agent to 0.5 or later, and give it a writable state path. The event log is on by default.',
    file: 'agent.toml',
    snippet: '[events]\nenabled = true\n\n[state]\npath = "/var/lib/helion-agent/state.db"',
  },
  offsite: {
    summary:
      'Copy the repository off this machine after the nightly backup (restic copy to another node or a cloud bucket), then report it to the status script.',
    file: 'backup job environment',
    snippet: 'HELION_OFFSITE_OK=1   # or 0, with HELION_OFFSITE_MSG\nHELION_OFFSITE_LABEL=pluto',
  },
  backups: {
    summary: 'Enable backups on the agent, then call helion-backup-status.sh at the end of your backup job.',
    file: 'agent.toml',
    snippet: '[backups]\nenabled = true\nstatus_file = "/host/backups/restic-status.json"',
  },
  logs: {
    summary: 'Enable log streaming on the agent.',
    file: 'agent.toml',
    snippet: '[docker]\nallow_logs = true',
  },
  actions: {
    summary: 'Allow container and volume actions on the agent.',
    file: 'agent.toml',
    snippet: '# above the first [section]\nallow_actions = true',
  },
  tailnet: {
    summary: 'Share the host tailscaled socket with the agent, then enable it. The agent gets read-only access.',
    file: 'agent.toml',
    snippet: '[tailscale]\nenabled = true\nsocket = "/host/tailscale/tailscaled.sock"',
  },
  uptime: {
    summary:
      'Update the agent to 0.6 or later. Checks are on by default; if the proxy on the node serves names that resolve somewhere the node cannot reach, list them here.',
    file: 'agent.toml',
    snippet: '[uptime]\nenabled = true\nlocal_domains = ["example.net"]',
  },
  updates: {
    summary: 'Update the agent to 0.8 or later. Checking registries for newer tags is on by default.',
    file: 'agent.toml',
    snippet: '[updates]\nenabled = true',
  },
  updatesApply: {
    summary:
      'Set the repository whose update workflow bumps the tag and deploys, and give the agent a fine-grained GitHub token for it (Actions: read and write) as HELION_AGENT_GITHUB_TOKEN.',
    file: 'agent.toml',
    snippet: '[updates]\nrepo = "owner/infrastructure"\nworkflow = "update.yml"',
  },
  wol: {
    summary: 'Turn on Wake-on-LAN on the agent of a node on the same LAN. Waking also needs allow_actions.',
    file: 'agent.toml',
    snippet: '[wol]\nenabled = true',
  },
  wolWindows: {
    summary:
      'For a Windows PC: enable Wake-on-LAN in the BIOS, turn on Wake on Magic Packet in the network adapter settings, and turn off Fast Startup.',
  },
  services: {
    summary: 'Group containers into a service with a Docker label.',
    file: 'docker-compose.yml',
    snippet: 'labels:\n  helion.service: gitea\n  helion.service.url: gitea.example.com',
  },
} satisfies Record<string, SetupInfo>;
