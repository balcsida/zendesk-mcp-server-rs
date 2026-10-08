# Agent skills

SKILL.md bundles in the open [Agent Skills](https://agentskills.io/specification) format. They teach AI coding agents to use Zendesk through the `zendesk` CLI and the `zendesk-mcp-server` tools.

- [zendesk](zendesk/SKILL.md): root skill with the operating rules, setup guard, command discovery and routing.
- [zendesk-tickets](zendesk-tickets/SKILL.md): find, read, reply to, update, merge and bulk-edit tickets.
- [zendesk-help-center](zendesk-help-center/SKILL.md): find, read, draft and publish Help Center articles, with their categories and sections.
- [zendesk-admin](zendesk-admin/SKILL.md): users, organizations, groups, views, macros, triggers, fields, custom objects, SLAs.

## Install

```bash
zendesk skills install
zendesk-mcp-server skills install
```

Without a binary:

```bash
npx skills add balcsida/zendesk-rs
gh skill install balcsida/zendesk-rs
```

Both binaries embed these files at build time. See the main README for the options.
