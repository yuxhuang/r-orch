# Remote agents

Remote agents let one machine hand tasks to agents that run on another machine running orch. Read this chapter when you want to use a second computer, for example a Linux box for tests, from your main project. You need two things before you start: orch 0.9.3 on both machines, and a network you trust (see the warning below).

## In this chapter

- [How remote agents work](#how-remote-agents-work)
- [Restrict who can connect](#restrict-who-can-connect)
- [Set up a worker and a controller](#set-up-a-worker-and-a-controller)
- [Assign work to a remote agent](#assign-work-to-a-remote-agent)
- [Approve work on the worker](#approve-work-on-the-worker)
- [Use capability labels](#use-capability-labels)
- [Check the state of your remotes](#check-the-state-of-your-remotes)
- [What happens when a worker goes offline](#what-happens-when-a-worker-goes-offline)
- [The environment of remote runs](#the-environment-of-remote-runs)
- [Set up two machines that control each other](#set-up-two-machines-that-control-each-other)
- [Commands](#commands)
- [Upgrade notes](#upgrade-notes)

## How remote agents work

| Term | Meaning |
|---|---|
| Controller | The machine where you create tasks and assign them. |
| Worker | The machine that runs the agents. It listens for connections. |
| Peer | Any other machine running orch. |
| Remote | A worker that you registered on the controller with `orch remote add`. |
| Peer name | The name a machine announces to its peers. |
| Alias | The short name you give a remote on the controller. Its agents appear as `<agent-id>@<alias>`. |
| `cap:` label | A task label such as `cap:linux` that requires a worker capability (see [Use capability labels](#use-capability-labels)). |

Two terms matter in more detail.

- The **controller** is the machine where you create tasks and assign them.
- The **worker** is the machine that runs the agents. It listens for connections.

A peer is any other machine running orch. Each machine has a **peer name** that others see. The default is the short host name. Set it with `--peer-name NAME`, or `peer.name` in `.orchestry/config.yml`.

On the controller you register a worker under an **alias** with `orch remote add`. The worker's agents then appear in your agent list as `<agent-id>@<alias>`, for example `agt_x@w1`. You assign tasks to them like any other agent.

Remote agents are leaf executors that use the worker's own resources.

- orch does not copy your repository, files or results between machines. Paths in a task refer to the worker.
- The worker runs its own agent command, role, skills, workspace setup and review criteria. Prepare them there before you assign work.
- Hand results back through summaries or messages.
- A remote agent must not run `orch task add`, `orch goal` or `orch context` expecting to change the controller's project. Those commands change the worker's own project.

A controller can connect to several workers. Each worker accepts one controller at a time. Only the orch process that runs the orchestrator for the project (`orch serve`, `orch run --watch`, or a dashboard started without `--observe` while no other orch process runs the orchestrator) opens peer connections. A dashboard that finds another orch process already running the orchestrator only observes it.

## Restrict who can connect

Peer traffic is plain TCP. It has no password, no token, no allow list and no encryption.

> **Warning:** A listening worker runs whatever a connecting peer sends it. Any machine that can reach the port can list the worker's agents and submit tasks to them, including `shell` agents that run any command. Anything sent between peers (messages, task results) can be read on the network. Listen only on a network where you trust every machine. Never listen on a public interface, shared Wi-Fi or a VPN that strangers share. Prefer a firewall that admits only your controller's IP address.

Listening is off by default. To limit exposure:

1. Bind to one specific address, the worker's LAN IP, not to every interface.
2. Add a firewall rule that allows port 7690 only from the controller.
3. Keep `--listen` off on machines that do not need to accept work.

A bare `--listen` binds `127.0.0.1:7690`, a loopback address that only the same machine can reach. To accept another machine, give an address explicitly: a specific LAN IP (preferred) or `0.0.0.0:7690` (all interfaces). For any address other than loopback, orch prints a warning on start, and `orch remote status` and the dashboard header show `(EXPOSED, no auth)`:

```text
  Listen: 0.0.0.0:17690 (EXPOSED, no auth)
  Controller: none
  ⚠ Peer listener 0.0.0.0:17690 accepts connections on ALL network interfaces with no authentication or encryption: any host that can reach it can run tasks on this instance's agents, including shell commands. Use it only on a trusted network; bind 127.0.0.1 to keep it local.
```

## Set up a worker and a controller

### On the worker

Before you start: the worker has the project initialized (`orch init`), its agents added, and the agent CLIs installed and signed in.

1. Start the orchestrator with a listen address and a name:

   ```sh
   orch serve --listen 192.168.1.20:7690 --peer-name worker-linux
   ```

   `--listen` also works with `orch run --watch` and `orch tui`. A `--listen` flag applies to that run only.

2. To make listening a saved preference for later starts, run:

   ```sh
   orch remote listen on 192.168.1.20:7690
   ```

   Without an address, `on` saves `127.0.0.1:7690`. `orch remote listen off` turns it off. Both take effect the next time an orchestrator starts, not on one that is running.

3. Check with `orch remote status`. The `Listen:` line shows the address.

A dashboard that only observes another orchestrator cannot enable listening. `orch tui --listen` then stops with `--listen requires the engine owner; another orchestrator holds the lock`. "Engine owner" in that message means the orch process that runs the orchestrator.

### On the controller

1. Register the worker. The alias uses lower-case letters, digits and hyphens. The port defaults to 7690.

   ```sh
   orch remote add w1 192.168.1.20
   ```

   ```text
     ✓ Added remote w1
   ```

2. Start the orchestrator, if it is not running:

   ```sh
   orch serve --peer-name controller
   ```

3. Check the link:

   ```sh
   orch remote list
   ```

   ```text
     ALIAS  ADDRESS            STATUS   
     w1     192.168.1.20:7690  offline  
   ```

   In this capture no worker was listening, so the status is `offline`. With a running worker it reads `online`.

The registry survives restarts. Links reconnect with growing waits until you run `orch remote remove w1`. Remove also deletes the cached status.

## Assign work to a remote agent

1. List agents and find the worker's agent ID:

   ```sh
   orch agent list
   ```

   Remote agents show as `<id>@<alias>`. The CAPS column shows host capabilities such as `linux` or `macos`.

2. Create a task for it:

   ```sh
   orch task add "Run the worker's checks" --assignee 'agt_x@w1'
   ```

3. Send a message if you need to:

   ```sh
   orch msg send 'agt_x@w1' "Please include the test results in your summary"
   ```

4. From the worker, you can reply to the controller's command line:

   ```sh
   orch msg send 'cli@controller' "Checks passed"
   ```

Replace `agt_x` with the real ID. Quote the `@` form so your shell does not misread it. Broadcast messages stay local.

Remote runs do not count against the controller's `scheduling.max_concurrent_agents`. The worker enforces its own limit, and each remote agent runs one task at a time.

How a remote task is shown in `orch task list`, `orch task show` and the dashboard:

| Shown | Meaning |
|---|---|
| `dispatched → w1` | The request is queued locally; the worker has not acknowledged it. After 5 minutes online with no answer the task fails. |
| `queued @w1` | The worker holds it but has not started. If it says `awaiting approval`, see the next section. |
| `RUN @w1` | The worker's agent is running. |
| `RUN? (w1 offline)` | The link dropped after submit. Nobody here knows the state; the next connection settles it. |

## Approve work on the worker

The worker's agent decides whether a remote task runs. Only an agent whose approval policy is `auto` runs it straight away. With `suggest`, `manual`, or no stored policy, the task waits.

A waiting task sits in `todo` on the worker with the label `awaiting-approval`. The controller shows it as in progress. Nothing runs until someone approves it **on the worker**, using the worker's own task ID. It differs from the controller's ID.

1. On the worker, find the task:

   ```sh
   orch task list
   ```

2. Approve it:

   ```sh
   orch task approve tsk_05O0U7_
   ```

   ```text
   ✓ Approved tsk_05O0U7_ → released to run
   ```

Check an agent's policy with `orch agent show <id>`. The `Agent Creator` agent from `orch init` uses `suggest`. A remote task can wait indefinitely for someone at the worker. If one waits an hour, the controller reports it once an hour, and never fails it.

## Use capability labels

A worker advertises host capabilities, `linux` or `macos`. To require one, add a label to the task:

```sh
orch task add "Build on Linux" --assignee 'agt_x@w1' --labels cap:linux
```

If the worker does not have the capability, the task stays waiting.

## Check the state of your remotes

```sh
orch remote status
```

```text
  Listen: off
  Controller: none
  w1: offline (connecting)
```

| Line | Meaning |
|---|---|
| `Listen:` | The effective listen address, or `off`. `(configured …, not active)` means you saved a preference that has not taken effect. |
| `Controller:` | The machine currently controlling this one. |
| `<alias>:` | The state of each remote you registered, with the reason and the retry countdown when offline. |

Common states: `offline (connecting)`, `offline (Peer closed connection; retry in 12s, attempt 5)`, `offline (Worker busy: controlled by mac; retry in 8s)` and `online v<version>`. `(version unknown)` beside an online peer means it is an older build; the link is fine. A climbing `attempt` with a short `retry in` means the worker accepts connections and closes them: look at `orch remote status` and `serve.log` on the worker. See [Reading a peer's link state](../RUNBOOK.md#reading-a-peers-link-state).

## What happens when a worker goes offline

- Offline agents stay visible. New tasks assigned to them wait in `todo`.
- A remote task already running waits for its result, without a retry, because the worker owns execution. Cancelling the task, or removing the remote, ends the wait.
- Messages queue while the link is down and are delivered once, without duplicates, on reconnect.
- Restarting the controller keeps remote runs and reattaches to them.
- A remote counts as online only while the link is live and the controller can save that state in `.orchestry/remotes/`. If the directory is unwritable or the disk is full, the remote shows `offline` even though the link is up. Nothing new is sent to it until the status can be saved. Free space or fix permissions. The remote returns by itself.
- A worker cancels leftover `todo` copies (called mirrors: the task the worker makes for each request from a controller) of tasks from a controller it has not seen for 24 hours.
- `orch remote remove` ends in-flight remote tasks for that alias with an unknown outcome.

Local agents keep running through all of this. For the phases and messages in detail, see [Remote task phases](../RUNBOOK.md#remote-task-phases) and [Mirrors left behind on a worker](../RUNBOOK.md#mirrors-left-behind-on-a-worker).

## The environment of remote runs

A run started for a remote controller does not inherit secret-looking environment variables from the worker's `orch` process. Local runs are not affected.

orch drops a variable when its name matches one of these rules, ignoring case:

- A segment of the name (the parts between underscores) is one of `KEY`, `KEYS`, `APIKEY`, `TOKEN`, `TOKENS`, `SECRET`, `SECRETS`, `PASSWORD`, `PASSWD`, `PASS`, `PAT`, `AUTH`, `OAUTH`, `CREDENTIAL`, `CREDENTIALS` or `COOKIE`. Examples: `DB_PASS`, `GITHUB_PAT`.
- The name contains `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `APIKEY`, `CREDENTIAL` or `PRIVATEKEY` anywhere.

So `ANTHROPIC_API_KEY`, `GITHUB_TOKEN` and `SSH_AUTH_SOCK` are dropped, and so is the harmless `TOKENIZERS_PARALLELISM`. Matching is by name only. A secret under a plain name, such as `DATABASE_URL`, is passed on.

> **Warning:** Name-based filtering is not a security boundary. Do not keep secrets in variables with innocent names on a worker that accepts remote tasks.

The effect: if a worker's `claude` or `codex` signs in through an environment variable, or git needs `SSH_AUTH_SOCK`, remote tasks fail to authenticate while the same agent works locally. Allow chosen variables by exact name when you start the worker's orchestrator:

```sh
ORCH_REMOTE_ENV_PASSTHROUGH=ANTHROPIC_API_KEY,SSH_AUTH_SOCK orch serve --listen 192.168.1.20:7690
```

The list is separated by commas or spaces, has no wildcards, and is read at the start of each run. Variables in an agent's own `config.env` are always passed.

## Set up two machines that control each other

A worker refuses a controller whose peer name equals one of its own aliases. This keeps `<id>@<alias>` unambiguous. The natural mutual setup trips over it: `mini` and `studio` each register the other under its host name, and both links are refused.

The symptom, on `mini`:

```text
  Listen: 127.0.0.1:17690
  Controller: none
  studio: offline (Inbound controller name collides with a configured outbound alias)
```

Fix it by giving each side a peer name that is not an alias on the other side:

```sh
orch serve --listen 192.168.1.20:7690 --peer-name mini-ctl      # on mini
orch serve --listen 192.168.1.21:7690 --peer-name studio-ctl    # on studio
```

Then `orch remote status` on both machines shows the other as online and as `Controller:`. One controller and one worker never hit this.

## Commands

| Command | What it does |
|---|---|
| `orch remote add ALIAS ADDR` | Registers a worker. `ADDR` is `host[:port]` or `[ipv6]:port`; the port defaults to 7690. The alias matches `[a-z0-9][a-z0-9-]*`. |
| `orch remote remove ALIAS` | Unregisters a remote. Its in-flight tasks fail with an unknown outcome. |
| `orch remote list` | Lists remotes with address and online state. |
| `orch remote status` | Shows the listen address, controller and each remote's state. |
| `orch remote listen on [ADDR]` / `off` | Saves the incoming-connection preference for the next time an orchestrator starts. |
| `--listen [ADDR]` on `serve`, `run` and `tui` | Listens for this run only. Overrides the saved preference. |
| `--peer-name NAME` on `serve`, `run` and `tui` | Sets the name other machines see. |

All of them accept `--json`, `--yaml` and `--quiet`. An invalid alias is refused:

```text
  ✕ Invalid remote alias: "bad_Alias"; expected [a-z0-9][a-z0-9-]*
```

## Upgrade notes

- Run the same orch version on every machine. Upgrade the controller and the workers together.
- Upgrade the orch process that runs the orchestrator and the `orch` command on the same machine together.
- If your agents need a credential from the environment on a worker, set `ORCH_REMOTE_ENV_PASSTHROUGH` (see [The environment of remote runs](#the-environment-of-remote-runs)). Remote tasks also wait for approval on the worker (see [Approve work on the worker](#approve-work-on-the-worker)).
- Before you install an older orch version on a machine, make sure no remote runs are in flight. Older versions cancel them.

The full upgrade history is in the [README](../../README.md#upgrading-from-v040-and-rolling-back).
