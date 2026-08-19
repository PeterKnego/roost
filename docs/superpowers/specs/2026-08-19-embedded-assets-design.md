# resh — embedded web assets design

Makes the binary self-contained by compiling `static/` into it, and replaces the
single hardcoded asset path with a layered lookup whose layers differ in what
they are *allowed* to serve. Supersedes nothing; changes only how a `/static/…`
request is answered and generalises the existing per-project theme hook.

## The problem this solves

`routes.rs` resolves every asset against one compile-time constant:

```rust
pub const STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");
```

Nothing is embedded — there is no `include_bytes!` anywhere in `src/` — and
`serve_static` does a fresh `std::fs::read` per request. So the installed
binary is an executable *plus* a hardcoded absolute path to a source tree it
does not own, and it re-reads that tree on every request.

Three consequences, in increasing order of how much they hurt:

1. **`cargo install resh` cannot work.** A crates.io user's binary would point
   at whatever `CARGO_MANIFEST_DIR` was during their build — a registry cache
   directory that is not guaranteed to survive. The README's "a single Rust
   binary" is not true today.
2. **Moving or deleting `static/` half-breaks a running server.** The workspace
   HTML still renders while every `<script>` and `<link>` 404s, so the failure
   presents as an unstyled dead page rather than a clean error.
3. **Dogfooding resh on resh is hazardous.** The deployed service and the
   editing checkout are the same files, so a save to `static/app.js` reaches
   every browser on that host immediately, with no rebuild and no restart. A
   syntax error there means `render()` never runs — no tabs, no tree, no editor
   — so the tool cannot be used to undo the edit. The blast radius is every
   workspace on the box, not just the project being edited, because all of them
   load the same `/static/app.js`. `git checkout`, `pull` and `stash` count as
   edits for this purpose. Recovery requires ssh.

There is no service-worker asset cache to soften any of this: `sw.js` does
navigation and focus only.

## What changes, in one sentence

Assets are embedded at build time and served from the binary by default; two
narrowly-scoped filesystem layers may override them, and **what a layer may
override depends on the class of the asset**.

## Asset classes

Class is decided by the extension of the **requested path**, before any lookup:

| Class | Extensions |
|---|---|
| **theme** | `.css`, `.svg`, `.png`, `.jpg`, `.jpeg`, `.gif`, `.webp`, `.ico`, `.woff`, `.woff2`, `.ttf`, `.otf` |
| **code** | `.js`, `.html`, `.htm`, `.wasm`, **and every unrecognised extension** |

Unrecognised extensions falling to **code** is deliberate: adding a new
overridable format must be an explicit edit to this table, never a side effect
of someone dropping an unfamiliar file into a theme directory.

The split exists because the two classes carry different authority. JavaScript
served from `/static/` runs same-origin with every terminal websocket on that
host, so a party who can replace it can drive every shell the server owns. CSS
and images cannot. Only an operator may replace code; a project may restyle.

## Two routes, not one overlay

`/static/{rel}` carries **no project context** — it is a project-independent
route, so it cannot know whose theme to apply. Rather than thread a project
identity through it, per-project theming stays on the route that already exists:
`/frag/{proj}/theme.css` generalises from one file to a directory,
`/frag/{proj}/theme/{rel}`. That route already resolves a project, already
refuses symlinks escaping it, and already has tests.

Resolution order, first hit wins:

```
GET /static/{rel}
  1. $RESH_STATIC/{rel}            any class      (operator runtime switch)
  2. ~/.config/resh/static/{rel}   theme class only
  3. embedded                      any class      (always present)

GET /frag/{proj}/theme/{rel}
  1. {project}/.resh/theme/{rel}   theme class only
```

The class restriction on layers 2 and 3 is the whole enforcement mechanism: a
`.js` placed in the user directory or in a project's theme directory is not
"blocked" by a check that could be forgotten — those layers are simply never
consulted for a code-class path, so the request falls through to the embedded
copy. `RESH_STATIC` is the single runtime switch that can replace app logic,
and it is settable only by whoever starts the process.

A layer that does not exist is skipped, not an error: `~/.config/resh/static/`
is optional and absent on a fresh install, and a project without `.resh/theme/`
simply falls through. `RESH_STATIC` is the exception — it is an explicit
operator setting, so pointing it at a missing or unreadable directory logs a
warning once at startup rather than silently serving embedded assets and
leaving the operator to wonder why their edits do nothing.

A code-class path requested on `/frag/{proj}/theme/{rel}` is a 404, not a
fall-through to the embedded copy. That route answers only for a project's own
theme; serving it embedded content under a project URL would imply the project
supplied bytes it did not.

**A per-project `.resh/config.toml` must never influence any of this.** It
cannot name an asset directory, and it cannot set `RESH_STATIC`. This is the
same rule `allowed_origins` already follows, for the same reason: a hostile repo
that could point asset resolution at its own files would be injecting into the
workspace origin.

## Themes need no new mechanism

A theme is `themes/{name}.css` found by the normal lookup, selected by the
existing `theme = "…"` setting, which the workspace page already turns into a
`/static/themes/{name}.css` link. So dropping
`~/.config/resh/static/themes/solarized.css` and setting `theme = "solarized"`
works with no new route, no new config key, and no code aware of "themes" as a
concept.

A project stylesheet referencing `logo.png` resolves relatively to
`/frag/{proj}/theme/logo.png`, so project themes need no absolute paths.

**`style.css` is the project theme's entry point.** The workspace page links
`/frag/{proj}/theme/style.css` when `{project}/.resh/theme/style.css` exists,
exactly as it links `/frag/{proj}/theme.css` today when that file exists — the
existing `has_theme_css` check gains a sibling rather than changing shape. Every
other file in the directory is reachable but never linked automatically; the
stylesheet pulls in what it needs. Without this convention a directory of assets
would have no way to be loaded at all.

`{project}/.resh/theme.css` keeps working unchanged. Where both it and
`.resh/theme/style.css` exist, the directory wins and only its link is emitted —
never both, or a project would be styled by two stylesheets at once.

## Embedding

`build.rs` walks `static/`, emitting a path-sorted table into `OUT_DIR`:

```rust
pub static ASSETS: &[(&str, &[u8])] = &[
    ("app.js", include_bytes!(".../static/app.js")),
    // …sorted by path
];
```

`include!`d by an `assets` module; lookup is a binary search over the const
slice. It declares `cargo:rerun-if-changed=static` so an asset edit rebuilds.

**No new dependency.** `include_dir` and `rust-embed` each replace this build
script with one macro, but pull a proc-macro dependency chain into a tree that
today has seven dependencies and none. `rust-embed`'s headline feature is a
debug-mode filesystem fallback, which is precisely the overlay specified above
— paying for it twice. The cost of the heavier option lands on every
`cargo install` a crates.io user runs.

Size: `static/` is ~596K, dominated by vendored `xterm.js` and
`highlight.min.js`. `[profile.release] strip = true` is already set. The assets
are not compressed; that is a deliberate non-goal here.

## Path safety

The filesystem layers keep `serve_static`'s existing guard — `canonicalize()`
then `starts_with(base)` — applied independently per layer, since each has a
different base.

The embedded table has no filesystem to canonicalize against, so it needs its
own normalisation, applied before lookup: reject an empty path, an absolute
path, any backslash, and any `.` or `..` segment. A traversal attempt must 404
identically whether or not a filesystem layer happens to be configured, so that
behaviour cannot be used to probe which layers are active.

## Response headers

Every response served from a **non-embedded, non-`RESH_STATIC`** layer — that
is, the user directory and project theme directories — carries:

```
Content-Security-Policy: sandbox
```

An SVG is not a passive image: `serve_static` maps `.svg` to `image/svg+xml`,
and an SVG carrying `<script>` executes with full origin privileges when opened
as a top-level document. It is inert via `<img>` or CSS `url()`, but the URL is
guessable and directly navigable. `sandbox` places such a document in an opaque
origin, which neuters the script without banning the format. The header is
ignored when the same file is fetched as a subresource, so it costs nothing in
the normal path.

`X-Content-Type-Options: nosniff` is added to all static responses, embedded
included.

Embedded and `RESH_STATIC` responses need no sandbox: both are operator-supplied
by definition.

## Testing

- The generated table contains the known assets (`app.js`, `style.css`,
  `themes/darcula.css`) and their bytes match the files on disk.
- Traversal — `../Cargo.toml`, `..%2FCargo.toml`, absolute paths, backslash
  variants — 404s, and does so identically with and without a filesystem layer
  configured.
- `RESH_STATIC` overrides exactly one file while every other asset still
  resolves to the embedded copy.
- A `.js` in the user directory is ignored and the embedded copy is served;
  the same for a `.js` in a project theme directory.
- A project theme directory serves `.css` and `.png`, and those responses carry
  the CSP header while embedded responses do not.
- `.resh/theme/` takes precedence over `.resh/theme.css` when both exist, and
  `.resh/theme.css` alone still works.
- `cargo package --list` includes `static/`, guarding the publish path — a
  missing asset there would fail the build for a crates.io user rather than for
  us.

## Packaging and documentation

`Cargo.toml` gains an explicit `include` list covering `src/`, `static/`,
`build.rs` and the docs the README references. `static/` is not gitignored so it
packages by default today, but relying on that default means discovering a
`include_bytes!` build failure from a user's bug report.

`docs/deploy.md`'s install step no longer needs the checkout to remain in place,
and its claim that the unit depends on a source tree is removed. The README's
"a single Rust binary" becomes accurate.

## Non-goals

- Compressing or fingerprinting embedded assets.
- Caching headers or an ETag on static responses; the current absence of them
  is unchanged by this work.
- A UI for installing or switching themes. Selection remains the existing
  `theme = "…"` config key.
- Hot-reload of *embedded* assets. Development hot-reload is `RESH_STATIC`
  pointed at a checkout, which is the supported path and needs no watcher.
