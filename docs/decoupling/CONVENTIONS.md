# runai — Module & Doc Conventions

> The reusable rules that keep this repo decoupled, skimmable, and friendly to both humans and AI
> agents. Apply these going forward to **every** module, not just the ones in the decoupling plan.
> Companion: [PLAN.md](PLAN.md) (the one-time split of the current monster files).

---

## 1. The size rule

| Threshold | Meaning |
|---|---:|
| **≤ 300 lines** | Comfortable. Default target for a single-responsibility file. |
| **≤ 500 lines** | Ideal ceiling. A file this big should own exactly one cohesive job. |
| **≤ 700 lines** | Hard ceiling. Crossing it is a signal to split, not a value judgment to ignore. |
| **> 700 lines** | A "monster file". Split it into a `foo/` directory (see §2). |

Tests don't get a free pass — a 1500-line `tests.rs` is a monster too; split it per-domain.

---

## 2. The `foo.rs` → `foo/` pattern

When a file outgrows the ceiling, convert `foo.rs` into a directory `foo/`:

```
foo/
  mod.rs        THIN. Declarations + re-exports + (rarely) a tiny shared type. NO business logic.
  aaa.rs        one cohesive responsibility
  bbb.rs        one cohesive responsibility
  ...
  tests.rs      OR per-submodule `#[cfg(test)] mod tests`
```

### `mod.rs` rules (non-negotiable)
- **≤ ~150 lines.** If it's longer, logic leaked in — move it out.
- Contains only: `mod x;` declarations, `pub use x::Item;` re-exports, and at most a couple of
  small shared types/consts that genuinely belong to no single submodule.
- **Re-exports MUST preserve every existing public path.** External code keeps doing
  `crate::core::foo::Bar` — it never learns the file split happened. This is the invariant that makes
  splits behavior-preserving and reviewable.

### Submodule rules
- One file = one responsibility you can name in a sentence without "and".
- Items used only within the module: `pub(super)` / `pub(crate)` / private. Don't widen visibility
  beyond what the public surface needs.
- Shared private helpers live in exactly **one** home submodule; siblings import via
  `use super::home::helper;`. Never copy-paste a helper into two files.

---

## 3. Visibility ladder

| Use it... | Make it... |
|---|---|
| outside the crate / re-exported from `mod.rs` as the public API | `pub` |
| anywhere in this crate but not the public API | `pub(crate)` |
| only within this module's submodules | `pub(super)` (from a submodule) |
| only within one file | private (no modifier) |

Default to the **narrowest** that compiles. Widening later is cheap; narrowing after callers
appear is a breaking change.

---

## 4. The `<name>.LLM.md` doc convention

Every non-trivial source file or module directory has a **sibling** `<name>.LLM.md`:
- `src/core/updater.rs` → `src/core/updater.LLM.md`
- `src/core/db/` (directory) → `src/core/db.LLM.md` (sibling of the dir, not inside it)

This keeps the "append `.LLM.md` to any path to find its doc" lookup working uniformly.

### Template
```markdown
# <module path> — LLM module guide

> One-liner: <what this module owns, ≤12 words>.

## Public surface (the API contract — external code depends on these exact paths)
- `crate::...::X` — <what>
- `crate::...::y()` — <what>

## Submodule map        (only if this is a directory module)
| File | Responsibility | Key items |
|---|---|---|
| `mod.rs` | re-exports only | `pub use ...` |
| `aaa.rs` | <one job> | `Foo`, `do_x()` |

## Invariants (load-bearing — do not break silently)
- <e.g. "row converters read columns positionally; SELECT order is load-bearing">

## Cross-module dependencies
- `crate::core::db::Database` — <why>

## Gotchas / where bodies are buried
- <thread::scope captures, !Sync handles, include_str! paths, macro coupling, unsafe blocks>

## Tests
- <what's covered, platform gating, fixtures>
```

### Doc maintenance rule (already an AGENTS.md hard rule — restated)
- Change a module's public API, behavior, invariant, or a gotcha → update its `*.LLM.md` **in the
  same commit**. Missing doc = half-finished work.
- Add a new module → add its `*.LLM.md` AND a row in the AGENTS.md "Module index" table, same commit.
- User-visible CLI flags / install steps → update **both** `README.md` and `README_zh.md`.

---

## 5. Where things live (repo layout contract)

```
src/
  cli/         clap subcommand surface + dispatch. Handlers in cli/handlers/.
  core/        business logic. Each concern is a module; big ones are dirs.
  mcp/         rmcp MCP server; tools in mcp/tools/ (one file per tool family).
  tui/         ratatui UI. app/ = state machine, ui/ = rendering, theme/i18n separate.
  server.rs    → server/ : axum HTTP dashboard, one file per route family.
web/
  index.html   load order: <link> CSS in <head>, <script> JS before </body>.
  css/         split by concern, loaded in cascade order.
  js/          split by view/concern; shared state via one namespace (no build step).
docs/
  decoupling/  this plan + these conventions.
  specs/       design docs & UI mocks.
```

- **One subcommand handler per file** under `cli/handlers/` once a handler exceeds ~150 lines.
- **One route family per file** under `server/` (auth, telemetry, skills, market, …).
- **One MCP tool family per file** under `mcp/tools/` (query, mutate, groups, market, …).
- **DB tables grouped by domain** under `core/db/` (router, resources, users, library, …); schema +
  migrations stay monolithic in `db/schema.rs`.

---

## 6. Behavior-preserving refactor checklist (use for every split)

1. **Move, don't rewrite.** No signature/logic/name changes during a split. Improvements are a
   separate, later commit.
2. **Public path frozen.** `grep -rn "crate::<module>::" src/` before and after → zero call-site edits.
3. **`include_str!`/`include_bytes!` paths** shift by one dir level when `foo.rs`→`foo/mod.rs`
   (`../x` → `../../x`). Grep and fix each.
4. **Tests move with their code**; preserve every `#[cfg(...)]` gate, `HOME_LOCK`, `with_home`, etc.
5. **Verify per module before moving on:**
   ```bash
   cargo build && cargo clippy --all-targets -- -W clippy::all && cargo test -- --test-threads=1
   ```
   Plus `safety_e2e` + `multiuser_owner_e2e` for anything touching `scanner/manager/paths/db`.
6. **One module per commit**, tagged `[refactor]`, compiles & passes standalone. No half-split trees.

---

## 7. Web assets without a build step

- Keep plain `<link>`/`<script>` tags — **no bundler, no `npm`, no transpile**.
- CSS: split by section banner, **preserve source order** (cascade is order-sensitive).
- JS: either (A) ordered `<script>` files sharing one `window.RUNAI` namespace, or (B) native ES
  modules (`<script type="module">` + `import`/`export`). Prefer A for pure moves, B for new shape.
- The server serves each file via a `.route()` + `include_str!` const; **extend the `serve_index`
  cache-bust replace to every asset URL** so `?v=<BUILD_ID>` is appended on each.
- Verify by loading the dashboard: themes, hash routing, market, and `?v=` on every asset in
  View-Source. There is no `cargo` gate for web — manual verification is the gate.

---

## 8. Anti-patterns (don't)

- ❌ Business logic in `mod.rs`.
- ❌ Splitting that forces consumers to change their `crate::...::X` paths.
- ❌ "While I'm here" logic tweaks during a mechanical move.
- ❌ A new module without its `*.LLM.md` and AGENTS.md index row.
- ❌ Splitting schema migrations across files, or separating positional row converters from their SELECTs.
- ❌ A web split that breaks the shared JS closure or forgets the cache-bust replace.
- ❌ Reporting "done" on a filesystem-touching module without the physical e2e gate (AGENTS.md 铁律 2).
