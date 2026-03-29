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
| `↑`/`↓` | Select next/prev node or line |
| `c` | Collapse or expand all headings |
| `PgUp`/`PgDn` | Page up/down |
| `Home`/`End` | Jump to top/bottom of the document |
| `e` | Edit the current block locally |
| `r` | Write a remark for the current selection |
| `Shift+A` | Analyze document for issues |
| `H` | Browse patch history snapshots |
| `W` | Save document |
| `u`/`U` | Undo/Redo |
| `R` | Toggle remarks panel |
| `?` | Help |
| `q` | Quit |

## How the AI calls work

Aichitect sends all AI traffic through OpenAI's `POST /responses` API. Authentication uses your `api_key`, and `organization`, `project`, and `base_url` are added when configured.

Each document keeps lightweight OpenAI session state on disk so analysis turns and patch turns can chain with `previous_response_id` while the Markdown file in Aichitect remains the canonical source of truth.

There are three AI flows:

### 1. Document creation

When you open a file that does not exist yet, Aichitect asks the model to generate raw Markdown. This flow expects plain Markdown back, not JSON, and the returned text becomes the entire document.

### 2. Revision from remarks

When you submit a remark or accept/customize a review item, Aichitect sends one patch request at a time. Each request prefers a targeted revision context pack containing:

- the target anchor and selected text for each remark
- nearby section / sibling node context
- list or code-line context when relevant

If the targeted scope grows too large or looks ambiguous, Aichitect falls back to the older full-document request that includes:

- an anchor map for the parsed document
- the full Markdown document
- the submitted remark
- the selected text for that remark

Patch generation uses the smaller `model_fix` model and requires a structured JSON response:

```json
{
  "patches": [
    {
      "op": "replace_section",
      "anchor": "p-0",
      "content": "...",
      "rationale": "..."
    }
  ]
}
```

Supported patch operations include replacing sections or code blocks, inserting before/after a node, deleting a block, updating heading text, and updating list items.

### 3. Review / issue finding

When you press `Shift+A`, Aichitect sends the full document plus anchor map to the stronger `model` and asks for structured review findings. The reply must be JSON with an `issues` array. Each issue points at an anchor, includes evidence from the document, explains why it matters, and suggests a concrete resolution.

Those review items form a queue you can work down in the TUI. Each item can be accepted as-is or customized, after which it is converted into a localized patch request and sent back through the patch flow above.

## How replies patch the document

The document is parsed into anchored nodes such as headings, paragraphs, list items, block quotes, and code blocks. AI replies do not directly rewrite the whole file. Instead, they return patch operations that target specific anchors.

Before sending a revision request, Aichitect captures a per-anchor content snapshot. When the reply comes back, each patch is only applied if the current content at that anchor still matches the snapshot from request time. If the document changed in the meantime, that patch is skipped instead of blindly overwriting newer edits.

Patches are applied from the end of the file toward the beginning so byte offsets stay valid while edits are being inserted or replaced. After patching, the document is reparsed and the anchor map is rebuilt.

If at least one patch applied successfully, Aichitect also writes a snapshot to:

```text
~/.aichitect/history/<document-stem>/<timestamp>.md
```

That history is what the `H` browser shows, and it gives you an on-disk trail of AI-produced revisions. Applied patch changes are now also saved to the working document immediately after they succeed.

## Review Mode

Press `Shift+A` to run the AI review pass. In review mode:

- `↑`/`↓` - navigate issues
- `a` - answer the suggested question for an issue
- `y` - accept the suggested resolution
- `d` - dismiss an issue
- `x` - clear cached analysis results
- `q`/`Esc` - exit review mode

## CLI flags

```bash
aichitect --init              # write sample config
aichitect --anchors file.md   # print anchor map and exit
```
