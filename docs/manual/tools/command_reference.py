#!/usr/bin/env python3
"""Render docs/manual/14-command-reference.md from the `orch --help` tree dump."""
import re, sys

src, dst = sys.argv[1], sys.argv[2]
text = open(src).read()
parts = re.split(r'^=================== orch ?(.*)$', text, flags=re.M)
cmds = []
for i in range(1, len(parts), 2):
    path = parts[i].strip()
    body = parts[i + 1].strip().splitlines()
    about = body[0].strip() if body else ''
    usage = next((l[len('Usage: '):].strip() for l in body if l.startswith('Usage: ')), '')
    cmds.append((path, about, usage))

chapter = {
    'init': ('01-getting-started.md', 'Getting started'),
    'agent': ('03-agents.md', 'Agents'),
    'task': ('04-tasks.md', 'Tasks'),
    'team': ('05-teams-and-goals.md', 'Teams and goals'),
    'goal': ('05-teams-and-goals.md', 'Teams and goals'),
    'msg': ('06-messages-and-context.md', 'Messages and context'),
    'context': ('06-messages-and-context.md', 'Messages and context'),
    'run': ('07-running-and-monitoring.md', 'Running and monitoring'),
    'serve': ('07-running-and-monitoring.md', 'Running and monitoring'),
    'status': ('07-running-and-monitoring.md', 'Running and monitoring'),
    'logs': ('07-running-and-monitoring.md', 'Running and monitoring'),
    'doctor': ('07-running-and-monitoring.md', 'Running and monitoring'),
    'update': ('07-running-and-monitoring.md', 'Running and monitoring'),
    'tui': ('08-dashboard.md', 'The dashboard'),
    'web': ('09-web-dashboard.md', 'The web dashboard'),
    'org': ('10-templates-and-shop.md', 'Templates and the agent shop'),
    'remote': ('11-remote-agents.md', 'Remote agents'),
    'config': ('12-configuration-and-output.md', 'Configuration and output'),
}
special = {'agent shop': ('10-templates-and-shop.md', 'Templates and the agent shop')}
# orch 0.8.0's --help has no summary, or a wrong one, for these commands; the descriptions come from
# docs/org-templates.md. Drop an entry once the command's own help is fixed.
summary_override = {
    'org template': 'Manage organization templates: list, show, create and change them',
    'org deploy': 'Deploy a template: create its agents, a team and an optional goal',
    'org template agent list': 'List the members of a template as written',
    'org template agent add': 'Add a member to a template from a JSON or YAML definition',
    'org template agent edit': "Edit one member's definition in $EDITOR",
    'org template agent update': "Replace one member's definition from JSON or YAML",
    'org template agent patch': "Merge a partial JSON or YAML definition into one member",
    'org template agent delete': 'Remove a member from a template (not the lead)',
}
cmds = [(path, summary_override.get(path, about), usage) for path, about, usage in cmds]

# Where --help overstates what the command does in this version.
caveat = {'update': 'In 0.8.0 it does not install anything; it prints how to upgrade. See [Getting started](01-getting-started.md).'}

def cell(s):
    return s.replace('|', '\\|')

order = ['init', 'agent', 'task', 'team', 'goal', 'msg', 'context', 'run', 'serve', 'status', 'logs',
         'doctor', 'update', 'tui', 'web', 'org', 'remote', 'config']
top = {c[0]: c for c in cmds if c[0] and ' ' not in c[0]}
assert set(order) == set(top), sorted(set(top) ^ set(order))

out = []
w = out.append
w('# Command reference')
w('')
w('Every orch command, with what it does and the chapter that explains how to use it. Use this page to find the')
w('right command; use the chapter for steps and examples. For every option of a command, run it with `--help`:')
w('')
w('```sh')
w('orch task add --help')
w('```')
w('')
w('> **Note:** Only `orch --help`, `orch --version`, `orch init`, `orch doctor`, `orch update` and the')
w('> `orch org template` commands work outside a project. Every other command, even with `--help`, needs a project:')
w('> run it in a directory where `orch init` has run, or in a subdirectory of one. Otherwise it prints')
w('> `Not initialized` and exits with code 3.')
w('')
w('How to read the usage column:')
w('')
w('| Notation | Meaning |')
w('|---|---|')
w('| `<id>` | A value you must give, such as a task ID |')
w('| `[OPTIONS]` | Optional flags, listed by `--help` |')
w('| `[COMMAND]` | A subcommand, listed in the rows below |')
w('')
w('## Options that work with every command')
w('')
w('`-V, --version` works only on its own (`orch --version`); the others work with any command.')
w('')
w('| Option | What it does |')
w('|---|---|')
for opt, desc in [('--json', 'Output as JSON'), ('--yaml', 'Output as YAML'),
                  ('--quiet', 'Minimal output (IDs only)'), ('--no-color', 'Disable colors'),
                  ('--ascii', 'ASCII-only output (no Unicode)'), ('-h, --help', 'Print help'),
                  ('-V, --version', 'Print version')]:
    w(f'| `{opt}` | {desc} |')
w('')
w('See [Configuration and output](12-configuration-and-output.md) for how the output options behave.')
w('')
w('## Commands')
w('')
for t in order:
    group = [c for c in cmds if c[0] == t or c[0].startswith(t + ' ')]
    f, title = chapter[t]
    w(f'### orch {t}')
    w('')
    w(f'{cell(top[t][1])}. Explained in [{title}]({f}).' + (' ' + caveat[t] if t in caveat else ''))
    w('')
    w('| Command | What it does | Usage |')
    w('|---|---|---|')
    for path, about, usage in group:
        link = special.get(path)
        what = cell(about) + (f'. See [{link[1]}]({link[0]}).' if link else '')
        if path in caveat:
            what += '. ' + caveat[path]
        w(f'| `orch {path}` | {what} | `{cell(usage)}` |')
    w('')
w('---')
w('')
w('This page is generated from the `--help` output of orch 0.8.0. If it and `--help` ever disagree, `--help` is')
w('right; see [About this manual](about-this-manual.md#keeping-the-manual-accurate).')
open(dst, 'w').write('\n'.join(out) + '\n')
print(len(cmds), 'commands')
