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
- `model` - defaults to `gpt-4o`
- `base_url` - lets you point at OpenAI-compatible endpoints
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
| `j`/`k` or `↑`/`↓` | Scroll document |
| `J`/`K` | Select next/prev node |
| `PgUp`/`PgDn` | Page up/down |
| `r` | Add remark to selected node |
| `S` | Send queued remarks to AI |
| `A` | Analyze document for issues |
| `H` | Browse patch history snapshots |
| `W` | Save document |
| `u`/`U` | Undo/Redo |
| `p` | Toggle remarks panel |
| `?` | Help |
| `q` | Quit |

## How the AI calls work

Aichitect sends all AI traffic through OpenAI-compatible `POST /chat/completions` calls. Authentication uses your `api_key`, and `organization`, `project`, and `base_url` are added when configured.

There are three AI flows:

### 1. Document creation

When you open a file that does not exist yet, Aichitect asks the model to generate raw Markdown. This flow expects plain Markdown back, not JSON, and the returned text becomes the entire document.

### 2. Revision from remarks

When you press `S`, Aichitect builds one request containing:

- an anchor map for the parsed document
- the full Markdown document
- the queued remarks
- the selected text for each remark
- related occurrences when the same wording appears elsewhere

The revision system prompt requires the model to answer with JSON only:

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

When you press `A`, Aichitect sends the full document plus anchor map to the model and asks for structured review findings. The reply must be JSON with an `issues` array. Each issue points at an anchor, includes evidence from the document, explains why it matters, and suggests a fix.

Those review items can then be answered or accepted in the TUI, after which they are converted into regular remarks and sent back through the patch flow above.

## How replies patch the document

The document is parsed into anchored nodes such as headings, paragraphs, list items, block quotes, and code blocks. AI replies do not directly rewrite the whole file. Instead, they return patch operations that target specific anchors.

Before sending a revision request, Aichitect captures a per-anchor content snapshot. When the reply comes back, each patch is only applied if the current content at that anchor still matches the snapshot from request time. If the document changed in the meantime, that patch is skipped instead of blindly overwriting newer edits.

Patches are applied from the end of the file toward the beginning so byte offsets stay valid while edits are being inserted or replaced. After patching, the document is reparsed and the anchor map is rebuilt.

If at least one patch applied successfully, Aichitect also writes a snapshot to:

```text
~/.aichitect/history/<document-stem>/<timestamp>.md
```

That history is what the `H` browser shows, and it gives you an on-disk trail of AI-produced revisions. Applied changes stay in memory until you press `W`, unless `autosave = true` is enabled.

## Review Mode

Press `A` to run the AI review pass. In review mode:

- `j`/`k` - navigate issues
- `a` - answer the suggested question for an issue
- `d` - dismiss an issue
- `S` - send answered issues as remarks for AI revision
- `q`/`Esc` - exit review mode

## CLI flags

```bash
aichitect --init              # write sample config
aichitect --anchors file.md   # print anchor map and exit
```
