# TUI workflow and worker contracts

Search transitions live in `src/tui/search_state.rs`; terminal rendering, channel
transport, database execution, rows and selection remain outside that module.

- Initial state is Idle. Editing enters Idle with a debounced pending query and
  immediately revokes the previous request. Dispatch clears debounce and enters
  Loading with a unique, increasing ID.
- Only a reply matching Loading's ID can enter Success or Error. Success hands
  owned rows to the app; an empty result is still success. Errors retain previous
  rows and selection. A later dispatch clears the search error and can recover.
- Replacing rows preserves the selected index when possible and clamps it when
  necessary; it refreshes detail through the existing typed ScriptView access.
- Search belongs to the browse session, not the currently visible pane. Opening
  detail, diff, stats or inspection does not cancel it. Replies may refresh the
  underlying browse results while away; returning preserves that browse state.
- Search send/disconnect failures enter Error and stop polling for that request.
  Rendering caches are rebuilt by existing result/detail application methods,
  never by the transition state.

## Worker inventory

| Worker | Queued requests | In-flight work | Reply/closure | Shutdown |
| --- | --- | --- | --- | --- |
| Search | Keep newest queued | Runs to completion | Active search ID only; pane changes retain session | Close sender, join |
| Detail | Keep newest queued | Runs to completion | Active detail ID only | Close sender, join |
| Diff | Keep newest queued | Runs to completion | Active diff ID only | Close sender, join |
| Folder | Keep newest queued | Runs to completion | Active folder ID only | Close sender, join |
| Stats | Keep newest queued | Runs to completion | Active stats ID only | Close sender, join |
| File check | Keep newest queued | Subprocess deadline, shutdown cancellation | Active file-check ID only | Set cancel flag, close sender, join |
| Inspection | Keep newest queued | Runs to completion | Active page ID only; closed pages discard replies | Sender/receiver drop; thread exits after current work |
| Command workflow | One dialog operation | Subprocess cancellation; doctor has 50-second deadline | Dialog owns response; disconnect becomes visible error | Set subprocess cancellation flag; thread is detached |

Coalescing and rejecting stale responses are **not cancellation**. Database
workers do not promise bounded shutdown: joining waits for current work (and
already queued work) to finish. No new interruption mechanism is introduced for
these local catalog reads. File checks can touch network filesystems and already
use an isolated subprocess with a six-second deadline and cancellation before
join; their worker tests cover timeout, queued replacement and shutdown.

For a new workflow, define ownership across navigation, the active ID and terminal
states first. Keep execution and rendering separate, reject late replies before
mutating rows, and document whether close merely discards replies or actually
interrupts execution. Add interruption/deadlines when the operation's blocking
behavior requires them, rather than describing ID invalidation as cancellation.

Search transition tests inject replies directly without thread timing. TUI tests
cover result selection, navigation, debounce and idle/render behavior; worker tests
cover database errors, response IDs and existing file-check interruption guarantees.
