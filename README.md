# Aichitect

Terminal-first AI document iteration tool for Markdown. Open a document, attach remarks to specific nodes, ask the AI for targeted revisions, and run a review pass that looks for ambiguity, contradictions, and missing implementation detail.

## Installation

```bash
cargo install --path .
```

## Setup

```bash
aichitect --init
# Edit ~/.aichitect/config.toml and add your OpenAI API key
```

Important config options:

- `api_key` - required
- `model` - defaults to `gpt-5.4` and is used for full-document analysis
- `model_fix` - defaults to `gpt-5.4-mini` and is used for document creation plus patch generation
- `base_url` - lets you point at OpenAI-compatible endpoints that support the Responses API
- `organization` / `project` - optional OpenAI headers
- `temperature` / `max_tokens` - forwarded to API requests when set
- `system_prompt_override` - replaces the default revision prompt
- `autosave` - saves the patched document automatically after AI changes apply

See `config.example.toml` for the full config shape.

## Usage

Open an existing Markdown file:

```bash
aichitect document.md
```

Or start a new document and let the AI draft the first version:

```bash
aichitect new-spec.md
```

## Key Bindings

| Key | Action |
|-----|--------|
| `Up`/`Down` | Select next/prev node or line |
| `Left` | Collapse heading / previous table column |
| `Right` | Expand heading / next table column |
| `Shift+Left` | Collapse all headings below cursor |
| `Shift+Right` | Expand all headings below cursor |
| `c` | Collapse or expand all headings |
| `PgUp`/`PgDn` | Page up/down |
| `Home`/`End` | Jump to top/bottom of the document |
| `e` | Edit the current block locally |
| `r` | Write a remark for the current selection |
| `R` | Toggle remarks side panel |
| `Shift+A` | Analyze document for issues |
| `H` | Browse patch history snapshots |
| `W` | Save document |
| `u`/`U` | Undo/Redo |
| `Ctrl+F` | Search within document |
| `Ctrl+C` | Copy current selection to clipboard |
| `Enter` | Follow link on selected node |
| `?` | Help overlay |
| `q` | Quit |

## Architecture

### Project Structure

```
src/
  main.rs              CLI entry point, config + document loading
  config/mod.rs        TOML configuration management
  document/
    mod.rs             Markdown parsing, rendering, patch application, undo/redo
    patch.rs           PatchOp enum and tests
    highlight.rs       Per-line syntax highlighting for 20+ languages
  openai/
    mod.rs             Module re-exports
    client.rs          OpenAI Responses API client
    session.rs         Per-document session state persistence
    prompts.rs         System prompts, request builders, response parsers
  remarks/mod.rs       Remark data model and store
  review/mod.rs        Review item model, categories, and store
  revision_context.rs  Targeted revision context builder
  history/mod.rs       On-disk revision history snapshots
  watcher.rs           File watcher for external changes (notify)
  tui/
    mod.rs             Terminal setup, event loop
    app.rs             Application state and business logic
    events.rs          Keyboard, mouse, and paste event handlers
    input.rs           Text input buffer with cursor and paste regions
    ui.rs              TUI rendering with Ratatui
```

### Data Flow

```
User Input
    |
    v
events.rs          Keyboard/mouse/paste dispatch
    |
    v
app.rs             State mutation + async AI requests
    |
    +--> document/mod.rs    Parsing, rendering, patching
    +--> openai/prompts.rs  Build request payloads
    +--> openai/client.rs   HTTP to OpenAI Responses API
    |
    v
AppEvent channel   Async results (patches, reviews, creation)
    |
    v
app.rs             Apply patches, save, refresh display
    |
    v
ui.rs              Render to terminal via Ratatui
```

### Document Model

The document is parsed from Markdown into a flat list of `DocNode` values. Each node gets a stable anchor ID (e.g. `h2-quick-start`, `p-0`, `cb-rust-1`, `li-3`) used to target AI patches.

Node types: `Heading`, `Paragraph`, `CodeBlock`, `ListItem`, `BlockQuote`, `Table`, `HorizontalRule`, `Html`.

The rendering pipeline converts nodes into `StyledLine` values with inline formatting (bold, italic, code, links), syntax highlighting for code blocks, and column-aware table rendering.

### Anchor-Based Patching

AI replies don't rewrite the whole file. They return JSON patch operations targeting specific anchors:

```json
{
  "patches": [
    {
      "op": "replace_section",
      "anchor": "p-0",
      "content": "Updated paragraph text.\n",
      "rationale": "Clarify requirements"
    }
  ]
}
```

Supported operations: `replace_section`, `replace_text_span`, `replace_code_block`, `insert_after`, `insert_before`, `delete_block`, `update_heading_text`, `update_list_item`.

Patches are applied from end-to-start to preserve byte offsets. A content snapshot taken before the request enables fingerprint-based fallback when anchors shift during concurrent editing.

### Save and History Guarantees

Every document mutation (AI patch, direct edit, document creation) saves the file to disk and writes a history snapshot to `~/.aichitect/history/<stem>/<timestamp>.md`. The undo stack only captures state when patches actually apply, preventing phantom undo entries.

## How the AI Calls Work

Aichitect sends all AI traffic through OpenAI's `POST /responses` API. Authentication uses your `api_key`, and `organization`, `project`, and `base_url` are added when configured.

Each document keeps lightweight OpenAI session state on disk (`~/.aichitect/sessions/`) so analysis turns and patch turns can chain with `previous_response_id` while the Markdown file in Aichitect remains the canonical source of truth.

There are three AI flows:

### 1. Document Creation

When you open a file that does not exist yet, Aichitect asks the model to generate raw Markdown. This flow expects plain Markdown back, not JSON, and the returned text becomes the entire document.

### 2. Revision from Remarks

When you submit a remark or accept/customize a review item, Aichitect sends one patch request at a time. Each request prefers a targeted revision context pack containing:

- the target anchor and selected text for each remark
- nearby section / sibling node context
- list or code-line context when relevant

If the targeted scope grows too large (>14k chars or >24 targets) or if there are more than 6 remarks, Aichitect falls back to a full-document request that includes the complete anchor map, full Markdown, and all submitted remarks.

Patch generation uses the smaller `model_fix` model with structured JSON output via OpenAI's JSON schema enforcement.

### 3. Review / Issue Finding

When you press `Shift+A`, Aichitect sends the full document plus anchor map to the stronger `model` and asks for structured review findings. The reply must be JSON with an `issues` array. Each issue targets an anchor, includes evidence from the document, explains why it matters, and suggests a concrete resolution.

Review categories: Ambiguity, Contradiction, Missing Acceptance Criteria, Undefined Term, Hidden Assumption, Missing Edge Case, Missing Operational Constraint, Unclear Ownership, Vague Success Metric, Missing Failure Behavior, Misleading Wording, Incomplete Code Example, Unspecified Input/Output.

## Review Mode

Press `Shift+A` to run the AI review pass. In review mode:

- `Up`/`Down` - navigate issues
- `a` - answer the suggested question for an issue
- `y` - accept the suggested resolution
- `d` - dismiss an issue
- `x` - clear cached analysis results
- `q`/`Esc` - exit review mode

## History Browser

Press `H` to open the history browser. Snapshots are automatically created after every document mutation (patches, direct edits, creation). Use `Up`/`Down` to browse, `Enter` to restore a snapshot, `q` to close.

History lives at `~/.aichitect/history/<document-stem>/`.

## CLI Flags

```bash
aichitect --init              # write sample config
aichitect --anchors file.md   # print anchor map and exit
```

## File Watcher

Aichitect watches the open document for external changes. When another editor or tool modifies the file on disk, Aichitect detects it within 500ms, merges the new content into the active session, and resets the AI session context so subsequent requests see the updated state.

During an active AI request the merge is deferred to avoid conflicts with in-flight patches.

## Dependencies

- **TUI**: ratatui + crossterm
- **Async**: tokio (multi-threaded runtime)
- **HTTP**: reqwest with JSON
- **Markdown**: pulldown-cmark
- **Data**: serde, serde_json, toml, uuid, chrono
- **File watching**: notify + notify-debouncer-mini
- **System**: arboard (clipboard), dirs, clap

## License

MIT
