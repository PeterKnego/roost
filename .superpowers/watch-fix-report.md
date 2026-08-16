# Large-project watcher fix (lazy-tree branch)

## Bug

`Hub::for_project` called `watch::spawn` synchronously on the connection
thread. `watch::spawn` walked the whole project tree and registered one
non-recursive OS watch per directory before returning. On a project with
16,682 directories this made `for_project` — and therefore the first
`State` snapshot the client needs to render anything — block indefinitely
on the connection thread. The workspace pane stayed empty forever; the
websocket itself connected fine.

## Changes

### 1. `src/hub.rs` — watcher setup moved off the connection thread

`Hub::for_project` still does the test-and-set on `watching` under the hub
lock exactly as before (so a second connection racing in does not spawn a
second watcher), but the actual `watch::spawn(...)` call now runs inside
its own `std::thread::spawn`. `for_project` returns immediately regardless
of project size. When that background thread finishes, it takes the hub
lock, sets `ws.watch_degraded`, and — only if watching came up degraded —
broadcasts a fresh `State` snapshot so already-connected clients learn
watching didn't fully succeed (a client that got its first snapshot before
setup finished would otherwise never find out).

### 2. `src/watch.rs` — recursive watch on macOS/Windows, capped per-directory walk on Linux

- `watch_tree` is now two `cfg`-gated implementations:
  - `#[cfg(any(target_os = "macos", target_os = "windows"))]`: registers a
    single `RecursiveMode::Recursive` watch on the project root. No walk at
    all. FSEvents/ReadDirectoryChangesW handle the subtree; `classify`
    (unchanged) still filters `SKIP_DIRS` and non-open paths on the event
    path, so correctness is unaffected — the only cost is some discarded
    events.
  - `#[cfg(not(...))]` (Linux/other): unchanged per-directory
    `RecursiveMode::NonRecursive` walk, now bounded by a new
    `MAX_WATCHED_DIRS = 8192` constant (documented: inotify's default
    `max_user_watches` is commonly 8192–65536, and both VS Code and
    IntelliJ degrade visibly here rather than erroring or hanging).
- The bounded-walk logic was extracted into a pure, platform-independent
  helper, `collect_watch_dirs(root, already, cap) -> (Vec<PathBuf>, bool)`,
  that does the filesystem walk (skipping `SKIP_DIRS`, not following
  symlinks) and stops at `cap`, counting from `already` so the initial walk
  and later "a directory was just created" calls share one budget for the
  life of a watcher. It makes no OS watch calls, so it's directly testable
  without a real watcher and without needing to run on Linux.
- The per-connection-thread dynamic re-registration of newly-created
  directories (needed on Linux because inotify never reports anything from
  a directory that was never explicitly watched) is now
  `#[cfg(not(macos/windows))]`-gated — skipped entirely on the recursive
  platforms, where the one root watch already covers directories created
  later.
- `spawn()` threads a `watched: usize` counter (Linux/other only) from the
  initial walk through to every later dynamic-registration call, so the cap
  is enforced across the whole life of the watcher, not reset per call.
  Hitting the cap logs once (on the call that crosses it, not on every
  subsequent directory) and returns `false`, which `for_project`'s
  background thread turns into `watch_degraded = true`.
- The `.git` watch (needed for `.git/index` / `.git/HEAD` → `StatusChanged`)
  is registered unconditionally in `spawn`, as before.

### 3. `tests/integration.rs` — one existing test adjusted for the new async race

`external_edit_updates_a_clean_buffer_live` used to write the test file
once, immediately after connecting, and assumed the watcher was already
live (true under the old synchronous-spawn design). With watcher setup now
asynchronous, there's a real, expected window right after connecting where
the watcher isn't registered yet — this is inherent to the fix. Rather than
writing once, the test now rewrites the file every 100ms in a background
thread until the client observes the change, so it no longer depends on
winning that race on the first attempt. Confirmed this was a genuine race
(not a fluke): the unmodified test failed ~1 run in 3 before this change;
after the fix, 8/8 full integration runs and 5/5 targeted runs passed.

## Tests added

- `src/watch.rs`: `bounded_walk_stops_at_the_cap`,
  `bounded_walk_does_not_hit_the_cap_on_a_small_tree`,
  `bounded_walk_honors_an_already_count_from_a_prior_call` — exercise
  `collect_watch_dirs` directly (no OS watches involved), covering the cap
  being hit, not being hit, and being pre-exceeded via `already`.
- `src/hub.rs`: `for_project_returns_promptly_on_a_large_tree` — creates a
  temp project with 4,000 directories, calls `Hub::for_project`, and
  asserts it returns in under 500ms. This is the regression test for the
  actual reported bug.
- Existing tests, including `watch::tests::symlink_loop_does_not_hang_the_walk`,
  are unchanged and still pass.

## Test command and full output

```
cd /Users/peter/Projects/deadlight && cargo test
```

```
running 100 tests
...
test hub::tests::for_project_returns_promptly_on_a_large_tree ... ok
...
test watch::tests::bounded_walk_does_not_hit_the_cap_on_a_small_tree ... ok
test watch::tests::bounded_walk_honors_an_already_count_from_a_prior_call ... ok
test watch::tests::bounded_walk_stops_at_the_cap ... ok
...
test watch::tests::symlink_loop_does_not_hang_the_walk ... ok
...
test result: ok. 100 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.39s

     Running unittests src/main.rs
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/integration.rs
running 22 tests
test external_edit_updates_a_clean_buffer_live ... ok
test invalid_session_name_is_refused ... ok
test set_mode_edit_then_save_writes_the_file ... ok
test reconnect_replays_buffer_text_for_open_edit_buffers ... ok
test terminal_ws_echoes_through_pty ... ok
test two_terminal_clients_mirror_one_session ... ok
test workspace_socket_malformed_json_is_reported_not_fatal ... ok
test workspace_socket_rejects_foreign_origin ... ok
test workspace_socket_rejects_missing_origin ... ok
test workspace_state_mirrors_between_two_clients ... ok
test ws_closes_when_child_exits_first ... ok
test ws_rejects_foreign_and_missing_origin ... ok
(+10 more, all ok)
test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.23s

   Doc-tests deadlight
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

122 tests total (100 lib + 22 integration), 0 failures. `cargo build`
produces only one pre-existing, unrelated warning (`unused import: Mode` in
`src/workspace.rs`), confirmed present on `master`/pre-change `lazy-tree`
too — nothing new introduced by this change.

## Needs browser verification (controller)

This dev machine is macOS, so only the recursive-watch (`RecursiveMode::Recursive`
on the project root) code path in `watch.rs` has actually executed here —
the Linux per-directory/capped path (`#[cfg(not(any(target_os = "macos",
target_os = "windows")))]`) compiles (verified via reading, not via a
Linux build — no cross-compile toolchain available in this environment)
but was only exercised indirectly, through the platform-independent
`collect_watch_dirs` unit tests. Please verify live against the reported
16,682-directory project:

1. Open the project in a browser: the workspace pane should now populate
   promptly (this is the primary regression — previously it hung
   indefinitely).
2. Confirm a `State` websocket message arrives quickly after connecting
   (should no longer need 90s of polling).
3. Since this is macOS, watching for that project uses the single
   recursive FSEvents watch — edit a file inside the project externally
   (e.g. `touch`/edit via another tool) and confirm the tree/buffer updates
   live, the same as before this change.
4. If there's any way to test this against a Linux deadlight process (CI,
   a container, another machine), specifically check: (a) a project
   smaller than 8192 directories still gets full live-watching, (b) a
   project larger than 8192 directories loads promptly and shows some
   degraded-watch indication in the UI (via `watch_degraded` on the
   `State` event) rather than hanging or erroring.
