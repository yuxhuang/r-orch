# Messages and context

Messages and context are the two ways to pass information to agents. Read this chapter to send an agent a note, to share a fact with every agent, and to choose between the two. For the short version, see [Messages](02-concepts.md#messages) and [Context](02-concepts.md#context).

## In this chapter

- [Messages or context: which to use](#messages-or-context-which-to-use)
- [Messages](#messages)
  - [Send a message to one agent](#send-a-message-to-one-agent)
  - [Send a message to many agents](#send-a-message-to-many-agents)
  - [Check what an agent will receive](#check-what-an-agent-will-receive)
  - [List messages](#list-messages)
  - [When an agent gets a message](#when-an-agent-gets-a-message)
  - [Limits](#limits)
- [Context](#context)
  - [Set a context entry](#set-a-context-entry)
  - [Read, list and delete entries](#read-list-and-delete-entries)
  - [How agents see the context](#how-agents-see-the-context)

## Messages or context: which to use

| | Message | Context entry |
|---|---|---|
| Goes to | One agent, or the members of a group | Every agent |
| Read | Once, at the start of the recipient's next run | At the start of every run |
| Lasts | Until it is delivered, or for 24 hours by default | Until you delete it, or its time to live ends |
| Use it for | An instruction or question: "keep the notes under 200 words" | A fact that stays true: "the API is at http://localhost:3000" |

Rules of thumb:

- If only one agent needs it, and only once, send a message.
- If every agent should know it for a while, set a context entry.
- If the text is long, write it to a file in your project and send the path.

## Messages

A message is a note for an agent. You send messages from the command line or from the dashboard, and agents send them too.

### Send a message to one agent

```sh
orch msg send agt_NnBifRi "Please keep the notes under 200 words" -s "Style"
```

```text
  ✓ Message sent: msg_Ffv7Yn5 → agt_NnBifRi
```

The recipient must be an agent's ID. A name is refused:

```text
  ✕ Recipient agent not found: noter
```

| Option | What it does | Default |
|---|---|---|
| `<to-agent-id>` | The recipient's agent ID. Required. A remote agent is written `<id>@<alias>`. | |
| `<body>` | The text. Required. | |
| `-s`, `--subject <subject>` | A subject line. | `(no subject)` |
| `--from <agent-id>` | The sender shown to the recipient. | `cli` |
| `--ttl <ms>` | How long the message stays valid, in milliseconds. From 1 to 604800000 (7 days). | 86400000 (24 hours) |
| `--reply-to <msg-id>` | Marks the message as a reply to another message. | None |

### Send a message to many agents

```sh
orch msg broadcast "Standup at 10" --from agt_3cU5sQA
```

```text
  ✓ Broadcast sent to 3 agent(s)
```

A broadcast creates one message per recipient. It skips the sender and every disabled agent, and it stays on this machine; it does not reach remote agents.

| Option | What it does | Default |
|---|---|---|
| `<body>` | The text. Required. | |
| `-s`, `--subject <subject>` | A subject line. | `(no subject)` |
| `--from <agent-id>` | The sender. | `cli` |
| `--team <team-id>` | Sends only to the members of this team. | All agents |
| `--ttl <ms>` | How long each message stays valid, in milliseconds. | 86400000 |

To message one team:

```sh
orch msg broadcast "Team only" --team team_XWLwLTn
```

```text
  ✓ Broadcast sent to 2 agent(s)
```

You can also send from the dashboard command bar with `/msg`. See [Message an agent](08-dashboard.md#message-an-agent).

### Check what an agent will receive

`orch msg inbox <agent-id>` shows the messages that wait for the agent. These are the messages the agent reads in its next run.

```sh
orch msg inbox agt_NnBifRi
```

```text
  msg_Ffv7Yn5 from cli — Style
  Please keep the notes under 200 words
```

After the agent has read them, the inbox is empty:

```text
  No pending messages.
```

An expired message does not appear in the inbox.

### List messages

`orch msg list` shows every message, with its status.

```sh
orch msg list --agent agt_NnBifRi
```

```text
  ID           FROM  TO           CHANNEL    STATUS     SENT
  msg_Ffv7Yn5  cli   agt_NnBifRi  direct     delivered  14s
  msg_NIZg50K  cli   agt_NnBifRi  broadcast  delivered  14s
  msg_CD7j46E  cli   agt_NnBifRi  direct     pending    9s
```

| Option | What it does |
|---|---|
| `--agent <agent-id>` | Shows messages the agent sent or received. |

| Column | Meaning |
|---|---|
| CHANNEL | `direct` for `msg send`, `broadcast` for `msg broadcast`. |
| STATUS | `pending` until delivered, then `delivered`. |
| SENT | How long ago the message was sent. |

A `pending` message whose time to live has passed still shows as `pending` here. It is never delivered.

### When an agent gets a message

An agent reads its messages at the start of a run. orch collects the agent's pending messages, puts them in the agent's prompt under the heading `Inbox`, and marks them `delivered`.

- A message does not interrupt a run that is going.
- If the agent never runs, the message waits, and it expires after its time to live. The default is 24 hours. Nothing tells you that a message expired.
- All pending messages for the agent arrive together, in the first run that starts after you send them.

A `shell` agent that prints its prompt shows the delivery:

```text
19:09:42  ▸ ## Inbox (4 messages)
19:09:42  ▸ ---
19:09:42  ▸ **From:** cli · **Subject:** Style
```

For anything that must arrive soon, check that something is starting runs: the orchestrator is running (`orch status` says `watching`), and the agent is not disabled ([Running and monitoring](07-running-and-monitoring.md)).

### Limits

| Limit | Value |
|---|---|
| Body length | 4000 characters. orch cuts a longer body to 4000 and does not warn you on the command line. The dashboard tells you when it cuts. |
| Time to live | From 1 millisecond to 7 days. The default is 24 hours. |

> **Note:** Send long text as a file path, not as a message body. orch silently cuts the end of a body that is longer than 4000 characters.

In the dashboard, read a long message in full with the viewer:

![The full-text viewer showing one complete activity entry, a failed run's error message, with the copy control in the footer](images/dashboard-message-viewer.png)

*The full-text viewer shows a snapshot of the whole text. Close and reopen it to see newer content.*

See [Read long text in the viewer](08-dashboard.md#read-long-text-in-the-viewer).

## Context

The context is a shared store of named values. Every agent sees every entry.

### Set a context entry

```sh
orch context set api-base-url "http://localhost:3000"
```

```text
  ✓ Set context "api-base-url"
```

Setting a key that exists replaces its value.

| Option | What it does | Default |
|---|---|---|
| `<key>` | The entry's name. Required. | |
| `<value>` | The text to store. Required. | |
| `--ttl <ms>` | The time to live in milliseconds. After that the entry disappears. | No limit |

Use a short, specific key such as `build-status` or `api-base-url`. Entries are stored in `.orchestry/context/`.

### Read, list and delete entries

```sh
orch context list
```

```text
  KEY           VALUE                  UPDATED  TTL
  api-base-url  http://localhost:3000  0s       —

  1 entries
```

```sh
orch context get api-base-url
```

```text
  api-base-url = http://localhost:3000
```

```sh
orch context delete api-base-url
```

```text
  ✓ Deleted context "api-base-url"
```

A key that does not exist, or whose time to live has ended, is reported as not found:

```text
  ✕ Context key "nope" not found
```

`orch context delete` prints the same success line for a key that does not exist.

Use `--json` or `--yaml` for scripts. `orch context set k2 v2 --quiet` prints only the key.

### How agents see the context

When orch starts a run, it adds every context entry to the agent's prompt:

```text
## Shared Context
Other agents have shared the following information:
- **api-base-url**: http://localhost:3000
- **k**: v
```

The prompt tells agents how to read and write the context with `orch context`, so agents can also share facts with each other. Because every run gets every entry, keep entries short and delete the ones you no longer need.

Agents that work on a goal can write a progress note under the key `<goal-id>-progress`. orch reads it back for that goal ([Goals](05-teams-and-goals.md#goals)).

> **Note:** A remote agent has its own project on its own machine. Use messages, not `orch context`, to reach it. See [Remote agents](11-remote-agents.md).
