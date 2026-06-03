# web/ — dashboard front-end assets

The `runai server` dashboard is a single-page app with **no CDN and no build
step**. The Rust binary bundles every asset via `include_str!` so the dashboard
works fully offline (same single-binary philosophy as the rest of runai).

## Layout

```
web/
├── index.html        # shell; references /app.js and /app.css routes (do not rename)
├── css/              # stylesheet, split into safe-boundary parts
└── js/               # app script, split into safe-boundary parts
```

`web/app.js` and `web/app.css` used to be two monolithic files (~110 KB / ~77 KB).
They were decoupled into the part files under `css/` and `js/` so each concern is
editable in isolation. The server stitches them back together at compile time.

## Concat order (load-bearing — never reorder)

The lexical filename order **is** the concatenation order. The `NN-` numeric
prefix encodes it. `src/server/mod.rs` lists the parts explicitly in this same
order inside `concat!(include_str!(...), ...)`.

### `css/` (in order)

| # | File | Concern |
|---|---|---|
| 01 | `01-base-themes.css` | CSS reset + light/dark theme variables |
| 02 | `02-mesh-cursor.css` | Background mesh + custom cursor |
| 03 | `03-layout-topbar-sidebar.css` | App shell: topbar + sidebar layout |
| 04 | `04-overview-tab.css` | Overview tab |
| 05 | `05-library-dropdown.css` | Library dropdown |
| 06 | `06-skill-rows.css` | Skill list rows |
| 07 | `07-skill-detail.css` | Skill detail panel |
| 08 | `08-event-dialog.css` | Event / activity dialog |
| 09 | `09-settings-tab.css` | Settings tab |
| 10 | `10-v15-account-auth-library.css` | v15 account / auth / per-user library |
| 11 | `11-v15-market-github.css` | v15 market + GitHub install UI |

### `js/` (in order)

| # | File | Concern |
|---|---|---|
| 01 | `01-iife-state-formatters.js` | IIFE open + shared state + formatters |
| 02 | `02-router.js` | Client-side tab router |
| 03 | `03-api-overview.js` | `api()` wrapper + overview rendering |
| 04 | `04-activity-detail.js` | Activity feed + event detail |
| 05 | `05-library-skills.js` | Library skill list |
| 06 | `06-library-detail.js` | Library skill detail |
| 07 | `07-polling-models.js` | Polling + model selection |
| 08 | `08-dropdown-swatches-cursor.js` | Dropdowns, swatches, custom cursor |
| 09 | `09-wiring.js` | DOM event wiring |
| 10 | `10-settings.js` | Settings tab logic |
| 11 | `11-account-library.js` | Account pill + library bulk ops |
| 12 | `12-admin-scope-skills.js` | Admin scope + skill scoping |
| 13 | `13-auth-bulk-prefs.js` | Auth modals + bulk actions + prefs |
| 14 | `14-market-list.js` | Market list + pager |
| 15 | `15-market-detail-github.js` | Market detail + GitHub install |
| 16 | `16-boot.js` | Boot / IIFE close |

## Invariant: the served bundle stays byte-identical

`const APP_JS` / `const APP_CSS` in `src/server/mod.rs` are
`concat!(include_str!(...))` over the part files **in the sorted order above**.
`concat!` does plain byte concatenation with no separator, so the served
`/app.js` and `/app.css` routes are byte-for-byte the same stream the old
monolithic files produced.

**Rules for editing the parts:**

- **Cut only at safe boundaries.** A split point must fall between a complete
  CSS rule / complete JS statement — never mid-token, never inside a string,
  never between an open brace and its close. Each `js/` part is concatenated raw
  (no `;` or newline injected), so a part that ends mid-statement would fuse
  with the next part and break. Same for CSS: a part ending mid-rule corrupts
  the stylesheet.
- **Never reorder.** The JS relies on top-to-bottom execution (IIFE opened in
  `01`, closed in `16`); reordering changes runtime behavior. CSS later rules
  override earlier ones, so order is also load-bearing there.
- **Keep `mod.rs` in sync.** Adding / renaming / removing a part means editing
  the `concat!` list in `src/server/mod.rs` to match — there is no globbing.
- **Verify byte-identity after any structural change** (resplit, merge, move a
  boundary):

  ```sh
  # both must print nothing = identical to the last-known-good single file
  git show <ref>:web/app.css | cmp - <(cat web/css/*.css)
  git show <ref>:web/app.js  | cmp - <(cat web/js/*.js)
  ```

  When `web/app.{js,css}` no longer exist (post-split), compare against the
  commit that still had them, or against a freshly captured baseline.
