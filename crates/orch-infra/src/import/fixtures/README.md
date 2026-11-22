# Read-only development snapshots

These invented examples are copied byte for byte from reviewed **unmerged**
inputs. They are development inputs, not accepted fixture-author deliveries.
No generator or fixture-author branch is changed here.

- T21a `857b462340ca55a702031cb21bb392f89e42c2ec`:
  `fixtures/storage/generated/small/.orchestry/`:
  `task.yml` is `tasks/tsk_-8biszr.yml`, `agent.yml` is
  `agents/agt_BneL5Al.yml`, `config.yml` and `state.json` retain their names,
  `events.jsonl` is `runs/run_FkMwTnz.jsonl`.
- T21b `4202178f499968344558603b3e8bceaf21bd5e25`:
  `fixtures/storage/index-recovery/`: the two `*.json` files are the corresponding
  cases' `expected.json`; `tsk_idx_*.yml` are from
  `index-absent-update-order/source/.orchestry/tasks/`.

The stale-index source itself is the merged `fixtures/storage-v1/index-disagrees`.
Tests open source fixtures read-only and compare their full bytes afterwards.
