# Filesystem watcher: fix for dropped `Remove` events

## Root cause (already established, not re-diagnosed here)

`notify-debouncer-full` 0.7's rename-correlation queue silently drops `Remove`
events under FSEvents' coalescing on macOS. Raw `notify` on the same
create/modify/delete sequence delivers `Remove(File)`; the debounced stream
never does. deadlight's watcher already did its own batch processing (lock
once, set `tree`/`status` booleans, broadcast once per batch), so the
debouncer's only real job was "wait for quiet, then hand me a batch" — a few
lines of channel timeouts, done directly below.

## Fix

- `src/watch.rs`: replaced `notify_debouncer_full::new_debouncer` with
  `notify::recommended_watcher` feeding a plain `std::sync::mpsc::channel`.
- The watcher thread now hand-rolls the debounce: blocks on `rx.recv()` for
  the first event of a batch, then calls `rx.recv_timeout(debounce)` in a
  loop, folding events into the batch until the quiet period elapses (or a
  new `MAX_BATCH_EVENTS = 10_000` cap is hit as a safety valve against an
  unbounded batch during something like a big `git checkout`), then runs the
  exact same batch body as before (unchanged: dedup of buffer rels via a
  `HashSet` before calling `file_changed_externally`, so `is_self_write`'s
  token isn't consumed by the first of several raw events for one save;
  `catch_unwind` around the batch; one `TreeChanged`/`StatusChanged`
  broadcast per batch, not per event).
- `watch_tree` (both the Linux per-directory-non-recursive variant and the
  macOS/Windows single-recursive-watch variant) now takes
  `&mut notify::RecommendedWatcher` directly instead of the debouncer
  wrapper, using the `notify::Watcher` trait's `.watch()`.
- The `notify::RecommendedWatcher` is kept bound for the watcher thread's
  whole lifetime (`keep`/`_keep`, same pattern as before) — dropping it
  silently stops watching, same hazard the debouncer had.
- Everything else is untouched: recursive single watch on macOS/Windows,
  capped per-directory non-recursive walk + dynamic registration on Linux,
  `.git` watched deliberately for `index`/`HEAD`, pure `classify` routing,
  `watch_degraded` flag, `MAX_WATCHED_DIRS` cap.
- `Cargo.toml`: removed the `notify-debouncer-full` dependency entirely.
  `Cargo.lock` updated accordingly (23 lines removed, no other dependency
  changes).

## Regression test

Added `watch::tests::deleted_files_reach_the_ui_same_as_created_ones` at the
bottom of `src/watch.rs`. It exercises the real OS watcher end to end (not
just `classify`, since the bug lived in the debouncing layer `classify`
never sees): spins up a real `Hub` + `watch::spawn` against a temp
directory, subscribes to the hub's broadcast channel, writes a file and polls
for a `TreeChanged` broadcast, then deletes the file and polls for a second
`TreeChanged` broadcast. A helper `wait_for(rx, needle, deadline)` polls
`recv_timeout` against a wall-clock deadline instead of a fixed sleep, so the
test isn't tuned to one debounce/OS-latency guess. Ran it 3x in a row plus
inside the full suite — no flakes observed.

## Test command and full output

```
$ cargo test
```

```
warning: unused import: `Mode`
 --> src/workspace.rs:3:34
  |
3 | use crate::proto::{self, Intent, Mode, PaneId, Sizes, Tab, PANE_COUNT};
  |                                  ^^^^
  |
  = note: `#[warn(unused_imports)]` (part of `#[warn(unused)]`) on by default

warning: `deadlight` (lib) generated 1 warning (run `cargo fix --lib -p deadlight` to apply 1 suggestion)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.13s
     Running unittests src/lib.rs (target/debug/deps/deadlight-e686cd90144b0934)

running 102 tests
test config::tests::defaults_when_no_files ... ok
test config::tests::unknown_keys_are_ignored ... ok
test config::tests::malformed_file_warns_and_keeps_defaults ... ok
test fileops::tests::create_file_refuses_to_clobber ... ok
test config::tests::project_overrides_global_per_key ... ok
test fileops::tests::operations_are_confined ... ok
test fileops::tests::delete_is_non_recursive ... ok
test fileops::tests::force_overrides_the_conflict ... ok
test fileops::tests::save_is_confined ... ok
test fileops::tests::create_delete_rename_round_trip ... ok
test fileops::tests::create_file_refuses_a_symlink_that_escapes_the_project ... ok
test gitio::tests::parses_rename_lines ... ok
test gitio::tests::parses_ordinary_and_untracked_lines ... ok
test http::tests::parses_bare_path ... ok
test http::tests::parses_path_and_query ... ok
test http::tests::percent_roundtrip ... ok
test http::tests::rejects_non_get ... ok
test http::tests::respond_writes_status_and_headers ... ok
test fileops::tests::save_refuses_when_disk_changed_underneath ... ok
test gitio::tests::status_errors_outside_a_repo ... ok
test fileops::tests::save_preserves_file_mode ... ok
test fileops::tests::save_writes_when_the_base_hash_matches ... ok
test hub::tests::a_mutation_reaches_every_subscriber ... ok
test hub::tests::buffer_text_is_not_echoed_to_its_author ... ok
test hub::tests::closing_a_clean_file_tab_drops_its_buffer_but_a_dirty_one_survives ... ok
test hub::tests::closing_a_tab_still_referenced_elsewhere_keeps_the_buffer ... ok
test hub::tests::dropped_subscribers_are_pruned ... ok
test gitio::tests::diff_untracked_binary_file_errors ... ok
test gitio::tests::status_against_real_repo ... ok
test gitio::tests::diff_tracked_untracked_and_escape ... ok
test origin::tests::configured_origin_passes_exactly ... ok
test origin::tests::host_prefers_forwarded_then_falls_back ... ok
test origin::tests::host_rejects_rebinding_and_missing ... ok
test origin::tests::hostile_and_missing_origins_are_rejected ... ok
test origin::tests::loopback_origins_always_pass ... ok
test projects::tests::first_root_wins_on_duplicate_name ... ok
test projects::tests::lists_visible_unreserved_dirs ... ok
test projects::tests::read_text_file_policies ... ok
test projects::tests::resolve_rejects_bad_names ... ok
test projects::tests::roots_env_overrides_defaults ... ok
test projects::tests::safe_resolve_blocks_escapes ... ok
test projects::tests::safe_resolve_parent_allows_new_names_and_blocks_escapes ... ok
test projects::tests::safe_resolve_parent_confines_a_dot_dot_that_actually_canonicalizes ... ok
test proto::tests::decodes_move_and_terminal_tabs ... ok
test proto::tests::decodes_open_tab ... ok
test proto::tests::diff_tab_none_is_the_full_diff_entry ... ok
test proto::tests::encodes_events_with_tag ... ok
test proto::tests::malformed_input_is_an_error_not_a_panic ... ok
test render::tests::changes_and_status_fragments ... ok
test render::tests::diff_lines_are_classified ... ok
test render::tests::esc_escapes_html ... ok
test render::tests::file_fragment_md_vs_code ... ok
test render::tests::index_page_lists_projects ... ok
test render::tests::markdown_raw_html_is_neutralized ... ok
test render::tests::markdown_renders_wrapped ... ok
test render::tests::project_name_is_escaped_everywhere ... ok
test render::tests::tree_level_answers_a_lazy_dir_fetch ... ok
test render::tests::tree_level_at_empty_rel_matches_the_fragments_top_level ... ok
test render::tests::tree_marks_open_path_and_skips_hidden ... ok
test render::tests::tree_pre_expands_the_whole_open_path ... ok
test render::tests::tree_renders_one_level_and_closed_dirs_omit_children ... ok
test render::tests::workspace_page_wires_everything ... ok
test session::tests::default_command_wraps_dtach_with_no_ui ... ok
test session::tests::env_override_replaces_the_command ... ok
test session::tests::scrollback_ring_is_bounded ... ok
test session::tests::session_names_are_strictly_validated ... ok
test session::tests::smallest_attachment_geometry_wins ... ok
test watch::tests::bounded_walk_does_not_hit_the_cap_on_a_small_tree ... ok
test watch::tests::bounded_walk_honors_an_already_count_from_a_prior_call ... ok
test watch::tests::bounded_walk_stops_at_the_cap ... ok
test hub::tests::for_project_returns_promptly_on_a_large_tree ... ok
test watch::tests::git_index_and_head_drive_the_status_pane ... ok
test watch::tests::open_buffers_beat_the_generic_tree_class ... ok
test watch::tests::ordinary_files_refresh_the_tree ... ok
test watch::tests::other_git_internals_are_ignored ... ok
test watch::tests::self_writes_are_suppressed_once ... ok
test watch::tests::skip_dirs_and_hide_are_ignored_entirely ... ok
test hub::tests::rename_rekeys_the_buffer_and_the_open_tab_so_a_later_save_still_works ... ok
test workspace::tests::closing_the_active_tab_clamps_the_index ... ok
test workspace::tests::default_layout_matches_the_spec ... ok
test workspace::tests::edit_buffer_marks_dirty_and_caps_buffer_count ... ok
test workspace::tests::edit_buffer_rejects_oversize_text ... ok
test workspace::tests::move_tab_between_panes_preserves_the_tab ... ok
test workspace::tests::open_tab_appends_and_activates ... ok
test workspace::tests::opening_an_already_open_tab_focuses_it_instead_of_duplicating ... ok
test workspace::tests::out_of_range_intents_error_rather_than_panic ... ok
test workspace::tests::set_mode_rewrites_the_matching_file_tab ... ok
test workspace::tests::two_terminals_with_different_names_coexist ... ok
test workspace::tests::view_exposes_metadata_without_text ... ok
test hub::tests::renaming_a_directory_rewrites_buffers_and_tabs_for_the_whole_subtree ... ok
test hub::tests::save_conflict_is_reported_and_the_file_is_untouched ... ok
test hub::tests::set_mode_edit_does_not_clobber_an_already_dirty_buffer ... ok
test hub::tests::set_mode_edit_reads_the_file_so_the_first_save_does_not_conflict ... ok
test hub::tests::version_advances_on_change_only ... ok
test watch::tests::deleted_files_reach_the_ui_same_as_created_ones ... ok
test watch::tests::symlink_loop_does_not_hang_the_walk ... ok
test wsstate::tests::corrupt_file_yields_defaults_with_a_warning ... ok
test wsstate::tests::load_caps_restored_buffers_deterministically ... ok
test wsstate::tests::missing_file_yields_defaults_without_warning ... ok
test wsstate::tests::out_of_range_active_index_is_clamped_on_load ... ok
test wsstate::tests::round_trips_layout_and_buffers ... ok
test wsstate::tests::state_file_is_not_world_readable ... ok

test result: ok. 102 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.41s

     Running unittests src/main.rs (target/debug/deps/deadlight-93cf582679567256)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/integration.rs (target/debug/deps/integration-807504126b8eef47)

running 23 tests
test fragments_render_and_errors_become_hints ... ok
test diff_traversal_path_is_rejected_with_hint ... ok
test index_lists_projects ... ok
test http_rejects_rebinding_host ... ok
test theme_css_symlink_escaping_the_project_is_refused ... ok
test static_assets_served_with_type ... ok
test tree_dir_lazily_returns_a_subdirectorys_children ... ok
test tree_dir_traversal_is_rejected_with_hint_and_leaks_no_listing ... ok
test tree_dir_with_empty_rel_returns_the_root_listing ... ok
test unknown_pages_are_404 ... ok
test workspace_page_applies_project_settings ... ok
test external_edit_updates_a_clean_buffer_live ... ok
test invalid_session_name_is_refused ... ok
test reconnect_replays_buffer_text_for_open_edit_buffers ... ok
test set_mode_edit_then_save_writes_the_file ... ok
test terminal_ws_echoes_through_pty ... ok
test two_terminal_clients_mirror_one_session ... ok
test workspace_socket_malformed_json_is_reported_not_fatal ... ok
test workspace_socket_rejects_foreign_origin ... ok
test workspace_socket_rejects_missing_origin ... ok
test workspace_state_mirrors_between_two_clients ... ok
test ws_closes_when_child_exits_first ... ok
test ws_rejects_foreign_and_missing_origin ... ok

test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.35s

   Doc-tests deadlight

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

All 125 tests pass (102 lib + 23 integration), including the new
`watch::tests::deleted_files_reach_the_ui_same_as_created_ones` regression
test. `cargo build` and `cargo clippy --all-targets` produce no new warnings
in `src/watch.rs` (the pre-existing warnings shown are all in unrelated
files: `src/workspace.rs`, `src/wsconn.rs`, `tests/integration.rs`).

## Concerns / notes

- The regression test uses real filesystem + OS watch + wall-clock polling
  (deadline 10s per phase); it's slower than the pure `classify` unit tests
  but not sleep-based, and it ran clean 3x in a row locally plus inside the
  full suite.
- `MAX_BATCH_EVENTS = 10_000` is a new safety cap that didn't exist before
  (the debouncer had its own internal bookkeeping); it only matters for
  pathologically bursty changesets and does not change behavior for normal
  saves/deletes.
