# Issue tracker: Linear

New issues, specs and work plans live in [Windence](https://linear.app/ivoxygen/project/windence-447cd3088af5), in the Ivoxygen team (`IVO`) of the redstone workspace. The project identifier is `P-IVO-7`.

Use the Linear connector for tracker operations. GitHub remains the home of the repository and pull requests at `ForeverInLaw/windence`.

## New work and existing references

- Create new tasks in the Windence project. Do not create matching GitHub issues.
- Keep existing GitHub issue links as historical specs and evidence. Read them with `gh` when a task points to one.
- This change does not move or close old GitHub issues. A bulk migration is separate work.
- When a skill says "publish to the issue tracker", create a Linear issue. When it says "fetch the relevant ticket", read the referenced Linear issue or historical GitHub issue.

## Issue conventions

Write a clear problem, the agreed scope and checkable completion criteria. Include source commits or documents when useful. Read existing issues before creating a duplicate.

Use native parent and blocking relations. Set `project` on every new issue. Choose priorities from the work's impact. Set an assignee or due date only when agreed or when claiming work.

The `ready-for-agent` label means the scope is complete. Open blockers still prevent implementation. See [triage roles](triage-labels.md) for the status mapping.

## Pull requests as a triage surface

**PRs as a request surface: no.** Review GitHub pull requests with `gh`. Link them to their Linear tasks. Treat an external PR as a feature request only when explicitly asked.

## Wayfinding operations

- **Map:** one parent issue containing decisions, open questions and links to child tasks.
- **Child:** a separate issue with its native parent relation set to the map.
- **Dependency:** a native blocking relation. Treat an issue as blocked while any required blocker remains open.
- **Frontier:** the map's `Todo` children with `ready-for-agent`, no assignee and no open blockers. Take the highest priority first, then the map's stated order.
- **Claim:** assign the issue to the implementing user and move it to `In Progress`.
- **Review:** use `In Review` when the change is awaiting review or a required check.
- **Resolve:** record what changed, validation, relevant PR or commit, and remaining risks. Move to `Done` only when its completion criteria are met. Update the map with any new decision.

The upstream adaptation plan starts at [IVO-66](https://linear.app/ivoxygen/issue/IVO-66/perenesti-poleznye-izmeneniya-upstream-v-windows-port).
