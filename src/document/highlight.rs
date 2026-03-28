//! Simple per-line syntax highlighter for code blocks.
//!
//! No external grammar files — keyword matching + token pattern scanning.
//! Unknown languages fall back to plain `CodeBlockLine` styling.

use super::SpanStyle;

/// Tokenize one line of `lang` code into (style, text) pairs covering the
/// entire line (including whitespace).
pub fn highlight_line(lang: &str, line: &str) -> Vec<(SpanStyle, String)> {
    if line.is_empty() {
        return vec![(SpanStyle::CodeBlockLine, String::new())];
    }

    let lang_owned = lang.trim().to_lowercase();
    let lang = lang_owned.as_str();

    if matches!(lang, "html" | "htm" | "xml" | "xhtml" | "svg" | "jsx" | "tsx") {
        return highlight_markup(lang, line);
    }

    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut raw: Vec<(SpanStyle, String)> = Vec::new();

    while i < n {
        // ── line comment ─────────────────────────────────────────────────────
        if let Some(pfx) = line_comment_prefix(lang) {
            let pc: Vec<char> = pfx.chars().collect();
            if chars[i..].starts_with(pc.as_slice()) {
                raw.push((SpanStyle::Comment, chars[i..].iter().collect()));
                break;
            }
        }

        // ── block comment opening /* (rest of line treated as comment) ───────
        if uses_block_comment(lang)
            && chars[i] == '/'
            && i + 1 < n
            && chars[i + 1] == '*'
        {
            raw.push((SpanStyle::Comment, chars[i..].iter().collect()));
            break;
        }

        let ch = chars[i];

        // ── whitespace ───────────────────────────────────────────────────────
        if ch == ' ' || ch == '\t' {
            let start = i;
            while i < n && (chars[i] == ' ' || chars[i] == '\t') {
                i += 1;
            }
            raw.push((SpanStyle::CodeBlockLine, chars[start..i].iter().collect()));
            continue;
        }

        // ── string literals ──────────────────────────────────────────────────
        if ch == '"' {
            i = lex_string(&chars, i, '"', &mut raw);
            continue;
        }
        if ch == '\'' {
            if lang == "rust" || lang == "rs" {
                i = lex_rust_char_or_lifetime(&chars, i, &mut raw);
            } else {
                i = lex_string(&chars, i, '\'', &mut raw);
            }
            continue;
        }
        if ch == '`'
            && matches!(lang, "javascript" | "js" | "typescript" | "ts" | "jsx" | "tsx")
        {
            i = lex_string(&chars, i, '`', &mut raw);
            continue;
        }

        // ── numbers ──────────────────────────────────────────────────────────
        if ch.is_ascii_digit()
            || (ch == '.' && i + 1 < n && chars[i + 1].is_ascii_digit())
        {
            let start = i;
            i = lex_number(&chars, i, lang);
            raw.push((SpanStyle::Number, chars[start..i].iter().collect()));
            continue;
        }

        // ── identifiers / keywords ───────────────────────────────────────────
        if ch.is_alphabetic() || ch == '_' {
            let start = i;
            while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let style = if keywords(lang).contains(&word.as_str()) {
                SpanStyle::Keyword
            } else {
                SpanStyle::CodeBlockLine
            };
            raw.push((style, word));
            continue;
        }

        // ── brackets ─────────────────────────────────────────────────────────
        if matches!(ch, '(' | ')' | '[' | ']' | '{' | '}') {
            raw.push((SpanStyle::Bracket, ch.to_string()));
            i += 1;
            continue;
        }

        // ── operators / punctuation ──────────────────────────────────────────
        raw.push((SpanStyle::Operator, ch.to_string()));
        i += 1;
    }

    merge(raw)
}

// ── token helpers ─────────────────────────────────────────────────────────────

fn lex_string(chars: &[char], start: usize, quote: char, out: &mut Vec<(SpanStyle, String)>) -> usize {
    let n = chars.len();
    let mut i = start + 1;
    while i < n {
        if chars[i] == '\\' && i + 1 < n {
            i += 2;
            continue;
        }
        if chars[i] == quote {
            i += 1;
            break;
        }
        i += 1;
    }
    out.push((SpanStyle::StringLit, chars[start..i].iter().collect()));
    i
}

fn lex_rust_char_or_lifetime(chars: &[char], start: usize, out: &mut Vec<(SpanStyle, String)>) -> usize {
    let n = chars.len();
    // Escaped char literal: '\n', '\\', '\x41', '\u{1F600}'
    if start + 1 < n && chars[start + 1] == '\\' {
        let mut i = start + 2;
        while i < n && chars[i] != '\'' { i += 1; }
        if i < n { i += 1; }
        out.push((SpanStyle::StringLit, chars[start..i].iter().collect()));
        return i;
    }
    // Single char: 'a'
    if start + 2 < n && chars[start + 2] == '\'' {
        out.push((SpanStyle::StringLit, chars[start..start + 3].iter().collect()));
        return start + 3;
    }
    // Lifetime: 'ident (no closing quote follows the identifier)
    if start + 1 < n && chars[start + 1].is_alphabetic() {
        let mut i = start + 1;
        while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
        out.push((SpanStyle::Operator, chars[start..i].iter().collect()));
        return i;
    }
    out.push((SpanStyle::Operator, "'".to_string()));
    start + 1
}

fn lex_number(chars: &[char], start: usize, lang: &str) -> usize {
    let n = chars.len();
    let mut i = start;
    // Prefixed literals: 0x, 0b, 0o
    if chars[i] == '0' && i + 1 < n {
        match chars[i + 1] {
            'x' | 'X' => {
                i += 2;
                while i < n && (chars[i].is_ascii_hexdigit() || chars[i] == '_') { i += 1; }
                if lang == "rust" || lang == "rs" {
                    while i < n && chars[i].is_alphabetic() { i += 1; }
                }
                return i;
            }
            'b' | 'B' => {
                i += 2;
                while i < n && (chars[i] == '0' || chars[i] == '1' || chars[i] == '_') { i += 1; }
                if lang == "rust" || lang == "rs" {
                    while i < n && chars[i].is_alphabetic() { i += 1; }
                }
                return i;
            }
            'o' | 'O' => {
                i += 2;
                while i < n && (chars[i].is_ascii_digit() || chars[i] == '_') { i += 1; }
                if lang == "rust" || lang == "rs" {
                    while i < n && chars[i].is_alphabetic() { i += 1; }
                }
                return i;
            }
            _ => {}
        }
    }
    // Decimal integer / float
    while i < n && (chars[i].is_ascii_digit() || chars[i] == '_') { i += 1; }
    // Fractional part
    if i < n && chars[i] == '.' && i + 1 < n && chars[i + 1].is_ascii_digit() {
        i += 1;
        while i < n && (chars[i].is_ascii_digit() || chars[i] == '_') { i += 1; }
    }
    // Exponent
    if i < n && (chars[i] == 'e' || chars[i] == 'E') {
        i += 1;
        if i < n && (chars[i] == '+' || chars[i] == '-') { i += 1; }
        while i < n && chars[i].is_ascii_digit() { i += 1; }
    }
    // Rust numeric suffixes: u8, i64, f32, usize …
    if lang == "rust" || lang == "rs" {
        while i < n && chars[i].is_alphanumeric() { i += 1; }
    }
    i
}

fn merge(raw: Vec<(SpanStyle, String)>) -> Vec<(SpanStyle, String)> {
    let mut out: Vec<(SpanStyle, String)> = Vec::new();
    for (style, text) in raw {
        match out.last_mut() {
            Some(last) if last.0 == style => last.1.push_str(&text),
            _ => out.push((style, text)),
        }
    }
    out
}

// ── language metadata ─────────────────────────────────────────────────────────

fn line_comment_prefix(lang: &str) -> Option<&'static str> {
    match lang {
        "rust" | "rs" | "java" | "c" | "cpp" | "c++" | "cc" | "cxx" | "h" | "hpp"
        | "javascript" | "js" | "typescript" | "ts" | "jsx" | "tsx"
        | "go" | "golang" | "dart" | "kotlin" | "kt" | "kts"
        | "swift" | "scala" | "groovy" | "v" | "zig" => Some("//"),

        "python" | "py" | "ruby" | "rb" | "shell" | "sh" | "bash" | "zsh" | "fish"
        | "yaml" | "yml" | "toml" | "r" | "perl" | "pl" | "nim"
        | "elixir" | "ex" | "exs" | "makefile" | "dockerfile" => Some("#"),

        "sql" | "postgres" | "mysql" | "sqlite" | "plpgsql"
        | "lua" | "haskell" | "hs" | "elm" | "ada" => Some("--"),

        "lisp" | "clojure" | "clj" | "scheme" | "racket" => Some(";"),

        _ => None,
    }
}

fn uses_block_comment(lang: &str) -> bool {
    matches!(
        lang,
        "rust" | "rs" | "java" | "c" | "cpp" | "c++" | "cc" | "cxx" | "h" | "hpp"
        | "javascript" | "js" | "typescript" | "ts" | "jsx" | "tsx"
        | "go" | "golang" | "dart" | "kotlin" | "kt" | "kts"
        | "swift" | "scala" | "css" | "scss" | "less" | "sass"
    )
}

fn keywords(lang: &str) -> &'static [&'static str] {
    match lang {
        "rust" | "rs" => &[
            "as", "async", "await", "box", "break", "const", "continue", "crate",
            "dyn", "else", "enum", "extern", "false", "fn", "for", "if", "impl",
            "in", "let", "loop", "macro_rules", "match", "mod", "move", "mut",
            "pub", "ref", "return", "self", "Self", "static", "struct", "super",
            "trait", "true", "type", "unsafe", "use", "where", "while",
            "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128",
            "isize", "str", "u8", "u16", "u32", "u64", "u128", "usize",
            "Err", "None", "Ok", "Option", "Result", "Some", "String", "Vec",
        ],
        "python" | "py" => &[
            "and", "as", "assert", "async", "await", "break", "class", "continue",
            "def", "del", "elif", "else", "except", "False", "finally", "for",
            "from", "global", "if", "import", "in", "is", "lambda", "None",
            "nonlocal", "not", "or", "pass", "raise", "return", "True", "try",
            "while", "with", "yield",
            "bool", "bytes", "dict", "float", "frozenset", "int", "list", "object",
            "set", "str", "tuple", "type",
            "len", "print", "range", "isinstance", "issubclass", "super", "self",
        ],
        "javascript" | "js" | "jsx" => &[
            "async", "await", "break", "case", "catch", "class", "const", "continue",
            "debugger", "default", "delete", "do", "else", "export", "extends",
            "false", "finally", "for", "from", "function", "if", "import", "in",
            "instanceof", "let", "new", "null", "of", "return", "static", "super",
            "switch", "this", "throw", "true", "try", "typeof", "undefined",
            "var", "void", "while", "with", "yield",
        ],
        "typescript" | "ts" | "tsx" => &[
            "abstract", "as", "async", "await", "break", "case", "catch", "class",
            "const", "continue", "declare", "default", "delete", "do", "else",
            "enum", "export", "extends", "false", "finally", "for", "from",
            "function", "if", "implements", "import", "in", "infer", "instanceof",
            "interface", "keyof", "let", "module", "namespace", "never", "new",
            "null", "of", "override", "private", "protected", "public", "readonly",
            "return", "satisfies", "static", "super", "switch", "this", "throw",
            "true", "try", "type", "typeof", "undefined", "unknown", "var", "void",
            "while", "with", "yield",
            "any", "boolean", "number", "object", "string", "symbol", "bigint",
        ],
        "go" | "golang" => &[
            "break", "case", "chan", "const", "continue", "default", "defer", "else",
            "fallthrough", "for", "func", "go", "goto", "if", "import", "interface",
            "map", "package", "range", "return", "select", "struct", "switch", "type",
            "var", "nil", "true", "false", "iota",
            "bool", "byte", "complex64", "complex128", "error", "float32", "float64",
            "int", "int8", "int16", "int32", "int64", "rune", "string",
            "uint", "uint8", "uint16", "uint32", "uint64", "uintptr",
            "append", "cap", "close", "copy", "delete", "len", "make", "new",
            "panic", "print", "println", "recover",
        ],
        "java" => &[
            "abstract", "assert", "boolean", "break", "byte", "case", "catch",
            "char", "class", "const", "continue", "default", "do", "double", "else",
            "enum", "extends", "false", "final", "finally", "float", "for", "goto",
            "if", "implements", "import", "instanceof", "int", "interface", "long",
            "native", "new", "null", "package", "private", "protected", "public",
            "return", "short", "static", "strictfp", "super", "switch",
            "synchronized", "this", "throw", "throws", "transient", "true", "try",
            "void", "volatile", "while",
        ],
        "kotlin" | "kt" | "kts" => &[
            "abstract", "actual", "as", "break", "by", "catch", "class", "companion",
            "const", "constructor", "continue", "crossinline", "data", "do", "else",
            "enum", "expect", "external", "false", "final", "finally", "for", "fun",
            "get", "if", "import", "in", "infix", "init", "inline", "inner",
            "interface", "internal", "is", "it", "lateinit", "noinline", "null",
            "object", "open", "operator", "out", "override", "package", "private",
            "protected", "public", "reified", "return", "sealed", "set", "super",
            "suspend", "tailrec", "this", "throw", "true", "try", "typealias",
            "val", "value", "var", "vararg", "when", "where", "while",
        ],
        "c" | "cpp" | "c++" | "cc" | "cxx" | "h" | "hpp" => &[
            "auto", "bool", "break", "case", "catch", "char", "class", "const",
            "const_cast", "continue", "default", "delete", "do", "double",
            "dynamic_cast", "else", "enum", "explicit", "extern", "false", "float",
            "for", "friend", "goto", "if", "inline", "int", "long", "mutable",
            "namespace", "new", "nullptr", "operator", "private", "protected",
            "public", "register", "reinterpret_cast", "return", "short", "signed",
            "sizeof", "static", "static_cast", "struct", "switch", "template",
            "this", "throw", "true", "try", "typedef", "typeid", "typename",
            "union", "unsigned", "using", "virtual", "void", "volatile", "while",
            "NULL", "include", "define", "ifdef", "ifndef", "endif", "pragma",
        ],
        "shell" | "sh" | "bash" | "zsh" | "fish" => &[
            "break", "case", "continue", "do", "done", "elif", "else", "esac",
            "exit", "export", "fi", "for", "function", "if", "in", "local",
            "readonly", "return", "select", "shift", "source", "then", "until",
            "while", "echo", "printf", "read", "true", "false", "declare", "unset",
        ],
        "sql" | "postgres" | "mysql" | "sqlite" | "plpgsql" => &[
            "ADD", "ALL", "ALTER", "AND", "AS", "ASC", "BEGIN", "BETWEEN", "BY",
            "CASE", "COMMIT", "CONSTRAINT", "COUNT", "CREATE", "CROSS", "DATABASE",
            "DEFAULT", "DELETE", "DESC", "DISTINCT", "DROP", "ELSE", "END", "EXCEPT",
            "EXISTS", "FALSE", "FROM", "FULL", "FUNCTION", "GROUP", "HAVING", "IF",
            "IN", "INDEX", "INNER", "INSERT", "INTO", "IS", "JOIN", "KEY", "LEFT",
            "LIKE", "LIMIT", "NOT", "NULL", "OFFSET", "ON", "OR", "ORDER", "OUTER",
            "PRIMARY", "PROCEDURE", "REFERENCES", "RIGHT", "ROLLBACK", "SCHEMA",
            "SELECT", "SET", "TABLE", "THEN", "TO", "TRANSACTION", "TRIGGER",
            "TRUE", "TRUNCATE", "UNION", "UNIQUE", "UPDATE", "USING", "VALUES",
            "VIEW", "WHEN", "WHERE", "WITH",
            "add", "all", "alter", "and", "as", "asc", "begin", "between", "by",
            "case", "commit", "count", "create", "cross", "database", "default",
            "delete", "desc", "distinct", "drop", "else", "end", "except", "exists",
            "false", "from", "full", "function", "group", "having", "if", "in",
            "index", "inner", "insert", "into", "is", "join", "key", "left", "like",
            "limit", "not", "null", "offset", "on", "or", "order", "outer", "primary",
            "procedure", "references", "right", "rollback", "schema", "select", "set",
            "table", "then", "to", "transaction", "trigger", "true", "truncate",
            "union", "unique", "update", "using", "values", "view", "when", "where",
            "with",
        ],
        "ruby" | "rb" => &[
            "alias", "and", "begin", "break", "case", "class", "def", "defined",
            "do", "else", "elsif", "end", "ensure", "false", "for", "if", "in",
            "module", "next", "nil", "not", "or", "redo", "rescue", "retry",
            "return", "self", "super", "then", "true", "undef", "unless", "until",
            "when", "while", "yield",
            "attr_accessor", "attr_reader", "attr_writer", "include", "extend",
            "require", "require_relative", "puts", "print", "raise",
        ],
        "swift" => &[
            "associatedtype", "class", "deinit", "enum", "extension", "fileprivate",
            "func", "import", "init", "inout", "internal", "let", "open", "operator",
            "precedencegroup", "private", "protocol", "public", "rethrows", "return",
            "static", "struct", "subscript", "typealias", "var", "break", "case",
            "continue", "default", "defer", "do", "else", "fallthrough", "for",
            "guard", "if", "in", "repeat", "switch", "throw", "try", "where",
            "while", "as", "catch", "false", "is", "nil", "super", "self", "Self",
            "throws", "true",
            "Bool", "Int", "Double", "Float", "String", "Character",
            "Array", "Dictionary", "Optional", "Any", "AnyObject", "Void",
        ],
        "lua" => &[
            "and", "break", "do", "else", "elseif", "end", "false", "for",
            "function", "goto", "if", "in", "local", "nil", "not", "or",
            "repeat", "return", "then", "true", "until", "while",
        ],
        "haskell" | "hs" => &[
            "as", "case", "class", "data", "default", "deriving", "do", "else",
            "forall", "foreign", "hiding", "if", "import", "in", "infix", "infixl",
            "infixr", "instance", "let", "module", "newtype", "of", "qualified",
            "then", "type", "where",
            "Bool", "Char", "Double", "Float", "IO", "Int", "Integer", "Maybe",
            "Ordering", "String", "False", "Just", "Left", "Nothing", "Right", "True",
        ],
        "elixir" | "ex" | "exs" => &[
            "after", "alias", "and", "case", "catch", "cond", "def", "defcallback",
            "defdelegate", "defexception", "defimpl", "defmacro", "defmacrop",
            "defmodule", "defoverridable", "defp", "defprotocol", "defstruct", "do",
            "else", "end", "fn", "for", "if", "import", "in", "not", "or", "quote",
            "raise", "receive", "require", "rescue", "try", "unless", "unquote",
            "use", "when", "with", "nil", "true", "false",
        ],
        "dart" => &[
            "abstract", "as", "assert", "async", "await", "base", "break", "case",
            "catch", "class", "const", "continue", "covariant", "default", "deferred",
            "do", "dynamic", "else", "enum", "export", "extends", "extension",
            "external", "factory", "false", "final", "finally", "for", "Function",
            "get", "hide", "if", "implements", "import", "in", "interface", "is",
            "late", "library", "mixin", "new", "null", "of", "on", "operator",
            "part", "required", "rethrow", "return", "sealed", "set", "show",
            "static", "super", "switch", "sync", "this", "throw", "true", "try",
            "type", "typedef", "var", "void", "when", "while", "with", "yield",
        ],
        "scala" => &[
            "abstract", "case", "catch", "class", "def", "do", "else", "extends",
            "false", "final", "finally", "for", "forSome", "if", "implicit",
            "import", "lazy", "match", "new", "null", "object", "override",
            "package", "private", "protected", "requires", "return", "sealed",
            "super", "this", "throw", "trait", "true", "try", "type", "val",
            "var", "while", "with", "yield",
        ],
        "php" => &[
            "abstract", "and", "array", "as", "break", "callable", "case", "catch",
            "class", "clone", "const", "continue", "declare", "default", "die", "do",
            "echo", "else", "elseif", "empty", "enum", "eval", "exit", "extends",
            "false", "final", "finally", "fn", "for", "foreach", "function",
            "global", "goto", "if", "implements", "include", "include_once",
            "instanceof", "insteadof", "interface", "isset", "list", "match",
            "namespace", "new", "null", "or", "print", "private", "protected",
            "public", "readonly", "require", "require_once", "return", "static",
            "switch", "throw", "trait", "true", "try", "unset", "use",
            "var", "while", "xor", "yield",
        ],
        "r" => &[
            "break", "else", "FALSE", "for", "function", "if", "in", "Inf",
            "NA", "NA_character_", "NA_complex_", "NA_integer_", "NA_real_",
            "NaN", "next", "NULL", "repeat", "return", "TRUE", "while",
        ],
        "zig" => &[
            "addrspace", "align", "allowzero", "and", "anyframe", "anytype", "asm",
            "async", "await", "break", "callconv", "catch", "comptime", "const",
            "continue", "defer", "else", "enum", "errdefer", "error", "export",
            "extern", "fn", "for", "if", "inline", "linksection", "noalias",
            "noinline", "nosuspend", "null", "opaque", "or", "orelse", "packed",
            "pub", "resume", "return", "struct", "suspend", "switch", "test",
            "threadlocal", "try", "undefined", "union", "unreachable", "usingnamespace",
            "var", "volatile", "while",
            "bool", "comptime_float", "comptime_int", "f16", "f32", "f64", "f80",
            "f128", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32",
            "u64", "u128", "usize", "void", "noreturn", "type", "anyerror",
        ],
        _ => &[],
    }
}

// ── HTML / XML / JSX markup highlighter ──────────────────────────────────────

/// Per-line tokenizer for HTML, XML, SVG, and JSX/TSX (tag portions).
/// For JSX/TSX, code outside tags is also highlighted with JS/TS keyword rules.
fn highlight_markup(lang: &str, line: &str) -> Vec<(SpanStyle, String)> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut raw: Vec<(SpanStyle, String)> = Vec::new();
    let is_jsx = matches!(lang, "jsx" | "tsx");

    while i < n {
        // HTML/XML comment: <!-- ... -->
        if i + 3 < n && chars[i] == '<' && chars[i+1] == '!' && chars[i+2] == '-' && chars[i+3] == '-' {
            let rest: String = chars[i..].iter().collect();
            if let Some(end_pos) = rest.find("-->") {
                raw.push((SpanStyle::Comment, rest[..end_pos + 3].to_string()));
                i += end_pos + 3;
            } else {
                raw.push((SpanStyle::Comment, rest));
                i = n;
            }
            continue;
        }

        // DOCTYPE / CDATA / processing instruction
        if i + 1 < n && chars[i] == '<' && chars[i+1] == '!' {
            let start = i;
            while i < n && chars[i] != '>' { i += 1; }
            if i < n { i += 1; }
            raw.push((SpanStyle::Dimmed, chars[start..i].iter().collect()));
            continue;
        }

        // JSX expression blocks: { ... } — highlight the brace but leave content to JS rules
        if is_jsx && chars[i] == '{' {
            raw.push((SpanStyle::Bracket, "{".to_string()));
            i += 1;
            // Collect until matching } or end of line
            let start = i;
            let mut depth = 1usize;
            while i < n {
                if chars[i] == '{' { depth += 1; }
                else if chars[i] == '}' {
                    depth -= 1;
                    if depth == 0 { break; }
                }
                i += 1;
            }
            if start < i {
                // Highlight the inner expression content with JS rules
                let inner: String = chars[start..i].iter().collect();
                let js_lang = if lang == "tsx" { "typescript" } else { "javascript" };
                let mut js_spans = highlight_line(js_lang, &inner);
                raw.append(&mut js_spans);
            }
            if i < n {
                raw.push((SpanStyle::Bracket, "}".to_string()));
                i += 1;
            }
            continue;
        }

        // Tag: < tagname ...attributes... > or </tagname>
        if chars[i] == '<' {
            raw.push((SpanStyle::Bracket, "<".to_string()));
            i += 1;
            // Closing slash
            if i < n && chars[i] == '/' {
                raw.push((SpanStyle::Bracket, "/".to_string()));
                i += 1;
            }
            // Tag name (HTML/XML: letters/digits/hyphens/colons; JSX: also dots for namespaced components)
            if i < n && (chars[i].is_alphabetic() || chars[i] == '_') {
                let start = i;
                while i < n && (chars[i].is_alphanumeric() || chars[i] == '-' || chars[i] == '_' || chars[i] == ':' || chars[i] == '.') {
                    i += 1;
                }
                raw.push((SpanStyle::Keyword, chars[start..i].iter().collect()));
            }
            // Attributes until > or />
            while i < n && chars[i] != '>' {
                if chars[i] == '/' && i + 1 < n && chars[i+1] == '>' {
                    raw.push((SpanStyle::Bracket, "/>".to_string()));
                    i += 2;
                    break;
                }
                if chars[i] == '"' {
                    i = lex_string(&chars, i, '"', &mut raw);
                } else if chars[i] == '\'' {
                    i = lex_string(&chars, i, '\'', &mut raw);
                } else if chars[i] == '=' {
                    raw.push((SpanStyle::Operator, "=".to_string()));
                    i += 1;
                } else if chars[i] == ' ' || chars[i] == '\t' {
                    let start = i;
                    while i < n && (chars[i] == ' ' || chars[i] == '\t') { i += 1; }
                    raw.push((SpanStyle::CodeBlockLine, chars[start..i].iter().collect()));
                } else if chars[i] == '{' && is_jsx {
                    // JSX expression in attribute value: attr={expr}
                    raw.push((SpanStyle::Bracket, "{".to_string()));
                    i += 1;
                    let start = i;
                    let mut depth = 1usize;
                    while i < n {
                        if chars[i] == '{' { depth += 1; }
                        else if chars[i] == '}' {
                            depth -= 1;
                            if depth == 0 { break; }
                        }
                        i += 1;
                    }
                    if start < i {
                        let inner: String = chars[start..i].iter().collect();
                        let js_lang = if lang == "tsx" { "typescript" } else { "javascript" };
                        let mut js_spans = highlight_line(js_lang, &inner);
                        raw.append(&mut js_spans);
                    }
                    if i < n {
                        raw.push((SpanStyle::Bracket, "}".to_string()));
                        i += 1;
                    }
                } else {
                    // Attribute name
                    let start = i;
                    while i < n && chars[i] != '=' && chars[i] != ' ' && chars[i] != '\t'
                        && chars[i] != '>' && !(chars[i] == '/' && i + 1 < n && chars[i+1] == '>')
                        && chars[i] != '"' && chars[i] != '\'' && chars[i] != '{' {
                        i += 1;
                    }
                    if i > start {
                        raw.push((SpanStyle::CodeBlockLine, chars[start..i].iter().collect()));
                    } else {
                        raw.push((SpanStyle::Operator, chars[i].to_string()));
                        i += 1;
                    }
                }
            }
            if i < n && chars[i] == '>' {
                raw.push((SpanStyle::Bracket, ">".to_string()));
                i += 1;
            }
            continue;
        }

        // Text content / JSX code outside tags
        if is_jsx {
            // In JSX, text between tags may be JS expressions — highlight with JS rules up to next <
            let start = i;
            while i < n && chars[i] != '<' && chars[i] != '{' { i += 1; }
            if i > start {
                let segment: String = chars[start..i].iter().collect();
                let js_lang = if lang == "tsx" { "typescript" } else { "javascript" };
                let mut js_spans = highlight_line(js_lang, &segment);
                raw.append(&mut js_spans);
            }
        } else {
            let start = i;
            while i < n && chars[i] != '<' { i += 1; }
            if i > start {
                raw.push((SpanStyle::CodeBlockLine, chars[start..i].iter().collect()));
            }
        }
    }

    merge(raw)
}
