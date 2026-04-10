# Settings Editor Redesign

**Date:** 2026-04-10  
**Status:** Approved

## Summary

Rewrite the settings editor from a complex history-tracking panel to a simple, modern JSON editor with syntax highlighting, inline validation, and real-time error detection. Remove history/tags/revisions UI and backend endpoints. Keep mismatch detection.

## Decisions

| Decision | Choice |
|----------|--------|
| Backend scope | Strip history/tags/revisions endpoints. Keep mismatch detection + current/apply. |
| Editor type | Syntax-highlighted textarea (overlay technique), no external deps |
| Layout | Full-width single panel, toolbar at top |
| Error UX | Inline line highlighting + error banner with jump-to-line |

## Editor Architecture

### Visual Structure

```
┌─────────────────────────────────────────────────┐
│ CLAUDE SETTINGS.JSON   ~/.claude/settings.json  │  ← title bar
│                        [Format] [Reset] [Apply] │  ← toolbar buttons
├─────────────────────────────────────────────────┤
│ ⚠ settings.json on disk differs from last       │  ← mismatch callout
│   applied config     [Keep disk] [Keep DB]      │     (conditional, orange)
├─────────────────────────────────────────────────┤
│ ✕ Line 7: Expected ',' after property value     │  ← error banner
│                                   jump to line  │     (conditional, red)
├────┬────────────────────────────────────────────┤
│ 1  │ {                                          │  ← gutter + overlay editor
│ 2  │   "model": "claude-sonnet-4-20250514",     │
│ 3  │   "permissions": {                         │
│ 4  │     "allow": ["Bash(*)"],                  │
│ 5  │     "deny": []                             │
│ 6  │   },                                       │
│*7  │   "maxTokens": 8096                        │  ← error line (red bg)
│ 8  │   "apiKey": "sk-ant-..."                   │
│ 9  │ }                                          │
├────┴────────────────────────────────────────────┤
│ 9 lines • 234 bytes      Last applied: 2m ago   │  ← status bar
└─────────────────────────────────────────────────┘
```

### Overlay Technique

A `<textarea>` with transparent text sits on top of a `<pre>` that renders syntax-highlighted content. Both share identical font, size, padding, and scroll position.

```
┌─────────────────────────────────────┐
│ <div class="editor-container">      │
│   ┌───────────────────────────────┐ │
│   │ <pre class="editor-highlight">│ │ ← visible, colored, pointer-events:none
│   │   (tokenized HTML)            │ │
│   │ </pre>                        │ │
│   │ <textarea class="editor-input"│ │ ← on top, transparent text, receives input
│   │   (raw text)                  │ │
│   │ </textarea>                   │ │
│   └───────────────────────────────┘ │
│ </div>                              │
└─────────────────────────────────────┘
```

- Both elements: `position: absolute`, same dimensions
- Textarea: `color: transparent; caret-color: #e8e9f0; background: transparent`
- Pre: `pointer-events: none; white-space: pre-wrap`
- On input: tokenize textarea value → update pre innerHTML
- On scroll: sync `pre.scrollTop = textarea.scrollTop`

### Components

1. **Toolbar** — Title ("CLAUDE SETTINGS.JSON"), file path hint, buttons:
   - **Format**: Pretty-print JSON (2-space indent)
   - **Reset**: Reload from server (confirm if dirty)
   - **Apply**: Submit to server (disabled + muted when JSON invalid)

2. **Mismatch callout** (conditional) — Orange banner when disk differs from last-applied config
   - "Keep disk" / "Keep DB" resolution buttons
   - Hidden when no mismatch

3. **Error banner** (conditional) — Red banner with parsed error message
   - Shows line number + error description
   - "Jump to line" link scrolls editor and places cursor
   - Hidden when JSON is valid

4. **Editor area** — Gutter + overlay
   - Line number gutter, scroll-synced
   - Error line: red gutter number, red left border, red background tint
   - Textarea for input, pre for rendering

5. **Status bar** — Line count + byte size on left, "Last applied: X ago" on right

## Syntax Highlighting

Regex-based JSON tokenizer (~60 lines). Runs on every input to re-render the `<pre>`.

| Token | Color | Hex |
|-------|-------|-----|
| Keys | Blue | `#7aa2f7` |
| String values | Green | `#9ece6a` |
| Numbers | Orange | `#ff9e64` |
| Booleans / null | Purple | `#bb9af7` |
| Punctuation (brackets, braces, colons, commas) | Gray | `#a9adc1` |
| Error line | Red | `#f7768e` |

Key vs string: a quoted string followed by `:` is a key; all others are values.

## Error Detection

On every `input` event (debounced 300ms):

1. `JSON.parse(textarea.value)` in try/catch
2. If error: extract line number from error message position, highlight error line, show banner
3. If valid: hide banner, reset all line highlights
4. Apply button disabled when invalid (`opacity: 0.4; cursor: not-allowed; pointer-events: none`)

Error line marking: gutter number turns red, line in pre gets `background: rgba(247,118,142,0.1)` + `border-left: 3px solid #f7768e`.

## Data Flow

### Load (on tab activation)

1. `GET /api/settings/current`
2. Extract `data.claude_settings.raw_json`
3. `JSON.stringify(raw_json, null, 2)` → set textarea value
4. Render highlighted pre
5. Store as `lastAppliedContent`
6. Check `db_file_mismatch` flag → show/hide mismatch callout

### Apply (button click)

1. Validate: `JSON.parse(textarea.value)` — if invalid, abort
2. `POST /api/settings/apply` with `{ json: <parsed value> }`
3. On success: green toast "Settings applied successfully" (fades after 3s), update timestamp
4. On failure: red toast with server error
5. Update `lastAppliedContent`

### Mismatch Resolution

Same as current: "Keep disk" applies disk version, "Keep DB" restores DB version. Both trigger the apply flow.

## What Gets Removed

### Frontend HTML (~40 lines)

- `settings-history-panel` — entire sidebar
- History search input, clear-all button, history list
- Preview panel + load-into-editor button
- Tags input + hint
- `grid grid-2` wrapper on settings tab

### Frontend JS (~500 lines)

**State variables:** `settingsHistorySearchQuery`, `settingsHistoryRequestToken`, `selectedSettingsHistoryId`, `selectedSettingsPreviewId`, `settingsHistoryById`, `settingsApplyInFlight`

**Functions:** `loadSettingsHistory`, `renderSettingsHistory`, `loadSelectedSettingsIntoEditor`, `deleteSettingsHistoryRevision`, `clearAllSettingsHistory`, `renderSettingsHistoryTags`, `toggleSettingsHistoryTag`, history row render/select/preview/delete handlers, tag patch handlers

### Backend — Endpoints Removed

- `GET /api/settings/history` — history list with search/pagination
- `DELETE /api/settings/history/:id` — delete single revision
- `DELETE /api/settings/history` — clear all history
- `PATCH /api/settings/history/:id/tags` — tag management

### Backend — Logic Simplified

- `POST /api/settings/apply` — stop inserting into `settings_revision` and `settings_revision_tags` tables. Keep atomic file write + `settings_current` update.
- Remove `derive_settings_tags()` and tag-related functions
- Remove history query/search functions
- Leave tables in schema (no migration needed)

## What Gets Added (~350 lines)

### Frontend HTML (~100 lines)

New editor card structure: toolbar, mismatch callout, error banner, editor container (gutter + textarea + pre), status bar

### Frontend CSS (~50 lines)

Editor-specific styles: `.settings-editor-container`, `.settings-gutter`, `.settings-highlight`, `.settings-error-line`, `.settings-error-banner`, `.settings-mismatch-banner`, `.settings-status-bar`

### Frontend JS (~200 lines)

- `highlightJSON(text)` — tokenizer returning highlighted HTML
- `syncEditorScroll()` — keeps textarea/pre/gutter aligned
- `validateSettingsJSON()` — debounced error detection + banner management
- `applySettings()` — simplified apply (no tags)
- `formatSettings()` — pretty-print
- `resetSettings()` — reload from server with dirty confirmation
- `renderSettingsStatus()` — status bar updates
- `jumpToLine(lineNum)` — scroll + cursor placement

## Backend — Kept Unchanged

- `GET /api/settings/current` — full mismatch detection logic stays
- Atomic file writes
- `settings_current` singleton table
- Backup file creation (simplified: single `.bak` overwrite)

## Out of Scope

- Code folding, search/replace within editor
- Undo/redo beyond browser-native textarea behavior
- JSON schema validation (syntax only)
- Settings key autocomplete
- Multi-file editing
- Database schema migration
- Changes to other dashboard tabs
