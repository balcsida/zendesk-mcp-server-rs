---
name: zendesk-tickets
description: >
  Find, read and change Zendesk tickets: look up by ID, search with ZQL, count, read comments,
  audits and SLA metrics, reply publicly or privately, change status, tags, assignee and custom
  fields, bulk update, merge, attach files, link problems and incidents, restore deleted or
  suspended tickets. Use for any request about a ticket, requester conversation or support queue.
---

# Zendesk Tickets

Use with the root skill [zendesk](../zendesk/SKILL.md) for surface choice, auth guard, command
discovery and the write rules. This skill adds ticket semantics.

## Use When

- The user names a ticket ID, requester, assignee, tag or status.
- A queue needs triage, counting, summarizing or a status sweep.
- A reply, internal note, tag, assignee or field change is needed.
- Tickets need merging, bulk updating, linking, restoring or recovering.

## First Route

| Intent | MCP tool | CLI |
| --- | --- | --- |
| One ticket | `get_ticket` | `zendesk tickets show-ticket <ID>` |
| Several tickets by ID | `get_tickets_bulk` | `zendesk tickets tickets-show-many --ids <ID>,<ID>` |
| Latest tickets | `get_tickets` | `zendesk tickets list-tickets` |
| Search with ZQL | `search`, `search_all_tickets` | `zendesk search list-search-results --query '<ZQL>'` |
| Count before fetching | `count_tickets` | `zendesk search count-search-results --query '<ZQL>'` |
| Comments | `get_ticket_comments` | `zendesk ticket-comments list-ticket-comments <ID>` |
| Audit trail | `get_ticket_audits` | `zendesk ticket-audits list-audits-for-ticket <ID>` |
| Metrics and SLA | `get_ticket_metrics`, `get_sla_breaches` (admin only) | `zendesk ticket-metrics show-ticket-metrics-by-ticket <ID>` |
| By user or organization | `get_user_tickets`, `get_organization_tickets` | `zendesk tickets list-user-requested-tickets <ID>` |
| Create | `create_ticket` | `zendesk tickets create-ticket -d @ticket.json` |
| Update | `update_ticket` | `zendesk tickets update-ticket <ID> -d @update.json` |
| Add or remove tags | `update_ticket_tags` | `zendesk tickets update-ticket <ID> -d '{"ticket":{"additional_tags":["<TAG>"]}}'` |
| Reply or note | `create_ticket_comment` | `zendesk tickets update-ticket <ID> -d @comment.json` |
| Bulk update | `update_tickets_bulk`, `get_job_status` | `zendesk tickets tickets-update-many --ids <ID>,<ID> -d @update.json` |
| Merge | `merge_tickets`, `get_job_status` | `zendesk tickets merge-tickets-into-target-ticket <ID> -d @merge.json` |
| Upload a file | `upload_attachment` | none; the CLI sends JSON bodies only |
| Problems and incidents | `search_problem_tickets`, `get_linked_incidents` | `zendesk tickets list-ticket-incidents <ID>` |
| Deleted tickets | `list_deleted_tickets`, `restore_deleted_ticket` | `zendesk tickets list-deleted-tickets` |
| Suspended tickets | `list_suspended_tickets`, `recover_suspended_ticket` | `zendesk suspended-tickets list-suspended-tickets` |
| Followers and CCs | `get_ticket_collaborators` | `zendesk tickets list-ticket-followers <ID>` |

If a CLI route's flags are unclear, read `zendesk <group> <operation> --help` first.

## Finding tickets

ZQL examples: `type:ticket status:open priority:urgent`, `type:ticket assignee:me`,
`type:ticket requester:<EMAIL>`, `type:ticket created>2026-01-01`, `type:ticket tags:<TAG>`.
Always include `type:ticket`, or users and organizations match too. Count first. Past 1,000 matches,
narrow by date; `search_all_tickets` sets `truncated` when it hit the cap.

## Reading

- Comment authors are IDs. Resolve names with `get_users_bulk` (see [zendesk-admin](../zendesk-admin/SKILL.md)).
- `get_ticket_comments` gives `content_url` for `get_ticket_attachment` (images only).
- Audits explain why a ticket changed; a trigger or macro in an audit can be read with `get_trigger` or `get_macro`.
- `status` is a category; `custom_status_id` maps to labels via `list_custom_statuses`.

## Safe reads and writes

- Comments are public by default and email the requester. Set `public` to false for internal notes unless a public reply was asked for.
- Status flow: new, open, pending, hold, solved, closed. Closed tickets are immutable; create a follow-up with `create_ticket` and `via_followup_source_id` instead.
- `update_ticket` `tags` replaces the whole list. Use `update_ticket_tags` (`add`, `remove`) to change single tags; on the CLI use `additional_tags` or `remove_tags`.
- Custom fields are set by field ID: read `list_ticket_fields` (`zendesk ticket-fields list-ticket-fields`) and send `{"id": <FIELD_ID>, "value": ...}`.
- Concurrent edits: read the ticket, then `update_ticket` with `safe_update` true and `updated_stamp` set to its `updated_at`; a 409 means it changed, so re-read.
- Bulk updates take up to 100 tickets and run as a job. If the result is still pending, poll `get_job_status` (`zendesk job-statuses show-job-status <JOB_ID>`) and report failures.
- Uploads return a token valid for 60 minutes; pass it in the token list when you create the comment or ticket.
- Link an incident to a problem with `problem_id` on `update_ticket`.
- Irreversible: `merge_tickets`, `mark_ticket_as_spam` (also suspends the requester), `make_comment_private`, `redact_comment_text`. `delete_ticket` is soft for 30 days. Confirm before each, unless execution was explicitly requested.
- After any write, read the ticket again and report what changed.

## Handoffs

- Resolve people, groups, macros, views or ticket fields: [zendesk-admin](../zendesk-admin/SKILL.md).
- Link a ticket to an article or draft a knowledge-base fix: [zendesk-help-center](../zendesk-help-center/SKILL.md).
- MCP prompts `analyze-ticket` and `draft-ticket-response` produce an analysis or a reply draft; the draft is text for review, not a sent comment.
