# Spine observer publication on native prompt revert

Paginated `thread/revert` keeps the thread id and replaces its runtime and canonical
rollout path. The old rollout is an immutable history reference, not truncated in
place (`thread-store/src/local/revert_thread.rs`).

## Runtime and listener boundaries

The thread-list state permit covers removal of the old runtime through registration
and response reconstruction of its replacement. The old listener drains Core's
shutdown events and exits after signalling the revert drain waiter: it must not
later idle-unload shared state during reload. The replacement acquires fresh idle
watches from the preserved subscriptions.

Thread-scoped RPCs, including revert and unsubscribe, execute on the same exclusive
queue. A public unsubscribe cannot run concurrently inside a revert. Transport
connection close is different: after the connection RPC drain deadline, it removes
subscriptions even if the handler is still running. If it finds the temporary
no-runtime gap, bookkeeping reconciliation waits for the existing thread-list
permit and rechecks the manager before teardown. It cannot carry a stale
`notLoaded` observation past replacement registration. Removing subscriptions is
not delayed by that permit; a prepared listener still unloads normally when idle.

A successful Spine-enabled revert publishes in this order:

1. The revert response.
2. `thread/reverted`, invalidating the client's previous observer tree.
3. The replacement runtime's queued Spine replay.

Starting consumption before step 2 can make the TUI discard the newly replayed
tree and leave its status stale. Preparation is therefore separate from activation.
Immediately after resume registers the replacement and its id is checked, acquire
unloading watches, register cancellation/commands, and spawn the held listener.
This precedes settings restoration, checkpointing, client metadata and response
reconstruction. Releasing the publication guard after step 2 starts consumption.
Dropping it on an error also starts consumption, without synthesizing a success
response or `thread/reverted` (R037 follows upstream's success-only contract).
Cancellation wins over activation. Only commands/events are held; idle watches
remain active. Cleanup revokes the command registration only if the channel still
belongs to that task, so an old state cannot revoke a cold-resumed registration.

Silent settings restoration refreshes the held listener's settings baseline and
skills watches before checkpointing can fail. Refresh only updates a still-matching
listener and cannot reinstall a closed one. Otherwise a duplicate restored settings
value can spuriously notify, or a change back to the resume-time value can be lost.
Activation has no await or error. Native Spine-off publication is unchanged;
rejected stored reverts reload the original history and activate before returning
the error, as before.

## Dynamic coverage

The embedded-app-server regression
`prompt_edit_replays_truncated_spine_tree_through_embedded_app_server` checks the
retained node, discarded node, status, composer, same thread id and follow-up model
input. It also checks the replacement canonical path and immutable old rollout.
Early listener activation is its product mutation.

`thread_revert_listener_tests` retains three lower-level handler tests with real
Core/local persistence: cursor-read failure followed by a real third turn; held
idle teardown and receiver exit; and old-state cleanup across real close/cold
resume, including actual commands through the new registration. Their direct
handler invocation deliberately bypasses the RPC queue: concurrent unsubscribe in
these unit tests is **not** evidence of a production RPC interleaving.

The cursor barrier identifies the response's backwards-cursor query by its store
parameters: archived turns included, no cursor, one descending turn, and items not
loaded. All other turn queries delegate directly to the real store, including the
native pre-revert receipt-retirement scan. This keeps the fault/hold after reload
and listener registration without relying on query order or fabricating receipts.
The history, listener teardown, cold-resume registration and settings assertions
remain unchanged; the listener module runs in both global OFF and configured ON.

`thread_revert_writer_tests` supplies the production `MessageProcessor` revert RPC
queue with the same decorated local store used by Core. Its `as_any` delegates to
the local store, preserving Core's rollout-path discovery. Barriers hold the second
settings checkpoint (the first belongs to resume). Stopping the real local writer
makes actual append/flush return `ThreadNotFound`; the handler preserves that error
and emits neither success nor `thread/reverted`. Reopening only the writer, not the
runtime or listener, permits a real third turn with exactly the retained prefix.

Two closing tests enter through real `MessageProcessor::connection_closed`, including
the product's 30-second RPC drain deadline, not a test sleep. With a prepared
replacement, normal idle unload must close its command receiver before the writer
fault is released; guard drop must not revive it. With the store holding the
committed revert before reload, the old listener must already have exited while
still subscribed. After disconnect removes the last subscription, reconciliation
must not discard shared state in this gap. Reload completes, then the replacement
idles out with no runtime or registry orphan.

The settings behavior test silently changes runtime settings while keeping persisted
resume settings different. It first repeats the restored value and requires no
notification through an ordered `TurnCompleted` event, then changes the value and
requires the actual settings notification. Changing the value first is insufficient:
other normalized settings, such as `serviceTier`, can mask a stale baseline.

Product mutations exercise pre-checkpoint preparation, stale settings baseline,
old drain exit, stale disconnect reconciliation, guard error-drop activation,
held idle unloading, old-channel unregister, and early successful publication.
Reproduction scripts and JUnit evidence live in the exercise task directory.

The guard covers fallible work after replacement registration/preparation, not
failures before a replacement exists. Writer repair is test-only; these tests do
not claim recovery from arbitrary disk corruption, disk-full errors or cancellation
of a task while resume itself is registering its runtime. No Core production fault
hook, storage retry or error suppression is added.
