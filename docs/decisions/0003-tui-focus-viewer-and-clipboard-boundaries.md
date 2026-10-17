# 0003. TUI focus, snapshot viewer and clipboard transport boundaries

- Status: accepted
- Date: 2026-10-17

## Context

The dashboard truncated agent messages, descriptions and results, had no per-section focus or paging, lost editor
input on `Esc`, and could not copy text. Fixing that touches input routing, message retention, the
`TuiBackend` facade, the terminal and the `orch web` browser client, so the choices below are costly to reverse
and others will ask why. The work is planned in [the TUI QoL plan](../plans/2026-10-16-tui-qol.md).

## Decision

1. **Focus is explicit and separate from selection.** `Focus` names the pane (main list, activity, one section of
   an open detail, viewer). `Tab`/`Shift-Tab` move pane focus; `Left`/`Right` still switch views. Entity letters
   (`e`, `d`, `c`, …) act only from the main list and only with plain or Shift modifiers.
2. **One shared read-only viewer is the read, scroll, select and copy surface.** It opens on an immutable
   snapshot (Human or Raw, or a single entity field) that is never refreshed; closing and reopening shows the
   latest. It is not the editor, and it does not reuse the textarea widget (which would normalize line endings
   and cap the offset at `u16`).
3. **Message bodies are kept whole or rejected whole.** The 2,048-character caps are removed. A body over 1 MiB
   becomes an explicit "Body unavailable" record, never a prefix. Limits are policy: 500 records, 1 MiB per
   body, 32 MiB shared across pending and live messages. They are component bounds, not a claim about process
   RSS, and exclude backend queues, allocator overhead and rendering temporaries. Allocation/frame measurements are acceptance evidence, not a production RSS guarantee.
4. **Copy is a `TuiBackend` facade call, not a TUI side effect.** `copy_text_via(text, Auto | Terminal | Native)`
   returns a ticket; `poll_copy_results` drains outcomes (`NativeWritten`, `TerminalRequested`, `Failed`). The
   TUI never touches the clipboard or stdout itself. The trait defaults to "Copy is not available".
5. **Routing never writes to two destinations.** Explicit `Terminal` and `Native` are absolute. `Auto` goes to the
   terminal when `ORCH_TUI_CLIENT=web`, an `SSH_*` variable, `TMUX` or `STY` is set; otherwise to the host
   clipboard, falling back to the terminal only when the host clipboard was already known to be unavailable
   before the request. A native failure never falls back to OSC 52.
6. **A terminal request is a request.** OSC 52 (`ESC ] 52 ; c ; <base64> BEL`, at most 64 KiB decoded) is
   reported as "requested", never "copied". No tmux/screen wrapper is emitted; tmux needs `set-clipboard on`,
   and GNU screen is unsupported on this path. Orch never edits a user's multiplexer or terminal configuration.
   The host clipboard path (arboard, one long-lived worker, at most 2 tickets, 3 s UI wait, 1 MiB) lives in
   `orch-infra`.
7. **The browser client gets a write-only OSC 52 handler.** `orch web` marks its PTY child
   `ORCH_TUI_CLIENT=web`; `index.html` accepts only `c;` plus strict base64 up to 64 KiB, shows a *Copy to
   clipboard* button, and writes on the user's click. Insecure or denied contexts show a selectable text box. It
   never reads the clipboard. The vendored xterm.js is not modified.
8. **Editor exits are guarded and drafts live in RAM only.** Every path that would drop edits asks first
   (Continue / Keep draft / Discard); quitting offers only Continue or Discard because drafts die with the
   process. Drafts are capped at 20, 2 MiB total and 256 KiB each; nothing is written to disk.
9. **Vim-style editing is not built.** It is feasible with `edtui` (a throwaway compile probe passed, attributed
   in the plan; the pinned toolchain and `cargo deny` were not checked), estimated at 2.5–4 engineer-days. It
   would replace the wizard's textarea, so it needs its own plan and ADR.

## Consequences

- `Tab` no longer switches top-level tabs; help and docs say so.
- Mouse capture now also enables button-motion tracking (DEC 1002) so drags select text; the abnormal-exit
  reset sequence gains `?1002l`.
- Reverting the UI code needs no data migration: nothing on disk, in the CLI or in the peer protocol changed.
- Rolling back the browser handler means the web client can only show the selectable fallback; the host clipboard
  must not be targeted instead.
- Terminal delivery of a copy is never verifiable from inside Orch. Users on tmux, screen, SSH or the browser
  may need *Copy on this host* or the terminal's own selection (`F2`).
- A real-clipboard smoke test has not been run; see the plan's verification status.
