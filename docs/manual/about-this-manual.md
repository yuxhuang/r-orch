# About this manual

This page explains who the manual is for, how it was written and checked, and how to keep it accurate. You do
not need it to use orch; start at the [manual's contents](README.md) instead.

## Who it is for

The manual is for people who use orch to coordinate AI coding agents in their own projects: developers and
operators who work in a terminal and use git, but do not read orch's source code. Team leads who want to know
whether orch fits their work can read [Getting started](01-getting-started.md) and
[Concepts](02-concepts.md) only.

It covers orch **0.9.3**: every command, the dashboard, the web dashboard, templates and the agent shop, and
remote agents. It does not cover how orch is built. Contributors should read [AGENTS.md](../../AGENTS.md) and
[ARCHITECTURE.md](../ARCHITECTURE.md).

## How it follows ISO 24495-1

ISO 24495-1:2023, *Plain language — Part 1: Governing principles and guidelines*, sets four principles for
plain-language documents: readers can get what they need, find it easily, understand it, and use it. The manual
applies them like this. It follows the standard's principles; it has not been assessed or certified against it.

| Principle | What we did |
|---|---|
| Readers get what they need | We wrote for named readers (above) and their tasks. Each chapter starts by saying what you can do after reading it. Internals are left out; deep operator material stays in the [runbook](../RUNBOOK.md) and is linked where you need it. |
| Readers can find what they need | Chapters follow the order you meet things: install, concepts, daily work, the dashboards, then advanced topics. Headings name tasks ("Add an agent"). Long chapters list their sections at the top. The [command reference](14-command-reference.md) maps every command to the chapter that explains it, and the [glossary](15-glossary.md) points to the section that explains each term. |
| Readers can understand what they find | Short sentences in the active voice, addressed to you. One name for each thing throughout. Terms are defined where they first appear and in the glossary. |
| Readers can use what they find | Procedures are numbered steps with one action each, with what you should see after the steps that matter. Commands are in copyable code blocks, output shown separately. Options are in tables. Warnings mark anything that can lose data, cost money or expose your machine. |

### How the manual was checked

Before the 0.8.0 release, people who had not written the manual checked it four ways:

- **Facts.** Every command, flag, default, limit, key binding and example output was compared with orch 0.8.0
  itself, by running it in scratch projects and by reading its source. Where older documentation disagreed with
  the program, the manual follows the program.
- **Plain language.** An editor reviewed every chapter against the four principles above: sentence length,
  voice, terms, headings, procedures, warnings and images.
- **Use.** A tester followed the manual as a new user would, starting from its contents page, through the
  quick start and five everyday tasks, and recorded every place they got stuck. Those places were fixed and
  tested again.
- **Safety.** A security reviewer checked the advice on the web dashboard and remote agents against the source,
  and the tools that produce the screenshots.

The update for orch 0.9.3 was checked in three of these ways. Two fact-checkers compared every command and flag,
and the limits, key bindings, figures and example output, with orch 0.9.3. A tester followed the quick start and
the changed procedures (task attachments, the dashboard, templates and troubleshooting) as a new user. A security
reviewer checked the advice on the web dashboard and remote agents, and the tools that produce the screenshots. No
plain-language editor reviewed the 0.9.3 changes.

Some behaviour could not be tested without a second machine or a real AI agent, such as two-machine remote
setups. The chapters that describe it say where it comes from the source or the [runbook](../RUNBOOK.md).

## Conventions

| You see | It means |
|---|---|
| `orch task add` | A command or text you type |
| `<id>` in a usage line | A value you supply |
| `[OPTIONS]` in a usage line | Optional flags; run the command with `--help` to list them |
| **Warning:** | Something that can lose data, cost money or expose your machine |
| **Note:** | Something easy to miss |

Examples were checked against orch 0.9.3 in a scratch project. IDs, times and paths in your output will differ.

## Keeping the manual accurate

When orch changes, update the manual in the same pull request. Two parts are generated and should be regenerated
rather than edited by hand:

- **Command reference.** Build orch, then run:

  ```sh
  docs/manual/tools/gen-command-reference.sh
  ```

  It runs every `orch … --help` in a throwaway project and rewrites
  [14-command-reference.md](14-command-reference.md).

- **Screenshots.** See [tools/README.md](tools/README.md). The script builds a demo project in a temporary
  directory, opens the real dashboard through `orch web` in headless Chrome, and saves the images in
  [images/](images/).

If the manual and `orch <command> --help` disagree, `--help` is right. Please report the difference.
