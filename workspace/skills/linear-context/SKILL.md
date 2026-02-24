# Linear Context

Before every response that touches work state, query current Linear issues.

## Tool: check_linear

Query the Linear GraphQL API for:
- Active cycle issues (status, assignee, priority)
- Recently updated issues (last 24h)
- Issues matching keywords from the current message

## Rules

- Never create, update, or close a Linear issue without first checking current state.
- Before responding to any message about work, check if relevant issues exist.
- A real PM always has their project board open. You must do the same.
