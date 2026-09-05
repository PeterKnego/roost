# Settings dialog

*2026-09-05. Status: design, awaiting review.*

## What and why

roost is configured by two TOML files, `~/.config/roost/config.toml` and
`{project}/.roost/config.toml`, read fresh on every request. Every change is
a text edit and a reload; the header's gear button has said "not implemented
yet" since the chrome redesign. With 40 themes now available by name
(roost's five files and daisyUI's 35), picking one by editing a file and
guessing at the result is the first thing a person hits.

This adds a settings dialog behind the gear: one pane for the settings, one
for choosing a theme with live preview. Both write the config files through
the project's websocket, the way every other state change already travels.

Decided with Peter on 2026-09-05: display keys only, both scopes; theme
choice previews live and is kept by Save.

## Scope of keys

The scope of each key is not the dialog's decision. It is what `config.rs`
already enforces, and the dialog surfaces it rather than relaxing it.

| Key | Type | Default | Scope in the dialog | Takes effect |
|---|---|---|---|---|
| `theme` | theme name | `darcula` | project or global | live (this browser during preview; every browser on the project after Save) |
| `hide` | list of names | `[]` | project or global | next tree refresh |
| `show_hidden` | bool | `false` | project or global | next tree refresh; see *show_hidden and the header toggle* |
| `autosave` | bool | `true` | project or global | this page at once (`data-autosave` is re-read from the snapshot); other browsers on reload |
| `share_selection` | bool | `false` | global only | next selection |
| `worktree_prompt` | bool | `true` | global only | next ✻ click |
| `allowed_origins` | list | `[]` | read-only | — |
| `max_upload_bytes` | integer | 100 MB | read-only | — |
| `ide` | bool | `true` | read-only | — |
| `roots` | list of paths | from the unit | read-only | — |

Global-only keys are global-only because `config.rs` refuses them from a
project file: a cloned repository must not be able to set them. The dialog
offers them in the Global scope only; in the Project scope the row is shown
disabled with "global only".

Read-only keys are the ones a page must never be able to write.
`allowed_origins` protects the page itself; `roots` and `ide` decide what the
process reaches; `max_upload_bytes` is a disk ceiling. Their rows show the
value and the sentence "edit `~/.config/roost/config.toml`". There is no
intent that can write them, not a disabled control: the allowlist in the hub
(below) does not contain them, so a forged intent is refused too.

**`default_tab` is gone.** It was a v2 setting (which single view a project
opened in) that the four-pane client never read; `config.rs` and `render.rs`
carried it anyway. Removed on 2026-09-05 (commit `1fd6a6d`), before this
dialog, so it never has to be explained as a control that does nothing. Old
files that still set it load silently.

## The write

### Intent

One new intent:

```
SetSetting { scope: Scope, key: String, value: Option<SettingValue> }
Scope = Global | Project
SettingValue = Bool(bool) | Str(String) | List(Vec<String>)
```

`value: None` clears the key from that scope's file, so inheritance resumes
(project → global → default). This is how "use the global value" is
expressed; there is no separate intent.

### Validation, in the hub, before any file is touched

- `key` must be in `WRITABLE[scope]`, a fixed table in `config.rs`:
  Project = {theme, hide, show_hidden, autosave}; Global = Project ∪
  {share_selection, worktree_prompt}. Anything else → `Event::Error`, no
  write. The table is the single source of the dialog's scope column too
  (it is sent in the snapshot), so the UI and the refusal cannot disagree.
- `value` must match the key's type. A theme name must be either an
  embedded roost theme file or a `DAISY_THEMES` entry; a name the client
  invented is refused rather than written (a wrong name in the file renders
  the page unthemed until someone edits it by hand). `hide` entries must
  be single path components, no `/`, no `..`, non-empty.
- A `None` for a key that is absent is a no-op that still re-snapshots.

### The file edit

`toml_edit` (already in the dependency tree as `toml`'s own parser, promoted
to a direct dependency) parses the file into a `DocumentMut`, sets or removes
the top-level key, and serialises it back. Comments, ordering and formatting
of everything else survive. This is the whole reason for `toml_edit` over
re-serialising a `RawConfig`: a config file people edit by hand is exactly
the kind of file the *claudehooks* rule was written for.

- A file that does not parse is refused with the parse error, and left byte
  for byte alone. Rewriting a file we could not read is how a hand-edited
  one gets destroyed.
- A missing file is created, with only the one key. A missing `.roost/`
  directory is created for a project write.
- The write is atomic: a temp file with a pid-unique name in the same
  directory, then `rename`. Mode follows the existing file's, as the
  claudehooks backup does.
- The project file is edited under the project hub's lock, as
  `SetClaudeHooks` does — one small file whose new content is what every
  client is about to be sent. The global file is shared by every hub, so its
  edit additionally holds a process-wide `Mutex<()>` in `config.rs`. Neither
  lock is held across anything but that one write.

### After the write

The hub bumps the workspace version, invalidates nothing (config is never
cached — `load` runs per request), re-snapshots, and broadcasts to the
project. A global change reaches other projects' open tabs on their next
page load, not live: those hubs are not told, and telling them is out of
scope (see *Non-goals*).

## What the browser learns

`WorkspaceView` gains a `settings` block, built in `snapshot_event` from a
fresh `config::load` of both files plus the global-only readers:

```
settings: {
  keys: [ { key, kind: "bool"|"str"|"list", writable: ["project","global"] | ["global"] | [],
            effective, project: value|null, global: value|null, default,
            reload: bool } ],
  themes: [ { name, kind: "roost"|"daisy" } ],
  project_file: ".roost/config.toml",   // for the row hints
  global_file: "/home/…/.config/roost/config.toml"
}
```

`effective` is what `load` resolved; `project` and `global` are the raw
values in each file, or null when the key is absent there. That is enough
for a row to say "project: nord · global: dark · default: darcula" and to
know which scope's Clear applies.

The theme catalogue is roost's embedded `themes/*.css` names, then
`render::DAISY_THEMES` in daisyUI's own order. `render::theme_head` and
this list share one function, so the dialog cannot offer a name the page
would not resolve.

## The dialog

Opened by `#settings` (title becomes "settings"). A new `<dialog
id="dlg-settings" class="roost">` shell in `render.rs` beside the other
four, filled entirely from the snapshot with `textContent`/`createElement`,
never markup — a `hide` entry or a root path is attacker-influenced text.
`dialog.js` gains `openSettings()`, which uses `runDialog` for the modal
mechanics (one at a time, Escape, backdrop, focus restore) but keeps its own
state while open, because the dialog stays open across several intents and
snapshots.

Header: two tabs, **Settings** and **Theme**, and a scope switch **Project ·
Global** that applies to both panes. Footer: **Save**, **Cancel**. Enter
saves: nothing here destroys.

### Settings pane

One row per key in the table above, in that order. A row is: label, control
(checkbox for bool; text input for str; one-entry-per-line textarea for
list), a source hint ("from project" / "from global" / "default"), and
**Clear** when the key is set in the current scope. Global-only keys are
disabled in Project scope with "global only"; read-only keys are text with
the edit-by-hand sentence.

Save sends one `SetSetting` per row whose control differs from the current
scope's file value (a cleared key sends `None`). Cancel discards. While a
row's intent is in flight the dialog waits for the next snapshot before
re-rendering rows, so a broadcast from another browser cannot overwrite what
this one is still typing; the snapshot that follows this dialog's own Save
does re-render, which is how the source hints update.

### Theme pane

A grid of tiles, roost's five under a "roost" heading, daisyUI's 35 under
"daisyUI". Each tile is painted from its own theme: the roost ones from the
five embedded files' `--bg`/`--fg`/`--accent` (sent in the catalogue as
three colours), the daisyUI ones from `[data-theme=name]` — a tile carries
its own `data-theme` attribute and the vendored themes stylesheet is linked
while the dialog is open, so the browser resolves each tile's `--color-*`
itself. The selected tile is marked.

Clicking a tile previews: `applyTheme(name)` swaps the page's theme
`<link>` for a roost name, or sets `data-theme` on `<html>` and links the
vendored file and bridge for a daisyUI name, and undoes the other. This is
the same resolution as `render::theme_head`, expressed in the client. Save
sends `SetSetting { scope, "theme", name }`; the snapshot that follows
confirms it. Cancel, Escape or backdrop calls `applyTheme` with the name the
dialog opened with, so the page is as it was.

A second browser on the project sees the theme change through the
snapshot: `state.settings.keys.theme.effective` differing from what the page
was rendered with calls `applyTheme` there too. That is what makes the
theme live for everyone, not just the browser that saved.

### show_hidden and the header toggle

The tree's header toggle stores a per-workspace override that outranks the
file in both directions (`Hub::show_hidden`). Writing `show_hidden` from the
dialog also clears that override, so the file value is what the tree shows
next. Otherwise a person who set the file to `true` and sees no change has
no way to find out why.

### Project CSS

The structural lock (`DIALOG_STRUCTURAL_CSS`) gains the new shell's
classes: `.dlg-tabs`, `.dlg-scope`, `.dlg-rows`, `.dlg-row`, `.dlg-themes`,
`.dlg-tile`, on the same rule as `.dlg-body` — display and visibility
locked, colour free. Nothing here is destructive, so the ruling is only that
a theme cannot hide a row or relabel Save.

## Errors

`Event::Error` from a refused or failed write shows as a banner (existing
`showError`) and the dialog stays open with the person's values intact, so
a fix is one edit away. A parse error names the file and the line, straight
from `toml_edit`.

## Testing

Rust, in `config.rs`:

- editing a file with comments and unrelated keys preserves them byte for
  byte outside the changed key (revert-check: swapping `toml_edit` for
  `toml::to_string` fails on the comment);
- clearing removes the key and leaves an empty file valid;
- an unparsable file is refused with its error and is unchanged after
  (revert-check: dropping the parse-failure arm fails on the byte compare);
- the per-scope allowlist refuses `share_selection` for Project and
  `allowed_origins` for both, by name in the error;
- an invented theme name is refused.

Rust, in `hub.rs`: `SetSetting` writes the project file, the next snapshot
carries the new effective value and source, and the broadcast reaches both
of two subscribers (two, so `send_to` and `broadcast` are distinguishable).

Browser, `tests/browser/settings.mjs`:

- the gear opens the dialog, in-page, with the rows the snapshot describes;
- preview then Cancel: `--bg` after equals `--bg` before;
- preview then Save: the project file contains `theme = "nord"`, a second
  browser on the project repaints without reload;
- scope Global: the write lands in the fixture's `ROOST_CONFIG` file, not
  the project's;
- Clear on a project key removes it and the row's hint says "from global";
- a read-only row has no control, and a forged `SetSetting` for
  `allowed_origins` produces an error banner and no change in the file.

Each browser assertion is revert-checked against the code it covers before
it is trusted, per `tests/browser/README.md`.

## Non-goals

- Writing `allowed_origins`, `max_upload_bytes`, `ide`, `roots` from the
  browser, in any form.
- Live propagation of a global change to other projects' open tabs.
- Custom theme creation or editing; the user-directory theme files stay a
  by-hand feature.
- Per-theme favicons, the overview page's fixed theme.
- Any change to the notice panel's hook row.
