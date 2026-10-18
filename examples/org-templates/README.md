# Organization-template examples

A runnable template and the inputs for the template mutation commands
([guide](../../docs/org-templates.md)). Nothing here names a person, a host or a path, and no command starts an agent.

```
review-pair/            a template directory (schema_version 1)
  org.yml               name, lead, description, the rule registry and the org defaults
  agents/lead.yml       one file per member; the lead selects a rule only by naming its file in the role text
  agents/reviewer.yml   selects the rule explicitly, overrides the adapter, has an explicit empty skills list
  rules/house-style.md  a shared rule, embedded into the roles that select it when they are deployed
mutations/              inputs for `update`, `patch` and `agent add`
```

Run them in a disposable project with a disposable global config, so the user template root is disposable too:

```bash
EX=/path/to/r-orch/examples/org-templates                     # this directory
export ORCHESTRY_GLOBAL_CONFIG_PATH="$PWD/home/global.yml"    # user templates then live in $PWD/home/templates
mkdir demo && cd demo && orch init

orch org template show --template-dir "$EX/review-pair" --resolved    # preview what deploy would create
orch org deploy --template-dir "$EX/review-pair" --goal "Review the open PR"
orch agent shop --template-dir "$EX/review-pair" --list

T="$(dirname "$ORCHESTRY_GLOBAL_CONFIG_PATH")/templates"       # the user template root, outside demo/
mkdir -p "$T" && cp -R "$EX/review-pair" "$T/"    # a user template must live in a directory named by its org name
orch org template update review-pair -f "$EX/mutations/org-update.json"
orch org template patch  review-pair -f "$EX/mutations/org-patch.yaml"
orch org template agent update review-pair --agent reviewer -f "$EX/mutations/agent-update.yaml"
orch org template agent patch  review-pair reviewer          -f "$EX/mutations/agent-patch.json"
orch org template agent add    review-pair                   -f "$EX/mutations/new-agent.json"
orch org template agent delete review-pair --agent tester
```

| File | Command | Effect |
|---|---|---|
| `mutations/org-update.json` | `org template update` | Replaces `org.yml`; `defaults.max_turns` becomes 60 |
| `mutations/org-patch.yaml` | `org template patch` | Changes the description, sets `defaults.max_turns` to 80 and removes `defaults.approval_policy` |
| `mutations/agent-update.yaml` | `org template agent update` | Replaces `reviewer.yml`; every optional field it omits is cleared (here: `skills`) |
| `mutations/agent-patch.json` | `org template agent patch` | Changes the description, removes the `skills` override and sets `max_turns` to 25 |
| `mutations/new-agent.json` | `org template agent add` | Creates `agents/tester.yml`; refused if it exists |
