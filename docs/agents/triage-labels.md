# Triage roles

The skills use five canonical roles. Windence maps them to the existing Linear statuses and the `ready-for-agent` label.

| Skill role | Linear mapping | Meaning |
| --- | --- | --- |
| `needs-triage` | `Backlog`, without `ready-for-agent` | The scope or priority needs review. |
| `needs-info` | `Backlog`, without `ready-for-agent`; record the missing answer in the description | Work needs information before it can be specified. |
| `ready-for-agent` | `Todo` and `ready-for-agent`; use `Backlog` while blocked or intentionally deferred | The scope and completion criteria are complete. An agent may start once blockers are closed. |
| `ready-for-human` | `Todo`, without `ready-for-agent`; describe the required human action | The next step requires a person. |
| `wontfix` | `Canceled`; record the reason | The task will not be implemented. |

Use `Bug`, `Improvement` or `Feature` to describe the kind of work. These labels do not determine readiness.

Read native blocking relations before claiming an issue. A readiness label does not override an open blocker or a deferred status.
