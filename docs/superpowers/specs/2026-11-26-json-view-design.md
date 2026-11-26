# JSON view

*2026-11-26. A `.json` file opens in Edit, as before. Its mode toggle now
also offers a view: the document indented and foldable, with nothing
written. A minified API dump or `package-lock.json` can be read without
being reformatted into a diff.*

## Shape

- **Still Edit by default.** JSON is mostly config, which people open to
  change. The client's `VIEW_TOGGLE_EXT` (`["json"]`) widens only which
  files get the 👁 toggle in Edit (`hasViewToggle`). It does not widen
  `RENDERED_EXT`, which decides the default mode. ✎ comes back from the view
  as from any Preview.
- **Fragment:** the `file` fragment's `is_json` arm reads the file through
  `read_text_file` (the 2 MB cap, the NUL sniff, confinement) and returns
  `render::json_fragment`. Edit never fetches this fragment, so the arm
  changes nothing about editing.
- **No writes anywhere.** There is no intent, and no buffer is touched.

## Exactly as written: `jsonview.rs`

The view must show what the file says. `serde_json::Value` changes it:
- a number past `f64` is rounded;
- `1e2` becomes `100.0`;
- a duplicate key loses its first value;
- `é` is decoded.

`arbitrary_precision` would fix numbers alone, and would change number
handling crate-wide, the protocol's untagged enums included.

So `jsonview::parse` is a small strict JSON parser that keeps every key and
scalar as the exact slice of the file. It only finds the structure.
- An error carries a message, a line, a column (counted in characters) and
  the offset, and the view shows the line with a caret under the column.
- Depth is bounded at 512: each level is a stack frame on a connection
  thread, and a 100 000-deep array overflowed the stack without the bound.
- A BOM is tolerated.

## Rendering

All of it is Rust, and every key and scalar goes through `esc`.
- **Objects and arrays** are `<details>`, so a node folds with no script.
  A folded node's summary says `N keys` or `N items`.
- **Empty containers** are one line, `{}` or `[]`.
- **Big documents.** Above 3000 values (`JSON_OPEN_ALL`) the document
  starts folded below its second level.
- **Expand all / collapse all** is one delegated click handler in `app.js`.
- **Phones.** Strings wrap anywhere, so a long value never scrolls the page
  sideways. The bar buttons are 44 px below 640 px.

## Tests

- **`jsonview.rs`:**
  - scalars and keys as written, including a probe showing what
    `serde_json` would have changed;
  - structure and counts;
  - errors with line and column, and a list of malformed inputs refused;
  - the depth bound (revert-checked: without it, the test run aborts on a
    stack overflow).
- **`routes.rs`:**
  - the fragment is the tree, keys and strings are escaped, the big number
    is kept, and the file is unchanged (revert-checked: without the arm,
    and without `esc` on keys);
  - invalid JSON names line 3, column 7 with a caret;
  - a 3000-value document opens two levels.
- **`tests/browser/json.mjs`:**
  - Edit offers 👁, and the tree shows every key in order;
  - a node folds, and collapse all and expand all work;
  - at 390 px nothing scrolls sideways and the buttons are 44 px;
  - ✎ goes back, and the file is byte-for-byte unchanged.

  Revert-checked three ways: the toggle list, the click handler, and the
  wrap rule.
