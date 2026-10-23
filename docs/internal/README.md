# Engineering internals

How r-orch behaves at runtime, for engineers changing the engine or an adapter.
[ARCHITECTURE.md](../ARCHITECTURE.md) is the structural view (which crate owns what, which traits, the lock
order). The documents here are the flow view: what happens, in what order, when a goal, task, agent or CLI
process is in play. They link to ARCHITECTURE.md and the [user manual](../manual/README.md) rather than
repeating them.

| Document | Covers |
|---|---|
| [orchestration.md](orchestration.md) | The engine: tick loop, scheduling, dispatch and completion, agent-to-agent and agent-to-task handoffs, goals, pause and resume, recovery |
| [driving-agent-clis.md](driving-agent-clis.md) | One run: how an adapter spawns an AI CLI, sends the prompt, parses its output, ends the process, and how the eight adapters differ |

## Reading order

1. **orchestration.md**: [Answers at a glance](orchestration.md#answers-at-a-glance) and
   [§1 The moving parts](orchestration.md#1-the-moving-parts) first. They name the files and processes
   everything else refers to.
2. The rest of orchestration.md, or straight to the question you have (table below).
   [§7 Worked example](orchestration.md#7-worked-example-one-goal-end-to-end) shows one goal end to end.
3. **driving-agent-clis.md** when you need to know what happens inside a single run. Start at
   [Q6](driving-agent-clis.md#q6-how-does-an-agent-drive-the-agent-cli); use the
   [comparison matrix](driving-agent-clis.md#comparison-matrix) for a specific adapter.

## Where each question is answered

The ten questions from the documentation brief. Q1–Q5 are in orchestration.md, Q6–Q10 in
driving-agent-clis.md.

| # | Question | Answer |
|---|---|---|
| Q1 | What orchestration logic is there; how are goals, tasks and agents set up; how is work handled, recorded and handed off? | [orchestration.md §2](orchestration.md#2-q1-orchestration-logic); setup [§2.10](orchestration.md#210-setting-up-goals-tasks-and-agents), evidence [§2.11](orchestration.md#211-evidence-what-is-written-where), checklist [§2.12](orchestration.md#212-checklist-will-this-task-be-handled); handoff [§3](orchestration.md#3-q2-how-does-one-agent-connect-to-another-agent) and [§4](orchestration.md#4-q3-how-does-one-agent-connect-to-another-task) |
| Q2 | How does one agent connect to another agent? | [orchestration.md §3](orchestration.md#3-q2-how-does-one-agent-connect-to-another-agent) |
| Q3 | How does one agent connect to another task? | [orchestration.md §4](orchestration.md#4-q3-how-does-one-agent-connect-to-another-task) |
| Q4 | How does a goal-driven system work? | [orchestration.md §5](orchestration.md#5-q4-how-does-a-goal-driven-system-work) |
| Q5 | How and when does a pause happen, and what makes it continue? | [orchestration.md §6](orchestration.md#6-q5-how-does-a-pause-happen); table [§6.1](orchestration.md#61-pause-and-resume-table) |
| Q6 | How does an agent drive the agent CLI? | [driving-agent-clis.md Q6](driving-agent-clis.md#q6-how-does-an-agent-drive-the-agent-cli) |
| Q7 | How are the request, response and tool calls sent? | [driving-agent-clis.md Q7](driving-agent-clis.md#q7-how-does-the-agent-send-request-response-tool-call) |
| Q8 | How is the conversation with the CLI maintained? | [driving-agent-clis.md Q8](driving-agent-clis.md#q8-how-does-the-agent-maintain-the-conversation-with-the-cli-tool) |
| Q9 | What else matters when driving a CLI? | [driving-agent-clis.md Q9](driving-agent-clis.md#q9-any-other-important-factors-when-driving-the-cli) |
| Q10 | How do the different adapters work? | [driving-agent-clis.md Q10](driving-agent-clis.md#q10-how-do-different-adapters-work) |

## Where one document stops and the other starts

orchestration.md ends when a prepared run is handed to the adapter: the task is claimed, the workspace
prepared, the prompt rendered, the run record created, and `Adapter::execute` called
([§2.4](orchestration.md#24-dispatch-and-collect)). driving-agent-clis.md covers everything inside that run:
argv and prompt transport, stdout parsing into events, process exit and cancellation, and what the collector
takes as the result. What the engine then does with the outcome (review, retry, cascade) is back in
orchestration.md [§2.5](orchestration.md#25-completion-success-review-retry-cascade).

## Code baseline

Both documents describe commit `40a6dc7` (tag `v0.10.0`) and nothing later. orchestration.md cites
`file:line` with shortened paths (prefix table at its top); driving-agent-clis.md uses reference links to
`github.com/yuxhuang/r-orch/blob/40a6dc78a8bd305ac00163282629c50855616788/...#L<n>`. Known gaps and limits:
engine gaps in [orchestration.md §8](orchestration.md#8-known-gaps-pending-fixes) (a row there may already be
fixed on `main`; row 1 is); adapter limits in
[driving-agent-clis.md Q9 "Known limits"](driving-agent-clis.md#known-limits).

To refresh a citation after the code moves:

1. Read the cited line at the baseline: `git show 40a6dc7:<path> | sed -n '<n>p'`.
2. Find the same code at the new commit (`git log -L<n>,<n>:<path> 40a6dc7..HEAD`, or search for it).
3. Check the sentence that cites it is still true. If not, fix the sentence, not just the number.
4. Update the line number, and in driving-agent-clis.md the commit hash in the URL and the `@<sha>` label.
5. When re-pinning a whole document, update its version line at the top and this section.
