# Vendored front-end assets

Everything under `static/vendor/` is a published dist file, checked in rather
than fetched. Two reasons it has to be that way: `build.rs` bakes everything
under `static/` into the binary and the deployed service reads no files at
runtime (see [deploy.md](deploy.md)), and a CDN link would be a third party
able to run code in a page that holds a websocket to a shell.

This file lives in `docs/` rather than in `static/vendor/` for the same
build.rs reason — anything in that directory becomes bytes in the binary and a
URL on the server.

Nothing here is minified or bundled by this repo; there is no JS build step,
which is also what rules out any library that requires one. Update by fetching
the new dist over the old file and running the browser tests — they are the
only thing that exercises any of it.

| File | Package | Version | Licence |
|---|---|---|---|
| `xterm.js`, `xterm.css`, `xterm-addon-fit.js` | `xterm` | not recorded when vendored | MIT |
| `htmx.min.js` | `htmx.org` | 2.0.4 | 0BSD |
| `highlight.min.js`, `hljs-github-dark.min.css` | `highlight.js` | 11.9.0 | BSD-3-Clause |
| `github-markdown.min.css` | `github-markdown-css` | 5.3.3 | MIT |
| `code-input.min.js`, `code-input.min.css` | `@webcoder49/code-input` | 2.8.3 | MIT |
| file-type/folder icons (data URIs in `static/style.css`) | `material-extensions/vscode-material-icon-theme` | 5.38.0 | MIT |

## The file-type icons

The one vendored asset that is not a file under `static/vendor/`: the tree,
changes and tab icons are Material Icon Theme SVGs URL-encoded into CSS
variables in `static/style.css` (the `--ic-*` block). They ride inside the
stylesheet because that is how the hand-drawn set they replaced already
shipped — one request, no binary assets — and a 20-icon subset is ~11KB.
Update by fetching `icons/<name>.svg` from the tagged release and re-encoding
(swap `"`→`'`, then percent-encode `< > # % & { }`); the mapping from
extension to variable is the `[data-ext=…]` block directly below the icons.
The twisties and the branch/terminal/diff icons are not from this set — they
are this codebase's own stroke drawings and follow the theme.

## code-input

The newest of them, and the only custom element in the codebase. It is what
makes a code file's editor syntax-highlighted: a `<code-input>` wraps a real
`<textarea>` and paints a highlighted `<pre>` underneath it, driven by the
`highlight.js` already vendored above.

**That it keeps a real textarea is the whole reason it was chosen.** Every
other candidate — CodeJar, CodeMirror, Ace — replaces the textarea with
something of its own, and `editors`, the 200 ms edit debounce, autosave, ⌘S,
the conflict patch and the blur flush in `static/app.js` all talk to that
textarea directly. Two details of the integration are not obvious and are
commented at their sites:

- The element builds its *own* textarea on connect. `mountEditor` takes that
  one and registers it in `editors`; wiring a textarea handed *to* the element
  instead fails silently and totally — the text types and highlights, but no
  edit is ever sent and nothing is ever saved.
- Wrapping is set with `white-space` on the `<code-input>` element and nowhere
  else: both layers take it by `inherit`, which is what keeps them wrapping at
  the same column. Its own stylesheet also pins `word-wrap: normal` on both, so
  an unbroken token scrolls rather than breaking — together. Under wrapping,
  the highlighted layer's `width: max-content` has to be overridden or the
  editor grows to its longest line instead of wrapping.
- A highlight.js theme paints its own background over the element's. Their CSS
  docs say to beat it on specificity, which `static/style.css` does.
- Its `value` must be set after it is in the DOM. Before that, its setter
  falls back to assigning `innerHTML`, which parses a source file's own angle
  brackets as markup.

Its optional plugins (indentation, bracket closing, find-and-replace) are
separate files; none are vendored.
