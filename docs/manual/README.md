# orch user manual

orch (version 0.9.3) coordinates AI coding agents in a project. You describe work as tasks and workers as agents.
orch matches them, runs each agent in its own workspace, keeps the logs, and moves each task through review to
done, with retries on failure. Everything lives in plain files in a `.orchestry/` folder in your project.

This manual is for developers and operators who run AI coding agents such as Claude Code or Codex and want one
tool to coordinate them. You should be comfortable in a terminal and with git. You do not need to know Rust.
Team leads who are deciding whether orch fits can read [Getting started](01-getting-started.md) and
[Concepts](02-concepts.md) only.

## How to use this manual

Find your goal in the table, then read the chapter. The table lists every chapter in order.

| I want to | Read | What is in it |
|---|---|---|
| Try orch for the first time, or install it | [1. Getting started](01-getting-started.md) | What orch does, requirements, install and upgrade, a quick start you can follow without an AI tool |
| Understand how tasks, agents and review fit together | [2. Concepts](02-concepts.md) | Projects, agents, tasks and their lifecycle, review, workspaces, teams, goals, messages, context, runs, autonomy, the orchestrator |
| Add an agent, or install one from the shop | [3. Agents](03-agents.md) | Add, list, edit, disable, enable and remove agents |
| Add, assign, approve, reject or retry a task | [4. Tasks](04-tasks.md) | Add, assign, edit, cancel, approve, reject and retry tasks |
| Group agents into a team, or set a goal | [5. Teams and goals](05-teams-and-goals.md) | Create and manage teams and goals |
| Message an agent, or share data between agents | [6. Messages and context](06-messages-and-context.md) | Send and read messages, and use the shared context |
| Run tasks, run unattended, read logs, check my setup | [7. Running and monitoring](07-running-and-monitoring.md) | `run`, `serve`, `status`, `logs`, `doctor` and `update` |
| Watch progress in a terminal | [8. Dashboard](08-dashboard.md) | Layout, views, keys, editing and the command bar |
| Watch progress in a browser | [9. Web dashboard](09-web-dashboard.md) | Start, sign in, reach it from another machine, security |
| Deploy a whole team from a template | [10. Templates and shop](10-templates-and-shop.md) | The agent shop and organization templates |
| Use agents on another machine | [11. Remote agents](11-remote-agents.md) | Controllers, workers, approval and environment |
| Change settings, or script orch with JSON or YAML | [12. Configuration and output](12-configuration-and-output.md) | Project and global settings, output flags, `update` and `patch` from files |
| Fix a problem | [13. Troubleshooting](13-troubleshooting.md) | Symptoms, causes and fixes |
| Look up a command | [14. Command reference](14-command-reference.md) | Every command in one list |
| Look up a word | [15. Glossary](15-glossary.md) | The terms this manual uses |

Each chapter starts with what you can do after reading it. Commands are in `sh` blocks. The output below
them was captured from orch 0.9.3, so your IDs and times differ.

For how this manual was written and how to keep it accurate, see [About this manual](about-this-manual.md).

## Other documents

- [Runbook](../RUNBOOK.md): recovery procedures for operators.
- [Updating definitions from files](../file-mutations.md): the full rules for `update` and `patch`.
- [Organization templates](../org-templates.md): the template format and the built-in a-team.
