# Aichitect

Terminal-first AI document iteration tool. Open a Markdown document, annotate nodes with remarks, send them to OpenAI for AI-powered surgical revisions, and run an ambiguity/contradiction review.

## Installation

```bash
cargo install --path .
```

## Setup

```bash
aichitect --init
# Edit ~/.aichitect/config.toml and add your OpenAI API key
```

## Usage

```bash
aichitect document.md
```

### Key Bindings

| Key | Action |
|-----|--------|
| `j`/`k` or `↑`/`↓` | Scroll document |
| `J`/`K` | Select next/prev node |
| `PgUp`/`PgDn` | Page up/down |
| `r` | Add remark to selected node |
| `S` | Send queued remarks to AI |
| `A` | Analyze document for issues (review mode) |
| `W` | Save document |
| `u`/`U` | Undo/Redo |
| `p` | Toggle remarks panel |
| `?` | Help |
| `q` | Quit |

### Review Mode

Press `A` to send the document to OpenAI for an ambiguity/contradiction review. In review mode:

- `j`/`k` — navigate issues
- `a` — answer the suggested question for an issue
- `d` — dismiss an issue
- `S` — send answered issues as remarks for AI revision
- `q`/`Esc` — exit review mode

### CLI flags

```bash
aichitect --init             # write sample config
aichitect --anchors file.md  # print anchor map and exit
```

## Configuration

See `config.example.toml` for all options.
# aichitect
