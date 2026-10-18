# File-mutation examples

Inputs for `orch agent|task|config update|patch -f FILE` ([guide](../../docs/file-mutations.md)). They contain no
ids, paths or hosts. Run them in a disposable project:

```bash
EX=/path/to/r-orch/examples/file-mutations          # this directory
mkdir demo && cd demo && orch init
orch agent add builder --adapter claude --approval-policy auto      # prints the agent id
orch agent update <agent-id> -f "$EX/agent-update.yaml"
orch agent patch  <agent-id> -f "$EX/agent-patch.json"
orch task add "Add input validation" -p 3                           # prints the task id
orch task update <task-id> -f "$EX/task-update.json"
orch task patch  <task-id> -f "$EX/task-patch.yaml"
orch config patch -f "$EX/config-patch.yaml"
```

| File | Command | Effect |
|---|---|---|
| `agent-update.yaml` | `agent update` | Replaces role and config; clears whatever is not listed |
| `agent-patch.json` | `agent patch` | Removes `config.model`, sets `config.max_turns` to 100 |
| `task-update.json` | `task update` | Replaces title, description, priority and labels |
| `task-patch.yaml` | `task patch` | Sets priority 1 and removes the labels |
| `config-patch.yaml` | `config patch` | Sets `scheduling.max_concurrent_agents` to 3 and adds `plugin.x` |
