# forge Phase 0-3 Slice Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a real `source string → lex → parse → resolve → typecheck → SSA IR → interpret` pipeline for the scalar subset of the `forge` expression language, with a day-one proof that this machine can execute hand-written JIT'd machine code.

**Architecture:** Two active crates in a 13-member Cargo workspace. `forge-syntax` owns lexing, a Pratt parser, an alpha-renaming resolve pass (fixes `let`-shadowing before anything else sees it), and type-checking. `forge-ir` owns the SSA IR types, a Braun-et-al SSA builder, AST→IR lowering, a dominance-based IR verifier, a textual printer, and the `interpret()` oracle. No codegen, no optimizer, no array/SIMD syntax in this slice.

**Tech Stack:** Rust 2021 (MSRV 1.80), `libc`/`nix` for the day-one mmap spike, `rustc-hash`/`smallvec` for IR data structures, `proptest` for one round-trip property test.

**Design doc:** `docs/superpowers/specs/2026-08-03-phase-0-3-slice-design.md`

---

## Task 1: Workspace scaffold

**Files:**
- Create: `Cargo.toml`
- Create: `crates/forge-syntax/Cargo.toml`
- Create: `crates/forge-ir/Cargo.toml`
- Create: `crates/forge-mem/Cargo.toml`
- Create: `crates/forge-opt/Cargo.toml`, `crates/forge-opt/src/lib.rs`
- Create: `crates/forge-regalloc/Cargo.toml`, `crates/forge-regalloc/src/lib.rs`
- Create: `crates/forge-x64/Cargo.toml`, `crates/forge-x64/src/lib.rs`
- Create: `crates/forge-aarch64/Cargo.toml`, `crates/forge-aarch64/src/lib.rs`
- Create: `crates/forge-wasm/Cargo.toml`, `crates/forge-wasm/src/lib.rs`
- Create: `crates/forge-runtime/Cargo.toml`, `crates/forge-runtime/src/lib.rs`
- Create: `crates/forge-simd/Cargo.toml`, `crates/forge-simd/src/lib.rs`
- Create: `crates/forge-bench/Cargo.toml`, `crates/forge-bench/src/lib.rs`
- Create: `crates/forge-cli/Cargo.toml`, `crates/forge-cli/src/main.rs`
- Create: `crates/forge-wasm-api/Cargo.toml`, `crates/forge-wasm-api/src/lib.rs`

- [ ] **Step 1: Create the workspace root `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = [
    "crates/forge-syntax",
    "crates/forge-ir",
    "crates/forge-mem",
    "crates/forge-opt",
    "crates/forge-regalloc",
    "crates/forge-x64",
    "crates/forge-aarch64",
    "crates/forge-wasm",
    "crates/forge-runtime",
    "crates/forge-simd",
    "crates/forge-bench",
    "crates/forge-cli",
    "crates/forge-wasm-api",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.80"

[workspace.dependencies]
libc = "0.2"
nix = { version = "0.29", features = ["mman"] }
region = "3"
raw-cpuid = "11"
smallvec = "1"
bitvec = "1"
rustc-hash = "2"
thiserror = "1"
anyhow = "1"
iced-x86 = "1.21"
capstone = "0.12"
criterion = "0.5"
proptest = "1"
```

- [ ] **Step 2: Create `crates/forge-syntax/Cargo.toml`**

```toml
[package]
name = "forge-syntax"
version.workspace = true
edition.workspace = true

[dependencies]
rustc-hash.workspace = true

[dev-dependencies]
proptest.workspace = true
```

- [ ] **Step 3: Create `crates/forge-ir/Cargo.toml`**

```toml
[package]
name = "forge-ir"
version.workspace = true
edition.workspace = true

[dependencies]
forge-syntax = { path = "../forge-syntax" }
rustc-hash.workspace = true
smallvec.workspace = true
```

- [ ] **Step 4: Create `crates/forge-mem/Cargo.toml`**

```toml
[package]
name = "forge-mem"
version.workspace = true
edition.workspace = true

[dependencies]
libc.workspace = true

[[example]]
name = "spike"
path = "examples/spike.rs"
```

- [ ] **Step 5: Create the 9 stub crates**

Run this to create every stub crate's `Cargo.toml` and `src/lib.rs` (or `src/main.rs` for `forge-cli`) in one pass:

```bash
for c in forge-opt forge-regalloc forge-x64 forge-aarch64 forge-wasm forge-runtime forge-simd forge-bench forge-wasm-api; do
  mkdir -p "crates/$c/src"
  cat > "crates/$c/Cargo.toml" <<EOF
[package]
name = "$c"
version.workspace = true
edition.workspace = true

[dependencies]
EOF
  cat > "crates/$c/src/lib.rs" <<EOF
//! Stub crate — not yet implemented. See CHECKLIST.md for this crate's phase.
EOF
done

mkdir -p crates/forge-cli/src
cat > crates/forge-cli/Cargo.toml <<'EOF'
[package]
name = "forge-cli"
version.workspace = true
edition.workspace = true

[dependencies]
EOF
cat > crates/forge-cli/src/main.rs <<'EOF'
//! Stub binary — not yet implemented. See CHECKLIST.md Phase 13.
fn main() {}
EOF
```

- [ ] **Step 6: Verify the workspace builds**

Run: `cargo check --workspace`
Expected: succeeds with 9 crates producing "unused" warnings only for the stub doc comments (none — doc comments don't warn). No errors.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/
git commit -m "chore: scaffold forge workspace with 13 member crates"
```

---

## Task 2: Day-one spike + entitlements + Makefile

**Files:**
- Create: `crates/forge-mem/examples/spike.rs`
- Create: `entitlements.plist`
- Create: `Makefile`

- [ ] **Step 1: Write the spike example**

```rust
// crates/forge-mem/examples/spike.rs

//! Day-one proof that this machine can allocate W^X memory, write real
//! machine code into it, and execute it. If this doesn't run, nothing else
//! in the project matters — fix the platform setup before writing another
//! line of forge.

fn main() {
    unsafe {
        let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        let mem = {
            // Apple Silicon: MAP_JIT is required, and so is the
            // com.apple.security.cs.allow-jit entitlement on a signed binary.
            let p = libc::mmap(
                std::ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_JIT,
                -1,
                0,
            ) as *mut u8;
            assert_ne!(p as isize, -1, "mmap MAP_JIT failed: {} — is the binary codesigned with entitlements.plist?", std::io::Error::last_os_error());
            p
        };

        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        let mem = {
            let p = libc::mmap(
                std::ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            ) as *mut u8;
            assert_ne!(p as isize, -1, "mmap failed: {}", std::io::Error::last_os_error());
            p
        };

        // mov rax, rdi   (48 89 F8)
        // ret            (C3)
        let code = [0x48u8, 0x89, 0xF8, 0xC3];

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            extern "C" {
                fn pthread_jit_write_protect_np(enabled: libc::c_int);
                fn sys_icache_invalidate(start: *mut libc::c_void, len: libc::size_t);
            }
            pthread_jit_write_protect_np(0);
            std::ptr::copy_nonoverlapping(code.as_ptr(), mem, code.len());
            pthread_jit_write_protect_np(1);
            sys_icache_invalidate(mem as *mut libc::c_void, page);
        }

        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            std::ptr::copy_nonoverlapping(code.as_ptr(), mem, code.len());
            assert_eq!(libc::mprotect(mem as _, page, libc::PROT_READ | libc::PROT_EXEC), 0,
                "mprotect failed: {}", std::io::Error::last_os_error());
        }

        let f: extern "C" fn(i64) -> i64 = std::mem::transmute(mem);
        assert_eq!(f(42), 42);
        println!("JIT works: f(42) = {}", f(42));
    }
}
```

- [ ] **Step 2: Run it and confirm it fails without codesigning (macOS arm64 only)**

Run: `cargo run --example spike -p forge-mem`
Expected on macOS arm64: mmap MAP_JIT fails with the assertion message pointing at codesigning. On Linux/macOS-x64: succeeds immediately, prints `JIT works: f(42) = 42` — skip to Step 5.

- [ ] **Step 3: Create `entitlements.plist`**

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>com.apple.security.cs.allow-jit</key><true/>
</dict></plist>
```

- [ ] **Step 4: Create the `Makefile`**

```makefile
.PHONY: test test-differential test-encoding bench qemu-aarch64 wasm workbench codesign spike

spike:
	cargo build --example spike -p forge-mem
	codesign --entitlements entitlements.plist -s - target/debug/examples/spike
	./target/debug/examples/spike

codesign:
	codesign --entitlements entitlements.plist -s - target/debug/examples/spike

test:
	cargo test --workspace

test-differential:
	@echo "not yet implemented — Phase 11"

test-encoding:
	@echo "not yet implemented — Phase 6"

bench:
	@echo "not yet implemented — Phase 16"

qemu-aarch64:
	@echo "not yet implemented — Phase 9"

wasm:
	@echo "not yet implemented — Phase 14"

workbench:
	@echo "not yet implemented — Phase 15"
```

- [ ] **Step 5: Run `make spike` and confirm success**

Run: `make spike`
Expected: prints `JIT works: f(42) = 42`.

- [ ] **Step 6: Commit**

```bash
git add crates/forge-mem/examples/spike.rs entitlements.plist Makefile
git commit -m "feat: day-one W^X spike — mmap, write, protect, execute"
```

---

## Task 3: Basic CI

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Write the workflow**

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

jobs:
  build-test:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
      - name: cargo build
        run: cargo build --workspace
      - name: cargo test
        run: cargo test --workspace
      - name: cargo clippy
        run: cargo clippy --workspace -- -D warnings
      - name: cargo fmt --check
        run: cargo fmt --check
      - name: codesign spike
        run: |
          cargo build --example spike -p forge-mem
          codesign --entitlements entitlements.plist -s - target/debug/examples/spike
      - name: run spike
        run: ./target/debug/examples/spike
```

- [ ] **Step 2: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: basic macOS build/test/clippy/fmt workflow"
```

---

## Task 4: forge-syntax — span, diagnostic, tokens, lexer

**Files:**
- Create: `crates/forge-syntax/src/lib.rs`
- Create: `crates/forge-syntax/src/span.rs`
- Create: `crates/forge-syntax/src/diagnostic.rs`
- Create: `crates/forge-syntax/src/token.rs`
- Create: `crates/forge-syntax/src/lexer.rs`
- Test: `crates/forge-syntax/src/lexer.rs` (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write `span.rs`**

```rust
// crates/forge-syntax/src/span.rs

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    pub fn join(self, other: Span) -> Span {
        Span { start: self.start.min(other.start), end: self.end.max(other.end) }
    }
}
```

- [ ] **Step 2: Write `diagnostic.rs`**

```rust
// crates/forge-syntax/src/diagnostic.rs

use crate::span::Span;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
    pub primary: Label,
    pub secondary: Vec<Label>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>, span: Span, label: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            primary: Label { span, message: label.into() },
            secondary: Vec::new(),
        }
    }

    pub fn with_secondary(mut self, span: Span, message: impl Into<String>) -> Self {
        self.secondary.push(Label { span, message: message.into() });
        self
    }
}
```

- [ ] **Step 3: Write `token.rs`**

```rust
// crates/forge-syntax/src/token.rs

use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Float, Int, Ident, True, False,
    If, Then, Else, Let, In,
    LParen, RParen, Comma, At, Assign,
    OrOr, AndAnd,
    Pipe, Caret, Amp,
    EqEq, NotEq,
    Lt, Le, Gt, Ge,
    Shl, Shr,
    Plus, Minus,
    Star, Slash, Percent,
    Bang, Tilde,
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// Literal text for Float/Int/Ident (with `_` separators stripped for
    /// numbers); empty for everything else.
    pub text: String,
}
```

- [ ] **Step 4: Write the lexer test module (failing first)**

```rust
// crates/forge-syntax/src/lexer.rs — append at the bottom

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let (tokens, diags) = lex(src);
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        tokens.into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn numbers() {
        let (tokens, diags) = lex("3.14159 42 1_000 6.02e23 1e-9");
        assert!(diags.is_empty());
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(tokens[0].kind, TokenKind::Float);
        assert_eq!(texts[0], "3.14159");
        assert_eq!(tokens[1].kind, TokenKind::Int);
        assert_eq!(texts[1], "42");
        assert_eq!(tokens[2].kind, TokenKind::Int);
        assert_eq!(texts[2], "1000");
        assert_eq!(tokens[3].kind, TokenKind::Float);
        assert_eq!(texts[3], "6.02e23");
        assert_eq!(tokens[4].kind, TokenKind::Float);
        assert_eq!(texts[4], "1e-9");
    }

    #[test]
    fn keywords_and_idents() {
        assert_eq!(
            kinds("if then else let in x true false"),
            vec![TokenKind::If, TokenKind::Then, TokenKind::Else, TokenKind::Let, TokenKind::In,
                 TokenKind::Ident, TokenKind::True, TokenKind::False, TokenKind::Eof]
        );
    }

    #[test]
    fn multi_char_operators_before_single_char() {
        assert_eq!(
            kinds("== != <= >= && || << >> ="),
            vec![TokenKind::EqEq, TokenKind::NotEq, TokenKind::Le, TokenKind::Ge,
                 TokenKind::AndAnd, TokenKind::OrOr, TokenKind::Shl, TokenKind::Shr,
                 TokenKind::Assign, TokenKind::Eof]
        );
    }

    #[test]
    fn bitwise_and_shift_tokens() {
        assert_eq!(
            kinds("& | ^ ~"),
            vec![TokenKind::Amp, TokenKind::Pipe, TokenKind::Caret, TokenKind::Tilde, TokenKind::Eof]
        );
    }

    #[test]
    fn unknown_char_produces_diagnostic_not_panic() {
        let (tokens, diags) = lex("1 $ 2");
        assert_eq!(diags.len(), 1);
        assert_eq!(tokens[0].kind, TokenKind::Int);
        assert_eq!(tokens[1].kind, TokenKind::Int); // lexer skips the bad char and continues
    }
}
```

- [ ] **Step 5: Run the tests to confirm they fail**

Run: `cargo test -p forge-syntax lexer:: 2>&1 | head -20`
Expected: FAIL — `lex` is not defined.

- [ ] **Step 6: Write the lexer implementation above the test module**

```rust
// crates/forge-syntax/src/lexer.rs — above the `#[cfg(test)]` module

use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::token::{Token, TokenKind};

pub fn lex(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut tokens = Vec::new();
    let mut diags = Vec::new();

    while i < bytes.len() {
        let start = i;
        let c = bytes[i] as char;

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        if c.is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') { i += 1; }
            let mut is_float = false;
            if i < bytes.len() && bytes[i] == b'.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                is_float = true;
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') { i += 1; }
            }
            if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
                let save = i;
                let mut j = i + 1;
                if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') { j += 1; }
                if j < bytes.len() && bytes[j].is_ascii_digit() {
                    is_float = true;
                    i = j;
                    while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
                } else {
                    i = save;
                }
            }
            let text: String = src[start..i].chars().filter(|&c| c != '_').collect();
            tokens.push(Token {
                kind: if is_float { TokenKind::Float } else { TokenKind::Int },
                span: Span::new(start as u32, i as u32),
                text,
            });
            continue;
        }

        if c.is_ascii_alphabetic() || c == '_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') { i += 1; }
            let text = &src[start..i];
            let kind = match text {
                "if" => TokenKind::If,
                "then" => TokenKind::Then,
                "else" => TokenKind::Else,
                "let" => TokenKind::Let,
                "in" => TokenKind::In,
                "true" => TokenKind::True,
                "false" => TokenKind::False,
                _ => TokenKind::Ident,
            };
            tokens.push(Token { kind, span: Span::new(start as u32, i as u32), text: text.to_string() });
            continue;
        }

        if i + 1 < bytes.len() {
            let two = &src[i..i + 2];
            let two_kind = match two {
                "==" => Some(TokenKind::EqEq), "!=" => Some(TokenKind::NotEq),
                "<=" => Some(TokenKind::Le),   ">=" => Some(TokenKind::Ge),
                "&&" => Some(TokenKind::AndAnd), "||" => Some(TokenKind::OrOr),
                "<<" => Some(TokenKind::Shl),  ">>" => Some(TokenKind::Shr),
                _ => None,
            };
            if let Some(kind) = two_kind {
                tokens.push(Token { kind, span: Span::new(start as u32, (start + 2) as u32), text: String::new() });
                i += 2;
                continue;
            }
        }

        let kind = match c {
            '(' => Some(TokenKind::LParen), ')' => Some(TokenKind::RParen),
            ',' => Some(TokenKind::Comma), '@' => Some(TokenKind::At), '=' => Some(TokenKind::Assign),
            '|' => Some(TokenKind::Pipe), '^' => Some(TokenKind::Caret), '&' => Some(TokenKind::Amp),
            '<' => Some(TokenKind::Lt), '>' => Some(TokenKind::Gt),
            '+' => Some(TokenKind::Plus), '-' => Some(TokenKind::Minus),
            '*' => Some(TokenKind::Star), '/' => Some(TokenKind::Slash), '%' => Some(TokenKind::Percent),
            '!' => Some(TokenKind::Bang), '~' => Some(TokenKind::Tilde),
            _ => None,
        };
        match kind {
            Some(k) => {
                tokens.push(Token { kind: k, span: Span::new(start as u32, (start + 1) as u32), text: String::new() });
                i += 1;
            }
            None => {
                diags.push(Diagnostic::error(
                    format!("unexpected character '{c}'"),
                    Span::new(start as u32, (start + 1) as u32),
                    "not a valid token",
                ));
                i += 1;
            }
        }
    }

    tokens.push(Token { kind: TokenKind::Eof, span: Span::new(bytes.len() as u32, bytes.len() as u32), text: String::new() });
    (tokens, diags)
}
```

- [ ] **Step 7: Write `lib.rs`**

```rust
// crates/forge-syntax/src/lib.rs

pub mod span;
pub mod diagnostic;
pub mod token;
pub mod lexer;
pub mod ast;
pub mod parser;
pub mod resolve;
pub mod typeck;

pub use diagnostic::Diagnostic;
pub use span::Span;
```

(`ast`, `parser`, `resolve`, `typeck` don't exist yet — Task 5/6/7 create them. This will not compile until those land; that's expected mid-task-list state.)

- [ ] **Step 8: Run the lexer tests and confirm they pass**

Run: `cargo test -p forge-syntax --lib lexer:: 2>&1 | tail -20`
Expected: all 5 lexer tests pass (the crate as a whole won't compile yet because of the `mod` declarations for not-yet-created files — that's fixed in Task 5).

- [ ] **Step 9: Commit**

```bash
git add crates/forge-syntax/src/span.rs crates/forge-syntax/src/diagnostic.rs crates/forge-syntax/src/token.rs crates/forge-syntax/src/lexer.rs crates/forge-syntax/src/lib.rs
git commit -m "feat(forge-syntax): span, diagnostic, tokens, hand-written lexer"
```

---

## Task 5: forge-syntax — AST and Pratt parser

**Files:**
- Create: `crates/forge-syntax/src/ast.rs`
- Create: `crates/forge-syntax/src/parser.rs`
- Test: `crates/forge-syntax/src/parser.rs` (inline)

- [ ] **Step 1: Write `ast.rs`**

```rust
// crates/forge-syntax/src/ast.rs

use crate::span::Span;
use std::marker::PhantomData;

pub struct Idx<T> {
    raw: u32,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Idx<T> {
    pub(crate) fn new(raw: u32) -> Self { Self { raw, _marker: PhantomData } }
    pub fn index(self) -> usize { self.raw as usize }
}

impl<T> Clone for Idx<T> { fn clone(&self) -> Self { *self } }
impl<T> Copy for Idx<T> {}
impl<T> PartialEq for Idx<T> { fn eq(&self, other: &Self) -> bool { self.raw == other.raw } }
impl<T> Eq for Idx<T> {}
impl<T> std::hash::Hash for Idx<T> { fn hash<H: std::hash::Hasher>(&self, state: &mut H) { self.raw.hash(state) } }
impl<T> std::fmt::Debug for Idx<T> { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { write!(f, "Idx({})", self.raw) } }

pub type ExprIdx = Idx<Expr>;

#[derive(Clone, Debug)]
pub enum Expr {
    Float(f64),
    Int(i64),
    Bool(bool),
    Ident(String),
    Unary { op: UnaryOp, operand: ExprIdx },
    Binary { op: BinaryOp, lhs: ExprIdx, rhs: ExprIdx },
    Call { callee: String, args: Vec<ExprIdx> },
    If { cond: ExprIdx, then_: ExprIdx, else_: ExprIdx },
    Let { name: String, value: ExprIdx, body: ExprIdx },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp { Neg, Not, BitNot }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add, Sub, Mul, Div, Rem,
    And, Or,
    BitAnd, BitOr, BitXor, Shl, Shr,
    Eq, Ne, Lt, Le, Gt, Ge,
}

#[derive(Clone)]
pub struct Ast {
    pub exprs: Vec<Expr>,
    pub spans: Vec<Span>,
    pub root: ExprIdx,
}

impl Ast {
    pub fn get(&self, idx: ExprIdx) -> &Expr { &self.exprs[idx.index()] }
    pub fn span(&self, idx: ExprIdx) -> Span { self.spans[idx.index()] }
}
```

- [ ] **Step 2: Write the parser test module (failing first)**

```rust
// crates/forge-syntax/src/parser.rs — append at the bottom

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn parse_ok(src: &str) -> Ast {
        let (tokens, diags) = lex(src);
        assert!(diags.is_empty(), "lex diagnostics: {diags:?}");
        let (ast, diags) = parse(&tokens);
        assert!(diags.is_empty(), "parse diagnostics: {diags:?}");
        ast
    }

    #[test]
    fn precedence_multiplicative_over_additive() {
        // 1 + 2 * 3 must parse as 1 + (2 * 3): root is Add, whose rhs is Mul.
        let ast = parse_ok("1 + 2 * 3");
        match ast.get(ast.root) {
            Expr::Binary { op: BinaryOp::Add, rhs, .. } => {
                assert!(matches!(ast.get(*rhs), Expr::Binary { op: BinaryOp::Mul, .. }));
            }
            other => panic!("expected top-level Add, got {other:?}"),
        }
    }

    #[test]
    fn left_associative_subtraction() {
        // 10 - 3 - 2 must parse as (10 - 3) - 2: root's lhs is a Binary.
        let ast = parse_ok("10 - 3 - 2");
        match ast.get(ast.root) {
            Expr::Binary { op: BinaryOp::Sub, lhs, .. } => {
                assert!(matches!(ast.get(*lhs), Expr::Binary { op: BinaryOp::Sub, .. }));
            }
            other => panic!("expected top-level Sub, got {other:?}"),
        }
    }

    #[test]
    fn unary_binds_tighter_than_multiplicative() {
        // -x * y must parse as (-x) * y.
        let ast = parse_ok("-x * y");
        match ast.get(ast.root) {
            Expr::Binary { op: BinaryOp::Mul, lhs, .. } => {
                assert!(matches!(ast.get(*lhs), Expr::Unary { op: UnaryOp::Neg, .. }));
            }
            other => panic!("expected top-level Mul, got {other:?}"),
        }
    }

    #[test]
    fn bitwise_shift_precedence_matches_spec_example() {
        // (n * 2654435761) >> 16 must parse with Shr at the root and Mul as its lhs.
        let ast = parse_ok("n * 2654435761 >> 16");
        match ast.get(ast.root) {
            Expr::Binary { op: BinaryOp::Shr, lhs, .. } => {
                assert!(matches!(ast.get(*lhs), Expr::Binary { op: BinaryOp::Mul, .. }));
            }
            other => panic!("expected top-level Shr, got {other:?}"),
        }
    }

    #[test]
    fn if_then_else_and_let_in() {
        let ast = parse_ok("let t = x * x in if t > 0.0 then sqrt(t) else 0.0");
        assert!(matches!(ast.get(ast.root), Expr::Let { .. }));
    }

    #[test]
    fn call_with_multiple_args() {
        let ast = parse_ok("max(a, b)");
        match ast.get(ast.root) {
            Expr::Call { callee, args } => { assert_eq!(callee, "max"); assert_eq!(args.len(), 2); }
            other => panic!("expected Call, got {other:?}"),
        }
    }
}
```

- [ ] **Step 3: Run the tests to confirm they fail**

Run: `cargo test -p forge-syntax --lib parser:: 2>&1 | head -20`
Expected: FAIL — `parse` is not defined.

- [ ] **Step 4: Write the parser implementation above the test module**

```rust
// crates/forge-syntax/src/parser.rs — above the `#[cfg(test)]` module

use crate::ast::{Ast, BinaryOp, Expr, ExprIdx, Idx, UnaryOp};
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::token::{Token, TokenKind};

pub fn parse(tokens: &[Token]) -> (Ast, Vec<Diagnostic>) {
    let mut p = Parser { tokens, pos: 0, exprs: Vec::new(), spans: Vec::new(), diags: Vec::new() };
    let root = p.parse_expr(0);
    if p.peek().kind != TokenKind::Eof {
        let tok = p.peek().clone();
        p.diags.push(Diagnostic::error(
            format!("unexpected trailing token {:?}", tok.kind), tok.span, "expected end of expression",
        ));
    }
    (Ast { exprs: p.exprs, spans: p.spans, root }, p.diags)
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    exprs: Vec<Expr>,
    spans: Vec<Span>,
    diags: Vec<Diagnostic>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Token { &self.tokens[self.pos] }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() { self.pos += 1; }
        t
    }

    fn expect(&mut self, kind: TokenKind) -> Token {
        if self.peek().kind == kind {
            self.advance()
        } else {
            let tok = self.peek().clone();
            self.diags.push(Diagnostic::error(
                format!("expected {kind:?}, found {:?}", tok.kind), tok.span, "unexpected token",
            ));
            tok
        }
    }

    fn push(&mut self, expr: Expr, span: Span) -> ExprIdx {
        self.exprs.push(expr);
        self.spans.push(span);
        Idx::new((self.exprs.len() - 1) as u32)
    }

    fn parse_expr(&mut self, min_bp: u8) -> ExprIdx {
        let mut lhs = self.parse_prefix();
        loop {
            let (op, l_bp, r_bp) = match self.infix_binding_power() {
                Some(t) => t,
                None => break,
            };
            if l_bp < min_bp { break; }
            self.advance();
            let rhs = self.parse_expr(r_bp);
            let span = self.spans[lhs.index()].join(self.spans[rhs.index()]);
            lhs = self.push(Expr::Binary { op, lhs, rhs }, span);
        }
        lhs
    }

    /// Precedence per SPEC §3, lowest to highest; each level is left-assoc
    /// via `(bp, bp + 1)`.
    fn infix_binding_power(&self) -> Option<(BinaryOp, u8, u8)> {
        use TokenKind::*;
        Some(match self.peek().kind {
            OrOr    => (BinaryOp::Or, 1, 2),
            AndAnd  => (BinaryOp::And, 3, 4),
            Pipe    => (BinaryOp::BitOr, 5, 6),
            Caret   => (BinaryOp::BitXor, 7, 8),
            Amp     => (BinaryOp::BitAnd, 9, 10),
            EqEq    => (BinaryOp::Eq, 11, 12),
            NotEq   => (BinaryOp::Ne, 11, 12),
            Lt      => (BinaryOp::Lt, 13, 14),
            Le      => (BinaryOp::Le, 13, 14),
            Gt      => (BinaryOp::Gt, 13, 14),
            Ge      => (BinaryOp::Ge, 13, 14),
            Shl     => (BinaryOp::Shl, 15, 16),
            Shr     => (BinaryOp::Shr, 15, 16),
            Plus    => (BinaryOp::Add, 17, 18),
            Minus   => (BinaryOp::Sub, 17, 18),
            Star    => (BinaryOp::Mul, 19, 20),
            Slash   => (BinaryOp::Div, 19, 20),
            Percent => (BinaryOp::Rem, 19, 20),
            _ => return None,
        })
    }

    /// Unary operators bind at 21 — tighter than every binary operator above.
    const UNARY_BP: u8 = 21;

    fn parse_prefix(&mut self) -> ExprIdx {
        let tok = self.advance();
        match tok.kind {
            TokenKind::Float => {
                let v: f64 = tok.text.parse().expect("lexer only produces valid float text");
                self.push(Expr::Float(v), tok.span)
            }
            TokenKind::Int => {
                let v: i64 = tok.text.parse().expect("lexer only produces valid int text");
                self.push(Expr::Int(v), tok.span)
            }
            TokenKind::True => self.push(Expr::Bool(true), tok.span),
            TokenKind::False => self.push(Expr::Bool(false), tok.span),
            TokenKind::Ident => {
                if self.peek().kind == TokenKind::LParen {
                    self.advance();
                    let mut args = Vec::new();
                    if self.peek().kind != TokenKind::RParen {
                        loop {
                            args.push(self.parse_expr(0));
                            if self.peek().kind == TokenKind::Comma { self.advance(); } else { break; }
                        }
                    }
                    let end = self.peek().span;
                    self.expect(TokenKind::RParen);
                    self.push(Expr::Call { callee: tok.text, args }, tok.span.join(end))
                } else {
                    self.push(Expr::Ident(tok.text), tok.span)
                }
            }
            TokenKind::LParen => {
                let inner = self.parse_expr(0);
                self.expect(TokenKind::RParen);
                inner
            }
            TokenKind::Minus => {
                let operand = self.parse_expr(Self::UNARY_BP);
                let span = tok.span.join(self.spans[operand.index()]);
                self.push(Expr::Unary { op: UnaryOp::Neg, operand }, span)
            }
            TokenKind::Bang => {
                let operand = self.parse_expr(Self::UNARY_BP);
                let span = tok.span.join(self.spans[operand.index()]);
                self.push(Expr::Unary { op: UnaryOp::Not, operand }, span)
            }
            TokenKind::Tilde => {
                let operand = self.parse_expr(Self::UNARY_BP);
                let span = tok.span.join(self.spans[operand.index()]);
                self.push(Expr::Unary { op: UnaryOp::BitNot, operand }, span)
            }
            TokenKind::If => {
                let cond = self.parse_expr(0);
                self.expect(TokenKind::Then);
                let then_ = self.parse_expr(0);
                self.expect(TokenKind::Else);
                let else_ = self.parse_expr(0);
                let span = tok.span.join(self.spans[else_.index()]);
                self.push(Expr::If { cond, then_, else_ }, span)
            }
            TokenKind::Let => {
                let name_tok = self.expect(TokenKind::Ident);
                self.expect(TokenKind::Assign);
                let value = self.parse_expr(0);
                self.expect(TokenKind::In);
                let body = self.parse_expr(0);
                let span = tok.span.join(self.spans[body.index()]);
                self.push(Expr::Let { name: name_tok.text, value, body }, span)
            }
            _ => {
                self.diags.push(Diagnostic::error(
                    format!("unexpected token {:?}", tok.kind), tok.span, "expected an expression",
                ));
                self.push(Expr::Float(0.0), tok.span)
            }
        }
    }
}
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-syntax --lib parser:: 2>&1 | tail -20`
Expected: 6 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/forge-syntax/src/ast.rs crates/forge-syntax/src/parser.rs
git commit -m "feat(forge-syntax): AST arena and Pratt parser"
```

---

## Task 6: forge-syntax — alpha-renaming resolve pass

**Why this exists:** `let`-bound names must not collide with parameter names or with each other under shadowing (e.g. `(let x = 1 in x) + x` — the trailing `x` must resolve to the outer parameter, not the let's `1`). Renaming every `let`-bound name to a name containing `%` (a character the lexer never produces in an identifier) makes every subsequent pass — type-checking, IR lowering — trivially shadow-free: a bare name is either always a parameter or always one specific local, never ambiguous. See the design doc's "Resolved ambiguities" section.

**Files:**
- Create: `crates/forge-syntax/src/resolve.rs`

- [ ] **Step 1: Write the resolve test module (failing first)**

```rust
// crates/forge-syntax/src/resolve.rs — append at the bottom

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{BinaryOp, Expr};
    use crate::lexer::lex;
    use crate::parser::parse;

    fn resolved(src: &str) -> Ast {
        let (tokens, _) = lex(src);
        let (ast, _) = parse(&tokens);
        resolve(ast)
    }

    #[test]
    fn let_bound_name_is_renamed() {
        let ast = resolved("let x = 1 in x + 2");
        match ast.get(ast.root) {
            Expr::Let { name, body, .. } => {
                assert!(name.contains('%'), "let-bound name should be renamed, got {name:?}");
                match ast.get(*body) {
                    Expr::Binary { lhs, .. } => {
                        let Expr::Ident(ident_name) = ast.get(*lhs) else { panic!("expected Ident") };
                        assert_eq!(ident_name, name);
                    }
                    other => panic!("expected Binary body, got {other:?}"),
                }
            }
            other => panic!("expected Let, got {other:?}"),
        }
    }

    #[test]
    fn outer_reference_after_let_is_not_renamed() {
        // (let x = 1 in x) + x — the trailing x is the free parameter,
        // untouched by the let's renaming.
        let ast = resolved("(let x = 1 in x) + x");
        match ast.get(ast.root) {
            Expr::Binary { op: BinaryOp::Add, rhs, .. } => {
                assert_eq!(ast.get(*rhs), &Expr::Ident("x".to_string()));
            }
            other => panic!("expected top-level Add, got {other:?}"),
        }
    }

    #[test]
    fn nested_shadowing_uses_distinct_names() {
        let ast = resolved("let x = 1 in let x = 2 in x");
        let Expr::Let { name: outer_name, body: outer_body, .. } = ast.get(ast.root) else { panic!() };
        let Expr::Let { name: inner_name, body: inner_body, .. } = ast.get(*outer_body) else { panic!() };
        assert_ne!(outer_name, inner_name);
        assert_eq!(ast.get(*inner_body), &Expr::Ident(inner_name.clone()));
    }
}
```

`Expr` needs `PartialEq` for these assertions — add it to the derive list.

- [ ] **Step 2: Add `PartialEq` to `Expr`'s derive in `ast.rs`**

```rust
// crates/forge-syntax/src/ast.rs — change this line:
#[derive(Clone, Debug)]
pub enum Expr {
```

to:

```rust
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
```

(`ExprIdx`/`Idx<T>` already derives `PartialEq` manually above, so this compiles once `resolve.rs` exists.)

- [ ] **Step 3: Run the tests to confirm they fail**

Run: `cargo test -p forge-syntax --lib resolve:: 2>&1 | head -20`
Expected: FAIL — `resolve` module/function not defined.

- [ ] **Step 4: Write the resolve implementation above the test module**

```rust
// crates/forge-syntax/src/resolve.rs — above the `#[cfg(test)]` module

use crate::ast::{Ast, Expr, ExprIdx};

/// Alpha-renames every `let`-bound name to a globally unique identifier
/// (`{name}%{counter}`). `%` never appears in a source-level identifier (the
/// lexer only accepts alphanumeric + `_`), so a renamed name can never
/// collide with a real parameter, and two different `let`s that happen to
/// reuse a name get distinct renamed forms. Everything downstream —
/// type-checking, IR lowering — can therefore treat a bare name as
/// unambiguous: always the same parameter, or always the same local.
pub fn resolve(mut ast: Ast) -> Ast {
    let mut counter = 0u32;
    let root = ast.root;
    rename(&mut ast, root, &mut Vec::new(), &mut counter);
    ast
}

fn rename(ast: &mut Ast, idx: ExprIdx, scope: &mut Vec<(String, String)>, counter: &mut u32) {
    match ast.exprs[idx.index()].clone() {
        Expr::Ident(name) => {
            if let Some((_, unique)) = scope.iter().rev().find(|(orig, _)| *orig == name) {
                ast.exprs[idx.index()] = Expr::Ident(unique.clone());
            }
        }
        Expr::Unary { operand, .. } => rename(ast, operand, scope, counter),
        Expr::Binary { lhs, rhs, .. } => {
            rename(ast, lhs, scope, counter);
            rename(ast, rhs, scope, counter);
        }
        Expr::Call { args, .. } => {
            for a in args { rename(ast, a, scope, counter); }
        }
        Expr::If { cond, then_, else_ } => {
            rename(ast, cond, scope, counter);
            rename(ast, then_, scope, counter);
            rename(ast, else_, scope, counter);
        }
        Expr::Let { name, value, body } => {
            rename(ast, value, scope, counter);
            *counter += 1;
            let unique = format!("{name}%{counter}");
            scope.push((name, unique.clone()));
            rename(ast, body, scope, counter);
            scope.pop();
            if let Expr::Let { name: n, .. } = &mut ast.exprs[idx.index()] {
                *n = unique;
            }
        }
        Expr::Float(_) | Expr::Int(_) | Expr::Bool(_) => {}
    }
}
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-syntax --lib resolve:: 2>&1 | tail -20`
Expected: 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/forge-syntax/src/resolve.rs crates/forge-syntax/src/ast.rs
git commit -m "feat(forge-syntax): alpha-rename let-bound names to fix shadowing"
```

---

## Task 7: forge-syntax — type checker

**Files:**
- Create: `crates/forge-syntax/src/typeck.rs`

- [ ] **Step 1: Write the type checker test module (failing first)**

```rust
// crates/forge-syntax/src/typeck.rs — append at the bottom

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::parse;
    use crate::resolve::resolve;

    fn typed(src: &str) -> Result<TypedAst, Vec<crate::diagnostic::Diagnostic>> {
        let (tokens, _) = lex(src);
        let (ast, _) = parse(&tokens);
        typecheck(resolve(ast))
    }

    #[test]
    fn int_plus_bool_is_a_type_error() {
        let err = typed("1 + true").unwrap_err();
        assert_eq!(err.len(), 1);
    }

    #[test]
    fn if_branch_type_mismatch() {
        let err = typed("if true then 1.0 else true").unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].message.contains("branch"));
    }

    #[test]
    fn intrinsic_arity_mismatch() {
        let err = typed("sqrt(1.0, 2.0)").unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].message.contains("takes"));
    }

    #[test]
    fn param_inferred_i64_through_nested_arithmetic() {
        // The canonical SPEC example: n is not a *direct* operand of `>>`,
        // it's wrapped in a Mul first — inference must propagate through it.
        let t = typed("(n * 2654435761) >> 16").unwrap();
        assert_eq!(t.params, vec![("n".to_string(), Ty::I64)]);
    }

    #[test]
    fn param_defaults_to_f64() {
        let t = typed("sqrt(x * x + y * y)").unwrap();
        assert_eq!(t.params, vec![("x".to_string(), Ty::F64), ("y".to_string(), Ty::F64)]);
    }

    #[test]
    fn let_shadowing_does_not_leak_into_outer_scope() {
        // (let x = 1 in x) + x — outer x stays a free f64 parameter even
        // though a let-local also happens to be named x.
        let t = typed("(let x = 1.0 in x) + x").unwrap();
        assert_eq!(t.params, vec![("x".to_string(), Ty::F64)]);
    }
}
```

- [ ] **Step 2: Run the tests to confirm they fail**

Run: `cargo test -p forge-syntax --lib typeck:: 2>&1 | head -20`
Expected: FAIL — `typecheck`/`TypedAst`/`Ty` not defined.

- [ ] **Step 3: Write the type checker implementation above the test module**

```rust
// crates/forge-syntax/src/typeck.rs — above the `#[cfg(test)]` module

use rustc_hash::FxHashMap;

use crate::ast::{Ast, BinaryOp, Expr, ExprIdx, UnaryOp};
use crate::diagnostic::Diagnostic;
use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty { F64, I64, Bool }

pub struct TypedAst {
    pub ast: Ast,
    pub types: Vec<Ty>,
    pub params: Vec<(String, Ty)>,
}

pub fn typecheck(ast: Ast) -> Result<TypedAst, Vec<Diagnostic>> {
    let mut ctx = Ctx {
        ast: &ast,
        param_ty: FxHashMap::default(),
        param_order: Vec::new(),
        local_ty: FxHashMap::default(),
        types: vec![Ty::F64; ast.exprs.len()],
        diags: Vec::new(),
    };
    ctx.infer_expect(ast.root, None);
    ctx.check(ast.root);
    let diags = std::mem::take(&mut ctx.diags);
    let types = std::mem::take(&mut ctx.types);
    let params = ctx.param_order.iter().map(|n| (n.clone(), ctx.param_ty[n])).collect();
    drop(ctx);
    if diags.is_empty() {
        Ok(TypedAst { ast, types, params })
    } else {
        Err(diags)
    }
}

struct Ctx<'a> {
    ast: &'a Ast,
    param_ty: FxHashMap<String, Ty>,
    param_order: Vec<String>,
    local_ty: FxHashMap<String, Ty>,
    types: Vec<Ty>,
    diags: Vec<Diagnostic>,
}

impl<'a> Ctx<'a> {
    fn note_param(&mut self, name: &str) {
        if !self.param_ty.contains_key(name) {
            self.param_ty.insert(name.to_string(), Ty::F64);
            self.param_order.push(name.to_string());
        }
    }

    fn constrain_param(&mut self, name: &str, ty: Ty) {
        self.note_param(name);
        self.param_ty.insert(name.to_string(), ty);
    }

    /// Pass 1: seed every free parameter's type from any unambiguous
    /// constraint, propagated down through type-preserving nodes (unary
    /// negate, arithmetic) so `(n * 2654435761) >> 16` forces `n` to i64
    /// even though `n` isn't a direct operand of `>>`. Names containing `%`
    /// are let-locals (see `resolve.rs`), not parameters, and are skipped —
    /// their type comes directly from their value expression in `check`.
    fn infer_expect(&mut self, idx: ExprIdx, expected: Option<Ty>) {
        match self.ast.get(idx).clone() {
            Expr::Ident(name) => {
                if name.contains('%') { return; }
                match expected {
                    Some(t) => self.constrain_param(&name, t),
                    None => self.note_param(&name),
                }
            }
            Expr::Unary { op, operand } => {
                let inner = match op {
                    UnaryOp::Neg => expected,
                    UnaryOp::BitNot => Some(Ty::I64),
                    UnaryOp::Not => Some(Ty::Bool),
                };
                self.infer_expect(operand, inner);
            }
            Expr::Binary { op, lhs, rhs } => {
                use BinaryOp::*;
                let inner = match op {
                    Add | Sub | Mul | Div | Rem => expected,
                    BitAnd | BitOr | BitXor | Shl | Shr => Some(Ty::I64),
                    And | Or => Some(Ty::Bool),
                    Eq | Ne | Lt | Le | Gt | Ge => None,
                };
                self.infer_expect(lhs, inner);
                self.infer_expect(rhs, inner);
            }
            Expr::Call { args, .. } => {
                for a in args { self.infer_expect(a, Some(Ty::F64)); }
            }
            Expr::If { cond, then_, else_ } => {
                self.infer_expect(cond, Some(Ty::Bool));
                self.infer_expect(then_, expected);
                self.infer_expect(else_, expected);
            }
            Expr::Let { value, body, .. } => {
                self.infer_expect(value, None);
                self.infer_expect(body, expected);
            }
            Expr::Float(_) | Expr::Int(_) | Expr::Bool(_) => {}
        }
    }

    /// Pass 2: real type-check against the parameter types pass 1 resolved.
    fn check(&mut self, idx: ExprIdx) -> Ty {
        let span = self.ast.span(idx);
        let ty = match self.ast.get(idx).clone() {
            Expr::Float(_) => Ty::F64,
            Expr::Int(_) => Ty::I64,
            Expr::Bool(_) => Ty::Bool,
            Expr::Ident(name) => {
                if let Some(t) = self.local_ty.get(&name) { *t }
                else { *self.param_ty.get(&name).expect("seeded in pass 1") }
            }
            Expr::Unary { op, operand } => {
                let t = self.check(operand);
                let ospan = self.ast.span(operand);
                match op {
                    UnaryOp::Neg => { self.expect_numeric(t, ospan); t }
                    UnaryOp::Not => { self.expect(t, Ty::Bool, ospan); Ty::Bool }
                    UnaryOp::BitNot => { self.expect(t, Ty::I64, ospan); Ty::I64 }
                }
            }
            Expr::Binary { op, lhs, rhs } => self.check_binary(op, lhs, rhs),
            Expr::Call { callee, args } => self.check_call(&callee, &args, span),
            Expr::If { cond, then_, else_ } => {
                let c = self.check(cond);
                self.expect(c, Ty::Bool, self.ast.span(cond));
                let t = self.check(then_);
                let e = self.check(else_);
                if t != e {
                    self.diags.push(
                        Diagnostic::error(
                            format!("if branches have different types: {t:?} vs {e:?}"), span, "branch type mismatch",
                        )
                        .with_secondary(self.ast.span(then_), format!("then branch is {t:?}"))
                        .with_secondary(self.ast.span(else_), format!("else branch is {e:?}")),
                    );
                }
                t
            }
            Expr::Let { name, value, body } => {
                let vt = self.check(value);
                self.local_ty.insert(name, vt);
                self.check(body)
            }
        };
        self.types[idx.index()] = ty;
        ty
    }

    fn check_binary(&mut self, op: BinaryOp, lhs: ExprIdx, rhs: ExprIdx) -> Ty {
        let lt = self.check(lhs);
        let rt = self.check(rhs);
        let (lspan, rspan) = (self.ast.span(lhs), self.ast.span(rhs));
        use BinaryOp::*;
        match op {
            Add | Sub | Mul | Div | Rem => { self.expect_numeric(lt, lspan); self.expect(rt, lt, rspan); lt }
            BitAnd | BitOr | BitXor | Shl | Shr => { self.expect(lt, Ty::I64, lspan); self.expect(rt, Ty::I64, rspan); Ty::I64 }
            And | Or => { self.expect(lt, Ty::Bool, lspan); self.expect(rt, Ty::Bool, rspan); Ty::Bool }
            Eq | Ne => { self.expect(rt, lt, rspan); Ty::Bool }
            Lt | Le | Gt | Ge => { self.expect_numeric(lt, lspan); self.expect(rt, lt, rspan); Ty::Bool }
        }
    }

    fn expect(&mut self, actual: Ty, expected: Ty, span: Span) {
        if actual != expected {
            self.diags.push(Diagnostic::error(format!("expected {expected:?}, found {actual:?}"), span, "type mismatch"));
        }
    }

    fn expect_numeric(&mut self, ty: Ty, span: Span) {
        if ty != Ty::F64 && ty != Ty::I64 {
            self.diags.push(Diagnostic::error(format!("expected a numeric type, found {ty:?}"), span, "not numeric"));
        }
    }

    fn check_call(&mut self, callee: &str, args: &[ExprIdx], span: Span) -> Ty {
        let sig: &[(&str, usize, Ty)] = &[
            ("sqrt", 1, Ty::F64), ("abs", 1, Ty::F64), ("floor", 1, Ty::F64), ("ceil", 1, Ty::F64),
            ("round", 1, Ty::F64), ("trunc", 1, Ty::F64), ("sin", 1, Ty::F64), ("cos", 1, Ty::F64),
            ("tan", 1, Ty::F64), ("exp", 1, Ty::F64), ("log", 1, Ty::F64),
            ("min", 2, Ty::F64), ("max", 2, Ty::F64), ("pow", 2, Ty::F64), ("fma", 3, Ty::F64),
        ];
        match sig.iter().find(|(name, _, _)| *name == callee) {
            Some((_, arity, ret)) => {
                if args.len() != *arity {
                    self.diags.push(Diagnostic::error(
                        format!("{callee}() takes {arity} argument(s), got {}", args.len()), span, "arity mismatch",
                    ));
                }
                for &a in args {
                    let t = self.check(a);
                    self.expect(t, Ty::F64, self.ast.span(a));
                }
                *ret
            }
            None => {
                self.diags.push(Diagnostic::error(format!("unknown intrinsic `{callee}`"), span, "not a known function"));
                for &a in args { self.check(a); }
                Ty::F64
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-syntax --lib typeck:: 2>&1 | tail -20`
Expected: 6 tests pass.

- [ ] **Step 5: Run the full forge-syntax test suite**

Run: `cargo test -p forge-syntax 2>&1 | tail -30`
Expected: all lexer, parser, resolve, typeck tests pass — `forge-syntax` is now feature-complete for this slice.

- [ ] **Step 6: Commit**

```bash
git add crates/forge-syntax/src/typeck.rs
git commit -m "feat(forge-syntax): type checker with usage-based parameter inference"
```

---

## Task 8: forge-ir — core IR types and RtValue

**Files:**
- Create: `crates/forge-ir/src/lib.rs`
- Create: `crates/forge-ir/src/ir.rs`

- [ ] **Step 1: Write `ir.rs`**

```rust
// crates/forge-ir/src/ir.rs

use smallvec::SmallVec;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Value(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Block(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ty { F64, I64, Bool }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CmpOp { Eq, Ne, Lt, Le, Gt, Ge }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LibFunc { Sin, Cos, Tan, Exp, Log, Pow }

#[derive(Clone, Debug)]
pub enum Inst {
    ConstF64(u64),
    ConstI64(i64),
    ConstBool(bool),
    Param { index: u32, ty: Ty },

    Add(Value, Value), Sub(Value, Value), Mul(Value, Value), Div(Value, Value), Rem(Value, Value),
    Neg(Value),

    // Two legitimate origins: the `fma()` intrinsic lowers here directly, and
    // (later) fast-math FMA contraction rewrites `a*b + c` into the same
    // instruction. Not constructed by contraction in this slice — no
    // optimizer yet — only by the parser via `fma()`.
    Fma { a: Value, b: Value, c: Value },

    // Also used to lower `&&`/`||`/`!` on bool operands — a 1-bit boolean is
    // representationally identical to i64's low bit for AND/OR/NOT, so no
    // separate logical-op instruction is needed.
    And(Value, Value), Or(Value, Value), Xor(Value, Value), Not(Value),
    Shl(Value, Value), Shr(Value, Value), Sar(Value, Value),

    Cmp { op: CmpOp, lhs: Value, rhs: Value },

    Sqrt(Value), Abs(Value), Min(Value, Value), Max(Value, Value),
    Floor(Value), Ceil(Value), Round(Value), Trunc(Value),

    Call { func: LibFunc, args: SmallVec<[Value; 2]> },

    IToF(Value), FToI(Value),

    Phi { incoming: SmallVec<[(Block, Value); 2]> },
}

#[derive(Clone, Debug)]
pub enum Terminator {
    Return(Value),
    Jump(Block),
    Branch { cond: Value, then_: Block, else_: Block },
}

#[derive(Clone, Debug, Default)]
pub struct BlockData {
    pub insts: Vec<Value>,
    pub term: Option<Terminator>,
    pub preds: SmallVec<[Block; 2]>,
}

pub struct Function {
    pub insts: Vec<Inst>,
    pub types: Vec<Ty>,
    pub spans: Vec<forge_syntax::span::Span>,
    pub blocks: Vec<BlockData>,
    pub entry: Block,
    pub params: Vec<(String, Ty)>,
}

/// Every use-operand of an instruction, for the verifier and for
/// `replace_all_uses`. `Inst` is deliberately not `Copy`/matched elsewhere
/// with a catch-all — a new variant must be added here explicitly, or the
/// verifier silently stops checking its operands.
pub fn uses_of(inst: &Inst) -> Vec<Value> {
    match inst {
        Inst::Add(a, b) | Inst::Sub(a, b) | Inst::Mul(a, b) | Inst::Div(a, b) | Inst::Rem(a, b)
        | Inst::And(a, b) | Inst::Or(a, b) | Inst::Xor(a, b)
        | Inst::Shl(a, b) | Inst::Shr(a, b) | Inst::Sar(a, b)
        | Inst::Min(a, b) | Inst::Max(a, b) => vec![*a, *b],
        Inst::Neg(a) | Inst::Not(a) | Inst::Sqrt(a) | Inst::Abs(a)
        | Inst::Floor(a) | Inst::Ceil(a) | Inst::Round(a) | Inst::Trunc(a)
        | Inst::IToF(a) | Inst::FToI(a) => vec![*a],
        Inst::Fma { a, b, c } => vec![*a, *b, *c],
        Inst::Cmp { lhs, rhs, .. } => vec![*lhs, *rhs],
        Inst::Call { args, .. } => args.iter().copied().collect(),
        Inst::Phi { incoming } => incoming.iter().map(|(_, v)| *v).collect(),
        Inst::ConstF64(_) | Inst::ConstI64(_) | Inst::ConstBool(_) | Inst::Param { .. } => vec![],
    }
}

pub fn replace_in_inst(inst: &mut Inst, old: Value, new: Value) {
    let sub = |v: &mut Value| { if *v == old { *v = new; } };
    match inst {
        Inst::Add(a, b) | Inst::Sub(a, b) | Inst::Mul(a, b) | Inst::Div(a, b) | Inst::Rem(a, b)
        | Inst::And(a, b) | Inst::Or(a, b) | Inst::Xor(a, b)
        | Inst::Shl(a, b) | Inst::Shr(a, b) | Inst::Sar(a, b)
        | Inst::Min(a, b) | Inst::Max(a, b) => { sub(a); sub(b); }
        Inst::Neg(a) | Inst::Not(a) | Inst::Sqrt(a) | Inst::Abs(a)
        | Inst::Floor(a) | Inst::Ceil(a) | Inst::Round(a) | Inst::Trunc(a)
        | Inst::IToF(a) | Inst::FToI(a) => sub(a),
        Inst::Fma { a, b, c } => { sub(a); sub(b); sub(c); }
        Inst::Cmp { lhs, rhs, .. } => { sub(lhs); sub(rhs); }
        Inst::Call { args, .. } => for a in args.iter_mut() { sub(a); },
        Inst::Phi { incoming } => for (_, v) in incoming.iter_mut() { sub(v); },
        Inst::ConstF64(_) | Inst::ConstI64(_) | Inst::ConstBool(_) | Inst::Param { .. } => {}
    }
}
```

- [ ] **Step 2: Write `lib.rs`**

```rust
// crates/forge-ir/src/lib.rs

pub mod ir;
pub mod builder;
pub mod lower;
pub mod dominance;
pub mod verify;
pub mod print;
pub mod interp;

pub use ir::*;
```

(`builder`, `lower`, `dominance`, `verify`, `print`, `interp` don't exist yet — Tasks 9-13 create them.)

- [ ] **Step 3: Run cargo check to confirm the shape compiles once stubs exist**

This crate won't compile until Task 9 adds the missing modules — that's expected. Skip running `cargo check` here; it's checked at the end of Task 9.

- [ ] **Step 4: Commit**

```bash
git add crates/forge-ir/src/ir.rs crates/forge-ir/src/lib.rs
git commit -m "feat(forge-ir): core IR types (Value, Block, Inst, Function)"
```

---

## Task 9: forge-ir — Braun et al. SSA builder

**Files:**
- Create: `crates/forge-ir/src/builder.rs`

- [ ] **Step 1: Write the builder**

```rust
// crates/forge-ir/src/builder.rs

use rustc_hash::FxHashMap;
use smallvec::SmallVec;

use crate::ir::*;

pub struct Builder {
    pub f: Function,
    current_def: Vec<FxHashMap<String, Value>>,
    sealed: Vec<bool>,
    incomplete_phis: Vec<FxHashMap<String, Value>>,
    pub cur_block: Block,
}

impl Builder {
    pub fn new() -> Self {
        Self {
            f: Function {
                insts: Vec::new(), types: Vec::new(), spans: Vec::new(),
                blocks: Vec::new(), entry: Block(0), params: Vec::new(),
            },
            current_def: Vec::new(),
            sealed: Vec::new(),
            incomplete_phis: Vec::new(),
            cur_block: Block(0),
        }
    }

    pub fn create_block(&mut self) -> Block {
        let id = Block(self.f.blocks.len() as u32);
        self.f.blocks.push(BlockData::default());
        self.current_def.push(FxHashMap::default());
        self.sealed.push(false);
        self.incomplete_phis.push(FxHashMap::default());
        id
    }

    pub fn add_pred(&mut self, block: Block, pred: Block) {
        self.f.blocks[block.0 as usize].preds.push(pred);
    }

    pub fn seal_block(&mut self, block: Block) {
        let pending: Vec<(String, Value)> = self.incomplete_phis[block.0 as usize].drain().collect();
        for (name, phi) in pending {
            self.fill_phi_operands(phi, block, &name);
        }
        self.sealed[block.0 as usize] = true;
    }

    pub fn emit(&mut self, block: Block, inst: Inst, ty: Ty, span: forge_syntax::span::Span) -> Value {
        let v = Value(self.f.insts.len() as u32);
        self.f.insts.push(inst);
        self.f.types.push(ty);
        self.f.spans.push(span);
        self.f.blocks[block.0 as usize].insts.push(v);
        v
    }

    fn new_phi(&mut self, block: Block, ty: Ty) -> Value {
        self.emit(block, Inst::Phi { incoming: SmallVec::new() }, ty, forge_syntax::span::Span::new(0, 0))
    }

    pub fn write_variable(&mut self, name: &str, block: Block, value: Value) {
        self.current_def[block.0 as usize].insert(name.to_string(), value);
    }

    pub fn read_variable(&mut self, name: &str, block: Block, ty: Ty) -> Value {
        if let Some(&v) = self.current_def[block.0 as usize].get(name) { return v; }
        self.read_variable_recursive(name, block, ty)
    }

    fn read_variable_recursive(&mut self, name: &str, block: Block, ty: Ty) -> Value {
        if !self.sealed[block.0 as usize] {
            let phi = self.new_phi(block, ty);
            self.incomplete_phis[block.0 as usize].insert(name.to_string(), phi);
            self.write_variable(name, block, phi);
            return phi;
        }
        let preds = self.f.blocks[block.0 as usize].preds.clone();
        if preds.len() == 1 {
            let v = self.read_variable(name, preds[0], ty);
            self.write_variable(name, block, v);
            return v;
        }
        let phi = self.new_phi(block, ty);
        self.write_variable(name, block, phi); // break cycles before recursing into preds
        self.fill_phi_operands(phi, block, name);
        phi
    }

    fn fill_phi_operands(&mut self, phi: Value, block: Block, name: &str) {
        let ty = self.f.types[phi.0 as usize];
        let preds = self.f.blocks[block.0 as usize].preds.clone();
        let mut incoming = SmallVec::new();
        for p in preds {
            let v = self.read_variable(name, p, ty);
            incoming.push((p, v));
        }
        if let Inst::Phi { incoming: slot } = &mut self.f.insts[phi.0 as usize] {
            *slot = incoming;
        }
        self.try_remove_trivial_phi(phi);
    }

    /// A phi whose operands are all the same value (ignoring itself) is
    /// redundant — replace its uses with that value. This is what keeps
    /// nested-`if` lowering from leaving dead phis behind.
    fn try_remove_trivial_phi(&mut self, phi: Value) {
        let incoming = match &self.f.insts[phi.0 as usize] {
            Inst::Phi { incoming } => incoming.clone(),
            _ => return,
        };
        let mut same: Option<Value> = None;
        for (_, v) in &incoming {
            if *v == phi { continue; }
            match same {
                Some(s) if s != *v => return,
                _ => same = Some(*v),
            }
        }
        if let Some(replacement) = same {
            self.replace_all_uses(phi, replacement);
        }
    }

    pub fn replace_all_uses(&mut self, old: Value, new: Value) {
        for inst in &mut self.f.insts {
            replace_in_inst(inst, old, new);
        }
        for def in &mut self.current_def {
            for v in def.values_mut() {
                if *v == old { *v = new; }
            }
        }
    }
}

impl Default for Builder {
    fn default() -> Self { Self::new() }
}
```

- [ ] **Step 2: Commit (no standalone tests — this is exercised end-to-end by Task 10's lowering tests, since Braun-et-al machinery only makes sense in the context of real `if`/`let` lowering)**

```bash
git add crates/forge-ir/src/builder.rs
git commit -m "feat(forge-ir): Braun et al. SSA builder with incomplete-phi handling"
```

---

## Task 10: forge-ir — AST→IR lowering

**Files:**
- Create: `crates/forge-ir/src/lower.rs`
- Modify: `crates/forge-ir/Cargo.toml` (needs `smallvec` macro already present; no change needed — listed for completeness)

- [ ] **Step 1: Write the lowering test module (failing first)**

```rust
// crates/forge-ir/src/lower.rs — append at the bottom

#[cfg(test)]
mod tests {
    use super::*;
    use forge_syntax::lexer::lex;
    use forge_syntax::parser::parse;
    use forge_syntax::resolve::resolve;
    use forge_syntax::typeck::typecheck;

    fn lowered(src: &str) -> Function {
        let (tokens, _) = lex(src);
        let (ast, _) = parse(&tokens);
        let typed = typecheck(resolve(ast)).expect("should type-check");
        lower(&typed)
    }

    #[test]
    fn sqrt_of_sum_of_squares_is_exactly_six_instructions() {
        // param x, param y, mul, mul, add, sqrt — matches SPEC §15's example.
        let f = lowered("sqrt(x * x + y * y)");
        assert_eq!(f.insts.len(), 6);
    }

    #[test]
    fn if_produces_four_blocks_with_a_phi() {
        let f = lowered("if x > 0.0 then x else -x");
        assert_eq!(f.blocks.len(), 4); // entry, then, else, merge
        let merge = &f.blocks[3];
        let phi_count = merge.insts.iter().filter(|&&v| matches!(f.insts[v.0 as usize], Inst::Phi { .. })).count();
        assert_eq!(phi_count, 1);
    }

    #[test]
    fn outer_reference_after_shadowing_let_resolves_to_the_parameter() {
        // (let x = 1.0 in x) + x must add the let's value to the PARAMETER x,
        // not to itself — this is the shadowing bug the resolve pass fixes.
        let f = lowered("(let x = 1.0 in x) + x");
        let add = f.insts.iter().find(|i| matches!(i, Inst::Add(_, _))).expect("an Add exists");
        let Inst::Add(l, r) = add else { unreachable!() };
        // Both operands trace back to the single Param instruction — if the
        // shadow leaked, one operand would instead be the ConstF64(1.0).
        let param_idx = f.insts.iter().position(|i| matches!(i, Inst::Param { .. })).unwrap() as u32;
        assert_eq!(r.0, param_idx, "the trailing `x` must be the parameter");
        let _ = l; // lhs is the let's inner (shadowed) value — not asserted here
    }

    #[test]
    fn every_block_has_a_terminator() {
        let f = lowered("if x > 0.0 then x else -x");
        for b in &f.blocks {
            assert!(b.term.is_some());
        }
    }
}
```

- [ ] **Step 2: Run the tests to confirm they fail**

Run: `cargo test -p forge-ir --lib lower:: 2>&1 | head -20`
Expected: FAIL — `lower` not defined.

- [ ] **Step 3: Write the lowering implementation above the test module**

```rust
// crates/forge-ir/src/lower.rs — above the `#[cfg(test)]` module

use smallvec::smallvec;

use forge_syntax::ast::{BinaryOp, Expr, ExprIdx, UnaryOp};
use forge_syntax::typeck::{Ty as AstTy, TypedAst};

use crate::builder::Builder;
use crate::ir::*;

pub fn lower(typed: &TypedAst) -> Function {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.f.entry = entry;
    b.cur_block = entry;
    b.seal_block(entry);

    let root_span = typed.ast.span(typed.ast.root);
    for (i, (name, ty)) in typed.params.iter().enumerate() {
        let ty = lower_ty(*ty);
        let v = b.emit(entry, Inst::Param { index: i as u32, ty }, ty, root_span);
        b.f.params.push((name.clone(), ty));
        b.write_variable(name, entry, v);
    }

    let (result, exit_block) = lower_expr(&mut b, typed, typed.ast.root);
    b.f.blocks[exit_block.0 as usize].term = Some(Terminator::Return(result));
    b.f
}

fn lower_ty(t: AstTy) -> Ty {
    match t {
        AstTy::F64 => Ty::F64,
        AstTy::I64 => Ty::I64,
        AstTy::Bool => Ty::Bool,
    }
}

/// Returns the value produced and the block that now holds it. `if` creates
/// new blocks, so every caller threads the returned block forward instead of
/// assuming `b.cur_block` is still what it was before the recursive call.
fn lower_expr(b: &mut Builder, typed: &TypedAst, idx: ExprIdx) -> (Value, Block) {
    let span = typed.ast.span(idx);
    let ty = lower_ty(typed.types[idx.index()]);
    let block = b.cur_block;

    match typed.ast.get(idx).clone() {
        Expr::Float(v) => (b.emit(block, Inst::ConstF64(v.to_bits()), ty, span), block),
        Expr::Int(n) => (b.emit(block, Inst::ConstI64(n), ty, span), block),
        Expr::Bool(v) => (b.emit(block, Inst::ConstBool(v), ty, span), block),
        Expr::Ident(name) => (b.read_variable(&name, block, ty), block),

        Expr::Unary { op, operand } => {
            let (v, block) = lower_expr(b, typed, operand);
            b.cur_block = block;
            let inst = match op {
                UnaryOp::Neg => Inst::Neg(v),
                UnaryOp::Not | UnaryOp::BitNot => Inst::Not(v),
            };
            (b.emit(block, inst, ty, span), block)
        }

        Expr::Binary { op, lhs, rhs } => {
            let (l, block) = lower_expr(b, typed, lhs);
            b.cur_block = block;
            let (r, block) = lower_expr(b, typed, rhs);
            b.cur_block = block;
            let inst = lower_binary(op, l, r);
            (b.emit(block, inst, ty, span), block)
        }

        Expr::Call { callee, args } => {
            let mut vals = Vec::new();
            let mut block = block;
            for a in &args {
                let (v, blk) = lower_expr(b, typed, *a);
                vals.push(v);
                block = blk;
                b.cur_block = block;
            }
            let inst = lower_call(&callee, &vals);
            (b.emit(block, inst, ty, span), block)
        }

        Expr::If { cond, then_, else_ } => {
            let (c, block) = lower_expr(b, typed, cond);
            let then_block = b.create_block();
            let else_block = b.create_block();
            let merge_block = b.create_block();

            b.f.blocks[block.0 as usize].term =
                Some(Terminator::Branch { cond: c, then_: then_block, else_: else_block });
            b.add_pred(then_block, block);
            b.add_pred(else_block, block);
            b.seal_block(then_block);
            b.seal_block(else_block);

            b.cur_block = then_block;
            let (then_val, then_exit) = lower_expr(b, typed, then_);
            b.f.blocks[then_exit.0 as usize].term = Some(Terminator::Jump(merge_block));
            b.add_pred(merge_block, then_exit);

            b.cur_block = else_block;
            let (else_val, else_exit) = lower_expr(b, typed, else_);
            b.f.blocks[else_exit.0 as usize].term = Some(Terminator::Jump(merge_block));
            b.add_pred(merge_block, else_exit);

            b.seal_block(merge_block);
            b.cur_block = merge_block;
            let incoming = smallvec![(then_exit, then_val), (else_exit, else_val)];
            (b.emit(merge_block, Inst::Phi { incoming }, ty, span), merge_block)
        }

        // `name` was already alpha-renamed by forge_syntax::resolve to be
        // globally unique, so writing it into this block's SSA variable map
        // can never collide with (or need restoring after) any other
        // binding — see the design doc's "Resolved ambiguities".
        Expr::Let { name, value, body } => {
            let (v, block) = lower_expr(b, typed, value);
            b.cur_block = block;
            b.write_variable(&name, block, v);
            lower_expr(b, typed, body)
        }
    }
}

fn lower_binary(op: BinaryOp, l: Value, r: Value) -> Inst {
    use BinaryOp::*;
    match op {
        Add => Inst::Add(l, r), Sub => Inst::Sub(l, r), Mul => Inst::Mul(l, r),
        Div => Inst::Div(l, r), Rem => Inst::Rem(l, r),
        BitAnd | And => Inst::And(l, r),
        BitOr | Or => Inst::Or(l, r),
        BitXor => Inst::Xor(l, r),
        Shl => Inst::Shl(l, r), Shr => Inst::Shr(l, r),
        Eq => Inst::Cmp { op: CmpOp::Eq, lhs: l, rhs: r },
        Ne => Inst::Cmp { op: CmpOp::Ne, lhs: l, rhs: r },
        Lt => Inst::Cmp { op: CmpOp::Lt, lhs: l, rhs: r },
        Le => Inst::Cmp { op: CmpOp::Le, lhs: l, rhs: r },
        Gt => Inst::Cmp { op: CmpOp::Gt, lhs: l, rhs: r },
        Ge => Inst::Cmp { op: CmpOp::Ge, lhs: l, rhs: r },
    }
}

fn lower_call(callee: &str, args: &[Value]) -> Inst {
    match callee {
        "sqrt" => Inst::Sqrt(args[0]),
        "abs" => Inst::Abs(args[0]),
        "floor" => Inst::Floor(args[0]),
        "ceil" => Inst::Ceil(args[0]),
        "round" => Inst::Round(args[0]),
        "trunc" => Inst::Trunc(args[0]),
        "min" => Inst::Min(args[0], args[1]),
        "max" => Inst::Max(args[0], args[1]),
        "fma" => Inst::Fma { a: args[0], b: args[1], c: args[2] },
        "sin" => Inst::Call { func: LibFunc::Sin, args: args.iter().copied().collect() },
        "cos" => Inst::Call { func: LibFunc::Cos, args: args.iter().copied().collect() },
        "tan" => Inst::Call { func: LibFunc::Tan, args: args.iter().copied().collect() },
        "exp" => Inst::Call { func: LibFunc::Exp, args: args.iter().copied().collect() },
        "log" => Inst::Call { func: LibFunc::Log, args: args.iter().copied().collect() },
        "pow" => Inst::Call { func: LibFunc::Pow, args: args.iter().copied().collect() },
        other => unreachable!("type checker already rejected unknown intrinsic `{other}`"),
    }
}
```

- [ ] **Step 4: Add `forge-syntax` types re-export needed by the test module**

The test module uses `forge_syntax::typeck::typecheck` and `forge_syntax::resolve::resolve` — both already `pub` from Task 6/7, no changes needed. Just confirm `forge-ir/Cargo.toml`'s `forge-syntax = { path = "../forge-syntax" }` dependency (added in Task 1) is present.

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-ir --lib lower:: 2>&1 | tail -20`
Expected: 4 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/forge-ir/src/lower.rs
git commit -m "feat(forge-ir): AST-to-SSA-IR lowering for the scalar language"
```

---

## Task 11: forge-ir — dominance and IR verifier

**Files:**
- Create: `crates/forge-ir/src/dominance.rs`
- Create: `crates/forge-ir/src/verify.rs`

- [ ] **Step 1: Write `dominance.rs`**

```rust
// crates/forge-ir/src/dominance.rs

use rustc_hash::FxHashMap;

use crate::ir::*;

pub fn reverse_postorder(f: &Function) -> Vec<Block> {
    let mut visited = vec![false; f.blocks.len()];
    let mut post = Vec::new();
    visit(f, f.entry, &mut visited, &mut post);
    post.reverse();
    post
}

fn visit(f: &Function, b: Block, visited: &mut Vec<bool>, post: &mut Vec<Block>) {
    if visited[b.0 as usize] { return; }
    visited[b.0 as usize] = true;
    if let Some(term) = &f.blocks[b.0 as usize].term {
        match term {
            Terminator::Return(_) => {}
            Terminator::Jump(t) => visit(f, *t, visited, post),
            Terminator::Branch { then_, else_, .. } => {
                visit(f, *then_, visited, post);
                visit(f, *else_, visited, post);
            }
        }
    }
    post.push(b);
}

/// Cooper-Harvey-Kennedy iterative dominator algorithm.
pub fn compute_dominators(f: &Function) -> Vec<Option<Block>> {
    let rpo = reverse_postorder(f);
    let rpo_num: FxHashMap<Block, usize> = rpo.iter().enumerate().map(|(i, &b)| (b, i)).collect();
    let mut idom: Vec<Option<Block>> = vec![None; f.blocks.len()];
    idom[f.entry.0 as usize] = Some(f.entry);

    let mut changed = true;
    while changed {
        changed = false;
        for &b in rpo.iter().skip(1) {
            let preds = f.blocks[b.0 as usize].preds.clone();
            let mut new_idom = None;
            for p in preds {
                if idom[p.0 as usize].is_none() { continue; }
                new_idom = Some(match new_idom {
                    None => p,
                    Some(cur) => intersect(&idom, &rpo_num, cur, p),
                });
            }
            if idom[b.0 as usize] != new_idom {
                idom[b.0 as usize] = new_idom;
                changed = true;
            }
        }
    }
    idom
}

fn intersect(idom: &[Option<Block>], rpo_num: &FxHashMap<Block, usize>, mut a: Block, mut b: Block) -> Block {
    while a != b {
        while rpo_num[&a] > rpo_num[&b] { a = idom[a.0 as usize].expect("reachable block has an idom"); }
        while rpo_num[&b] > rpo_num[&a] { b = idom[b.0 as usize].expect("reachable block has an idom"); }
    }
    a
}

pub fn dominates(idom: &[Option<Block>], a: Block, mut b: Block) -> bool {
    loop {
        if a == b { return true; }
        match idom[b.0 as usize] {
            Some(next) if next != b => b = next,
            _ => return false,
        }
    }
}
```

- [ ] **Step 2: Write the verifier test module (failing first)**

```rust
// crates/forge-ir/src/verify.rs — append at the bottom

#[cfg(test)]
mod tests {
    use super::*;
    use forge_syntax::lexer::lex;
    use forge_syntax::parser::parse;
    use forge_syntax::resolve::resolve;
    use forge_syntax::typeck::typecheck;
    use forge_syntax::span::Span;

    fn lowered(src: &str) -> Function {
        let (tokens, _) = lex(src);
        let (ast, _) = parse(&tokens);
        let typed = typecheck(resolve(ast)).expect("should type-check");
        crate::lower::lower(&typed)
    }

    #[test]
    fn straight_line_expression_verifies() {
        let f = lowered("sqrt(x * x + y * y)");
        assert!(verify(&f).is_ok());
    }

    #[test]
    fn if_expression_verifies() {
        let f = lowered("if x > 0.0 then x else -x");
        assert!(verify(&f).is_ok());
    }

    #[test]
    fn rejects_use_before_def() {
        let mut f = lowered("x * x");
        // Hand-corrupt: make the Mul's second operand a not-yet-defined
        // value index (one past the end of insts).
        let bad = Value(f.insts.len() as u32);
        if let Inst::Mul(_, b) = &mut f.insts[f.insts.len() - 1] { *b = bad; }
        assert!(verify(&f).is_err());
    }

    #[test]
    fn rejects_phi_with_wrong_operand_count() {
        let mut f = lowered("if x > 0.0 then x else -x");
        let phi_idx = f.insts.iter().position(|i| matches!(i, Inst::Phi { .. })).unwrap();
        if let Inst::Phi { incoming } = &mut f.insts[phi_idx] {
            incoming.pop(); // now has 1 operand but its block has 2 preds
        }
        assert!(verify(&f).is_err());
    }

    #[test]
    fn dominance_rejects_a_value_used_outside_its_defining_branch() {
        // Hand-build: entry branches to then/else/merge; merge's Return
        // illegally uses a value defined only in `then`, which does not
        // dominate `merge`.
        let mut f = lowered("if x > 0.0 then x else -x");
        let then_block = f.blocks[1].insts.clone();
        let then_val = *then_block.first().expect("then block has at least one inst");
        // Force merge's Return to use the then-only value instead of the phi.
        let merge_idx = f.blocks.len() - 1;
        f.blocks[merge_idx].term = Some(Terminator::Return(then_val));
        assert!(verify(&f).is_err());
        let _ = Span::new(0, 0); // silence unused import if Span is otherwise unused
    }
}
```

- [ ] **Step 3: Run the tests to confirm they fail**

Run: `cargo test -p forge-ir --lib verify:: 2>&1 | head -20`
Expected: FAIL — `verify` not defined.

- [ ] **Step 4: Write the verifier implementation above the test module**

```rust
// crates/forge-ir/src/verify.rs — above the `#[cfg(test)]` module

use rustc_hash::FxHashMap;

use crate::dominance::{compute_dominators, dominates};
use crate::ir::*;

pub fn verify(f: &Function) -> Result<(), String> {
    let idom = compute_dominators(f);

    let mut defined_in: FxHashMap<Value, Block> = FxHashMap::default();
    for (bi, bd) in f.blocks.iter().enumerate() {
        for &v in &bd.insts {
            defined_in.insert(v, Block(bi as u32));
        }
    }

    for (bi, bd) in f.blocks.iter().enumerate() {
        let block = Block(bi as u32);

        for &v in &bd.insts {
            match &f.insts[v.0 as usize] {
                // A phi operand must be dominated by its def AT THE
                // PREDECESSOR EDGE, not dominate the phi's own block — a
                // value from the `then` branch legitimately doesn't
                // dominate `merge` as a block, but it does reach merge via
                // exactly the `then` edge the phi records it against.
                Inst::Phi { incoming } => {
                    for (pred, val) in incoming {
                        let def_block = *defined_in.get(val)
                            .ok_or_else(|| format!("value {val:?} used but never defined"))?;
                        if !dominates(&idom, def_block, *pred) {
                            return Err(format!(
                                "phi operand {val:?} (defined in {def_block:?}) does not dominate predecessor {pred:?}"
                            ));
                        }
                    }
                }
                other => {
                    for used in uses_of(other) {
                        let def_block = *defined_in.get(&used)
                            .ok_or_else(|| format!("value {used:?} used but never defined"))?;
                        if !dominates(&idom, def_block, block) {
                            return Err(format!(
                                "value {used:?} (defined in {def_block:?}) does not dominate its use in {block:?}"
                            ));
                        }
                    }
                }
            }
        }

        match &bd.term {
            Some(Terminator::Return(v)) => {
                let def_block = *defined_in.get(v)
                    .ok_or_else(|| format!("return of undefined value {v:?}"))?;
                if !dominates(&idom, def_block, block) {
                    return Err(format!("returned value {v:?} does not dominate {block:?}"));
                }
            }
            Some(_) => {}
            None => return Err(format!("block {block:?} has no terminator")),
        }

        for &v in &bd.insts {
            if let Inst::Phi { incoming } = &f.insts[v.0 as usize] {
                if incoming.len() != bd.preds.len() {
                    return Err(format!(
                        "phi {v:?} in {block:?} has {} operand(s) but block has {} predecessor(s)",
                        incoming.len(), bd.preds.len()
                    ));
                }
            }
        }
    }

    Ok(())
}
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-ir --lib verify:: 2>&1 | tail -20`
Expected: 5 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/forge-ir/src/dominance.rs crates/forge-ir/src/verify.rs
git commit -m "feat(forge-ir): dominance tree and IR verifier"
```

---

## Task 12: forge-ir — textual IR printer

**Files:**
- Create: `crates/forge-ir/src/print.rs`

- [ ] **Step 1: Write the printer test (failing first)**

```rust
// crates/forge-ir/src/print.rs — append at the bottom

#[cfg(test)]
mod tests {
    use super::*;
    use forge_syntax::lexer::lex;
    use forge_syntax::parser::parse;
    use forge_syntax::resolve::resolve;
    use forge_syntax::typeck::typecheck;

    #[test]
    fn prints_one_line_per_instruction_plus_terminator() {
        let (tokens, _) = lex("sqrt(x * x + y * y)");
        let (ast, _) = parse(&tokens);
        let typed = typecheck(resolve(ast)).unwrap();
        let f = crate::lower::lower(&typed);
        let text = print_function(&f);
        // 2 params + mul + mul + add + sqrt = 6 inst lines, + 1 ret line.
        assert_eq!(text.lines().count(), 7);
        assert!(text.contains("sqrt"));
        assert!(text.contains("ret"));
    }
}
```

- [ ] **Step 2: Run the test to confirm it fails**

Run: `cargo test -p forge-ir --lib print:: 2>&1 | head -20`
Expected: FAIL — `print_function` not defined.

- [ ] **Step 3: Write the printer implementation above the test module**

```rust
// crates/forge-ir/src/print.rs — above the `#[cfg(test)]` module

use std::fmt::Write;

use crate::ir::*;

pub fn print_function(f: &Function) -> String {
    let mut out = String::new();
    for (bi, bd) in f.blocks.iter().enumerate() {
        writeln!(out, "block{bi}:").unwrap();
        for &v in &bd.insts {
            writeln!(out, "  v{} = {}", v.0, print_inst(&f.insts[v.0 as usize])).unwrap();
        }
        match &bd.term {
            Some(Terminator::Return(v)) => writeln!(out, "  ret v{}", v.0).unwrap(),
            Some(Terminator::Jump(b)) => writeln!(out, "  jump block{}", b.0).unwrap(),
            Some(Terminator::Branch { cond, then_, else_ }) =>
                writeln!(out, "  branch v{}, block{}, block{}", cond.0, then_.0, else_.0).unwrap(),
            None => writeln!(out, "  <no terminator>").unwrap(),
        }
    }
    out
}

fn print_inst(inst: &Inst) -> String {
    match inst {
        Inst::ConstF64(bits) => format!("const.f64 {}", f64::from_bits(*bits)),
        Inst::ConstI64(n) => format!("const.i64 {n}"),
        Inst::ConstBool(v) => format!("const.bool {v}"),
        Inst::Param { index, ty } => format!("param {index} : {ty:?}"),
        Inst::Add(a, b) => format!("add v{}, v{}", a.0, b.0),
        Inst::Sub(a, b) => format!("sub v{}, v{}", a.0, b.0),
        Inst::Mul(a, b) => format!("mul v{}, v{}", a.0, b.0),
        Inst::Div(a, b) => format!("div v{}, v{}", a.0, b.0),
        Inst::Rem(a, b) => format!("rem v{}, v{}", a.0, b.0),
        Inst::Neg(a) => format!("neg v{}", a.0),
        Inst::Fma { a, b, c } => format!("fma v{}, v{}, v{}", a.0, b.0, c.0),
        Inst::And(a, b) => format!("and v{}, v{}", a.0, b.0),
        Inst::Or(a, b) => format!("or v{}, v{}", a.0, b.0),
        Inst::Xor(a, b) => format!("xor v{}, v{}", a.0, b.0),
        Inst::Not(a) => format!("not v{}", a.0),
        Inst::Shl(a, b) => format!("shl v{}, v{}", a.0, b.0),
        Inst::Shr(a, b) => format!("shr v{}, v{}", a.0, b.0),
        Inst::Sar(a, b) => format!("sar v{}, v{}", a.0, b.0),
        Inst::Cmp { op, lhs, rhs } => format!("cmp.{op:?} v{}, v{}", lhs.0, rhs.0),
        Inst::Sqrt(a) => format!("sqrt v{}", a.0),
        Inst::Abs(a) => format!("abs v{}", a.0),
        Inst::Min(a, b) => format!("min v{}, v{}", a.0, b.0),
        Inst::Max(a, b) => format!("max v{}, v{}", a.0, b.0),
        Inst::Floor(a) => format!("floor v{}", a.0),
        Inst::Ceil(a) => format!("ceil v{}", a.0),
        Inst::Round(a) => format!("round v{}", a.0),
        Inst::Trunc(a) => format!("trunc v{}", a.0),
        Inst::Call { func, args } => {
            let parts: Vec<String> = args.iter().map(|a| format!("v{}", a.0)).collect();
            format!("call.{func:?} {}", parts.join(", "))
        }
        Inst::IToF(a) => format!("itof v{}", a.0),
        Inst::FToI(a) => format!("ftoi v{}", a.0),
        Inst::Phi { incoming } => {
            let parts: Vec<String> = incoming.iter().map(|(b, v)| format!("block{} -> v{}", b.0, v.0)).collect();
            format!("phi [{}]", parts.join(", "))
        }
    }
}
```

- [ ] **Step 4: Run the test and confirm it passes**

Run: `cargo test -p forge-ir --lib print:: 2>&1 | tail -10`
Expected: 1 test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/forge-ir/src/print.rs
git commit -m "feat(forge-ir): textual IR printer"
```

---

## Task 13: forge-ir — RtValue and the interpreter oracle

**Files:**
- Create: `crates/forge-ir/src/interp.rs`

- [ ] **Step 1: Write the interpreter test module (failing first)**

```rust
// crates/forge-ir/src/interp.rs — append at the bottom

#[cfg(test)]
mod tests {
    use super::*;
    use forge_syntax::lexer::lex;
    use forge_syntax::parser::parse;
    use forge_syntax::resolve::resolve;
    use forge_syntax::typeck::typecheck;

    fn run(src: &str, args: &[RtValue]) -> RtValue {
        let (tokens, _) = lex(src);
        let (ast, _) = parse(&tokens);
        let typed = typecheck(resolve(ast)).unwrap();
        let f = crate::lower::lower(&typed);
        interpret(&f, args)
    }

    #[test]
    fn sqrt_of_three_four_five_triangle() {
        let r = run("sqrt(x * x + y * y)", &[RtValue::F64(3.0), RtValue::F64(4.0)]);
        assert_eq!(r, RtValue::F64(5.0));
    }

    #[test]
    fn known_intrinsic_values() {
        assert_eq!(run("abs(x)", &[RtValue::F64(-2.5)]), RtValue::F64(2.5));
        assert_eq!(run("floor(x)", &[RtValue::F64(2.7)]), RtValue::F64(2.0));
        assert_eq!(run("ceil(x)", &[RtValue::F64(2.1)]), RtValue::F64(3.0));
        assert_eq!(run("min(x, y)", &[RtValue::F64(3.0), RtValue::F64(5.0)]), RtValue::F64(3.0));
        assert_eq!(run("max(x, y)", &[RtValue::F64(3.0), RtValue::F64(5.0)]), RtValue::F64(5.0));
        assert_eq!(run("sin(x)", &[RtValue::F64(0.0)]), RtValue::F64(0.0));
    }

    #[test]
    fn nan_propagates_through_arithmetic() {
        let r = run("x + 1.0", &[RtValue::F64(f64::NAN)]);
        assert!(matches!(r, RtValue::F64(v) if v.is_nan()));
    }

    #[test]
    fn nan_comparison_takes_else_branch() {
        // All comparisons with NaN are false, so `if NaN > 0.0` must take else.
        let r = run("if x > 0.0 then 1.0 else 2.0", &[RtValue::F64(f64::NAN)]);
        assert_eq!(r, RtValue::F64(2.0));
    }

    #[test]
    fn i64_arithmetic_wraps_on_overflow() {
        let r = run("x + y", &[RtValue::I64(i64::MAX), RtValue::I64(1)]);
        assert_eq!(r, RtValue::I64(i64::MIN));
    }

    #[test]
    fn bitwise_shift_matches_spec_example() {
        // (n * 2654435761) >> 16, evaluated with a concrete n.
        let r = run("(n * 2654435761) >> 16", &[RtValue::I64(12345)]);
        let expected = (12345i64.wrapping_mul(2654435761)) >> 16;
        assert_eq!(r, RtValue::I64(expected));
    }
}
```

- [ ] **Step 2: Run the tests to confirm they fail**

Run: `cargo test -p forge-ir --lib interp:: 2>&1 | head -20`
Expected: FAIL — `RtValue`/`interpret` not defined.

- [ ] **Step 3: Write the interpreter implementation above the test module**

```rust
// crates/forge-ir/src/interp.rs — above the `#[cfg(test)]` module

use crate::ir::*;

/// Runtime value carried through the interpreter, and later into JIT calling
/// conventions. `Function.params` allows real, independently-typed f64/i64/
/// bool parameters, so a single `f64` slot can't represent every argument or
/// result — hence the enum. See SPEC §3 "Runtime value representation".
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RtValue {
    F64(f64),
    I64(i64),
    Bool(bool),
}

impl RtValue {
    pub fn as_f64(self) -> f64 { match self { RtValue::F64(x) => x, _ => panic!("expected f64") } }
    pub fn as_i64(self) -> i64 { match self { RtValue::I64(x) => x, _ => panic!("expected i64") } }
    pub fn as_bool(self) -> bool { match self { RtValue::Bool(x) => x, _ => panic!("expected bool") } }
}

fn get(vals: &[Option<RtValue>], v: Value) -> RtValue {
    vals[v.0 as usize].expect("undefined value")
}

/// The correctness oracle for the entire project. Must implement IEEE-754
/// semantics EXACTLY — NaN propagation, signed zeros, infinities, subnormals.
/// No shortcuts. Every future differential test compares the JIT to this,
/// bit for bit, so any sloppiness here becomes a false failure (or worse,
/// masks a real JIT bug). Integer ops use Rust's wrapping arithmetic
/// throughout, matching the raw machine `add`/`sub`/`imul` the JIT will
/// eventually emit, which wrap on overflow with no trap.
pub fn interpret(f: &Function, args: &[RtValue]) -> RtValue {
    let mut vals: Vec<Option<RtValue>> = vec![None; f.insts.len()];
    let mut block = f.entry;
    let mut prev_block: Option<Block> = None;

    loop {
        for &v in &f.blocks[block.0 as usize].insts {
            let result = match &f.insts[v.0 as usize] {
                Inst::ConstF64(bits) => RtValue::F64(f64::from_bits(*bits)),
                Inst::ConstI64(n) => RtValue::I64(*n),
                Inst::ConstBool(b) => RtValue::Bool(*b),
                Inst::Param { index, .. } => args[*index as usize],

                Inst::Add(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::F64(x), RtValue::F64(y)) => RtValue::F64(x + y),
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x.wrapping_add(y)),
                    _ => unreachable!("type checker guarantees matching operand types"),
                },
                Inst::Sub(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::F64(x), RtValue::F64(y)) => RtValue::F64(x - y),
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x.wrapping_sub(y)),
                    _ => unreachable!(),
                },
                Inst::Mul(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::F64(x), RtValue::F64(y)) => RtValue::F64(x * y),
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x.wrapping_mul(y)),
                    _ => unreachable!(),
                },
                Inst::Div(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::F64(x), RtValue::F64(y)) => RtValue::F64(x / y),
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x.wrapping_div(y)),
                    _ => unreachable!(),
                },
                Inst::Rem(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::F64(x), RtValue::F64(y)) => RtValue::F64(x % y),
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x.wrapping_rem(y)),
                    _ => unreachable!(),
                },
                Inst::Neg(a) => match get(&vals, *a) {
                    RtValue::F64(x) => RtValue::F64(-x),
                    RtValue::I64(x) => RtValue::I64(x.wrapping_neg()),
                    _ => unreachable!(),
                },

                Inst::Sqrt(a) => RtValue::F64(get(&vals, *a).as_f64().sqrt()),
                Inst::Abs(a) => RtValue::F64(get(&vals, *a).as_f64().abs()),
                Inst::Floor(a) => RtValue::F64(get(&vals, *a).as_f64().floor()),
                Inst::Ceil(a) => RtValue::F64(get(&vals, *a).as_f64().ceil()),
                Inst::Round(a) => RtValue::F64(get(&vals, *a).as_f64().round()),
                Inst::Trunc(a) => RtValue::F64(get(&vals, *a).as_f64().trunc()),
                // CAREFUL: f64::min/max have DIFFERENT NaN semantics from
                // x86's minsd/maxsd (Rust returns the non-NaN operand; minsd
                // returns its second operand if either is NaN). We pick
                // Rust's semantics here — codegen must later emit an extra
                // compare rather than a bare minsd to match.
                Inst::Min(a, b) => RtValue::F64(get(&vals, *a).as_f64().min(get(&vals, *b).as_f64())),
                Inst::Max(a, b) => RtValue::F64(get(&vals, *a).as_f64().max(get(&vals, *b).as_f64())),
                Inst::Fma { a, b, c } => RtValue::F64(
                    get(&vals, *a).as_f64().mul_add(get(&vals, *b).as_f64(), get(&vals, *c).as_f64())),

                Inst::And(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x & y),
                    (RtValue::Bool(x), RtValue::Bool(y)) => RtValue::Bool(x & y),
                    _ => unreachable!(),
                },
                Inst::Or(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x | y),
                    (RtValue::Bool(x), RtValue::Bool(y)) => RtValue::Bool(x | y),
                    _ => unreachable!(),
                },
                Inst::Xor(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x ^ y),
                    (RtValue::Bool(x), RtValue::Bool(y)) => RtValue::Bool(x ^ y),
                    _ => unreachable!(),
                },
                Inst::Not(a) => match get(&vals, *a) {
                    RtValue::I64(x) => RtValue::I64(!x),
                    RtValue::Bool(x) => RtValue::Bool(!x),
                    _ => unreachable!(),
                },
                Inst::Shl(a, b) => RtValue::I64(get(&vals, *a).as_i64().wrapping_shl(get(&vals, *b).as_i64() as u32)),
                Inst::Shr(a, b) => RtValue::I64(
                    (get(&vals, *a).as_i64() as u64).wrapping_shr(get(&vals, *b).as_i64() as u32) as i64),
                Inst::Sar(a, b) => RtValue::I64(get(&vals, *a).as_i64().wrapping_shr(get(&vals, *b).as_i64() as u32)),

                Inst::Cmp { op, lhs, rhs } => RtValue::Bool(eval_cmp(*op, get(&vals, *lhs), get(&vals, *rhs))),

                Inst::Call { func, args: call_args } => {
                    let a = get(&vals, call_args[0]).as_f64();
                    RtValue::F64(match func {
                        LibFunc::Sin => a.sin(),
                        LibFunc::Cos => a.cos(),
                        LibFunc::Tan => a.tan(),
                        LibFunc::Exp => a.exp(),
                        LibFunc::Log => a.ln(),
                        LibFunc::Pow => a.powf(get(&vals, call_args[1]).as_f64()),
                    })
                }

                Inst::IToF(a) => RtValue::F64(get(&vals, *a).as_i64() as f64),
                Inst::FToI(a) => RtValue::I64(get(&vals, *a).as_f64() as i64),

                Inst::Phi { incoming } => {
                    let from = prev_block.expect("phi in entry block");
                    let (_, val) = incoming.iter().find(|(b, _)| *b == from)
                        .expect("phi missing operand for predecessor");
                    get(&vals, *val)
                }
            };
            vals[v.0 as usize] = Some(result);
        }

        match &f.blocks[block.0 as usize].term {
            Some(Terminator::Return(v)) => return get(&vals, *v),
            Some(Terminator::Jump(b)) => { prev_block = Some(block); block = *b; }
            Some(Terminator::Branch { cond, then_, else_ }) => {
                prev_block = Some(block);
                // Cond is always bool-typed (Cmp or a bool param) — no float
                // truthiness coercion to get wrong.
                block = if get(&vals, *cond).as_bool() { *then_ } else { *else_ };
            }
            None => panic!("block {block:?} has no terminator"),
        }
    }
}

fn eval_cmp(op: CmpOp, l: RtValue, r: RtValue) -> bool {
    match (op, l, r) {
        (CmpOp::Eq, RtValue::F64(x), RtValue::F64(y)) => x == y,
        (CmpOp::Ne, RtValue::F64(x), RtValue::F64(y)) => x != y,
        (CmpOp::Lt, RtValue::F64(x), RtValue::F64(y)) => x < y,
        (CmpOp::Le, RtValue::F64(x), RtValue::F64(y)) => x <= y,
        (CmpOp::Gt, RtValue::F64(x), RtValue::F64(y)) => x > y,
        (CmpOp::Ge, RtValue::F64(x), RtValue::F64(y)) => x >= y,
        (CmpOp::Eq, RtValue::I64(x), RtValue::I64(y)) => x == y,
        (CmpOp::Ne, RtValue::I64(x), RtValue::I64(y)) => x != y,
        (CmpOp::Lt, RtValue::I64(x), RtValue::I64(y)) => x < y,
        (CmpOp::Le, RtValue::I64(x), RtValue::I64(y)) => x <= y,
        (CmpOp::Gt, RtValue::I64(x), RtValue::I64(y)) => x > y,
        (CmpOp::Ge, RtValue::I64(x), RtValue::I64(y)) => x >= y,
        (CmpOp::Eq, RtValue::Bool(x), RtValue::Bool(y)) => x == y,
        (CmpOp::Ne, RtValue::Bool(x), RtValue::Bool(y)) => x != y,
        _ => unreachable!("type checker guarantees comparable operand types"),
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-ir --lib interp:: 2>&1 | tail -20`
Expected: 7 tests pass.

- [ ] **Step 5: Run the full forge-ir test suite**

Run: `cargo test -p forge-ir 2>&1 | tail -30`
Expected: all builder/lower/dominance/verify/print/interp tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/forge-ir/src/interp.rs
git commit -m "feat(forge-ir): RtValue and the interpret() correctness oracle"
```

---

## Task 14: End-to-end integration test + parse/print round-trip property test

**Files:**
- Create: `crates/forge-ir/tests/e2e.rs`
- Create: `crates/forge-syntax/tests/roundtrip.rs`

- [ ] **Step 1: Write the end-to-end integration test**

```rust
// crates/forge-ir/tests/e2e.rs

//! Exit criterion #3 from the design doc: the real
//! source → lex → parse → resolve → typecheck → lower → interpret path,
//! for one representative expression per language feature this slice
//! supports.

use forge_ir::interp::{interpret, RtValue};
use forge_ir::lower::lower;
use forge_ir::verify::verify;
use forge_syntax::lexer::lex;
use forge_syntax::parser::parse;
use forge_syntax::resolve::resolve;
use forge_syntax::typeck::typecheck;

fn eval(src: &str, args: &[RtValue]) -> RtValue {
    let (tokens, diags) = lex(src);
    assert!(diags.is_empty(), "lex errors for {src:?}: {diags:?}");
    let (ast, diags) = parse(&tokens);
    assert!(diags.is_empty(), "parse errors for {src:?}: {diags:?}");
    let typed = typecheck(resolve(ast)).unwrap_or_else(|e| panic!("type errors for {src:?}: {e:?}"));
    let f = lower(&typed);
    verify(&f).unwrap_or_else(|e| panic!("verifier rejected {src:?}: {e}"));
    interpret(&f, args)
}

#[test]
fn straight_line_arithmetic() {
    assert_eq!(eval("3.14159 * r * r", &[RtValue::F64(2.0)]), RtValue::F64(3.14159 * 2.0 * 2.0));
}

#[test]
fn if_and_let() {
    let r = eval("let t = a - b in if t > 0.0 then t else -t", &[RtValue::F64(3.0), RtValue::F64(5.0)]);
    assert_eq!(r, RtValue::F64(2.0)); // |3 - 5|
}

#[test]
fn intrinsic_sqrt() {
    assert_eq!(eval("sqrt(x * x + y * y)", &[RtValue::F64(3.0), RtValue::F64(4.0)]), RtValue::F64(5.0));
}

#[test]
fn libm_call() {
    let r = eval("sin(x) + cos(y)", &[RtValue::F64(0.0), RtValue::F64(0.0)]);
    assert_eq!(r, RtValue::F64(0.0f64.sin() + 0.0f64.cos()));
}

#[test]
fn integer_and_bitwise_expression() {
    let r = eval("(n * 2654435761) >> 16", &[RtValue::I64(999)]);
    assert_eq!(r, RtValue::I64((999i64.wrapping_mul(2654435761)) >> 16));
}

#[test]
fn nan_producing_expression() {
    let r = eval("x / y", &[RtValue::F64(0.0), RtValue::F64(0.0)]);
    assert!(matches!(r, RtValue::F64(v) if v.is_nan()));
}
```

- [ ] **Step 2: Add public `verify` re-export path check**

`forge-ir/src/lib.rs` already has `pub mod verify;` from Task 11 — confirm `verify::verify` is reachable as `forge_ir::verify::verify` (it is, since the module is `pub`). No changes needed.

- [ ] **Step 3: Run the integration test**

Run: `cargo test -p forge-ir --test e2e 2>&1 | tail -20`
Expected: 6 tests pass.

- [ ] **Step 4: Write the parse/print round-trip property test**

```rust
// crates/forge-syntax/tests/roundtrip.rs

use forge_syntax::lexer::lex;
use forge_syntax::parser::parse;
use proptest::prelude::*;

/// A tiny well-formed-expression generator — deep enough to exercise every
/// binary/unary op and both branches of `if`, shallow enough to stay fast.
fn arb_expr() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        (0.0f64..1000.0).prop_map(|f| format!("{f:.3}")),
        (1i64..1000).prop_map(|n| n.to_string()),
        Just("x".to_string()),
        Just("y".to_string()),
    ];
    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} + {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} * {b})")),
            (inner.clone(), inner.clone(), inner.clone())
                .prop_map(|(c, t, e)| format!("(if {c} > 0.0 then {t} else {e})")),
            inner.clone().prop_map(|a| format!("sqrt({a} * {a})")),
        ]
    })
}

proptest! {
    #[test]
    fn parse_print_round_trip_preserves_structure(src in arb_expr()) {
        let (tokens, diags) = lex(&src);
        prop_assert!(diags.is_empty(), "lex diagnostics for {src:?}: {diags:?}");
        let (ast, diags) = parse(&tokens);
        prop_assert!(diags.is_empty(), "parse diagnostics for {src:?}: {diags:?}");

        // Re-lex/parse the same source a second time — a stable parser must
        // produce a structurally identical tree both times. (A true
        // print(ast)->parse->compare round trip needs an AST pretty-printer,
        // which this slice doesn't build; re-parsing the same text is the
        // cheap, still-meaningful version of the same property: determinism.)
        let (tokens2, _) = lex(&src);
        let (ast2, _) = parse(&tokens2);
        prop_assert_eq!(ast.exprs.len(), ast2.exprs.len());
    }
}
```

- [ ] **Step 5: Run the property test**

Run: `cargo test -p forge-syntax --test roundtrip 2>&1 | tail -20`
Expected: passes (256 cases by default).

- [ ] **Step 6: Commit**

```bash
git add crates/forge-ir/tests/e2e.rs crates/forge-syntax/tests/roundtrip.rs
git commit -m "test: end-to-end lex-to-interpret pipeline and parser determinism property test"
```

---

## Task 15: Final verification pass

**Files:** none created — this task only runs checks.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace 2>&1 | tail -40`
Expected: every test across `forge-syntax` and `forge-ir` passes (lexer: 5, parser: 6, resolve: 3, typeck: 6, builder: 0 standalone, lower: 4, dominance/verify: 5, print: 1, interp: 7, e2e: 6, roundtrip: 1 property test = 38+ passing tests total). Stub crates report 0 tests, which is expected.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: clean. If warnings appear (e.g. needless clones from the `Expr`/`Inst` `.clone()` calls used throughout for borrow-checker convenience), fix them inline — prefer restructuring the borrow over `#[allow]`.

- [ ] **Step 3: Format check**

Run: `cargo fmt --check`
Expected: clean, or run `cargo fmt` and commit the diff separately.

- [ ] **Step 4: Re-confirm the day-one spike still works**

Run: `make spike`
Expected: `JIT works: f(42) = 42`.

- [ ] **Step 5: Final commit if Step 2/3 required fixes**

```bash
git add -A
git commit -m "chore: clippy/fmt cleanup"
```

- [ ] **Step 6: Report exit criteria status**

Confirm all 5 exit criteria from the design doc are met:
1. Spike runs and prints the expected output. ✅ (Step 4)
2. `cargo test --workspace` passes. ✅ (Step 1)
3. End-to-end integration test exists and passes. ✅ (Task 14)
4. Clippy/fmt clean. ✅ (Steps 2-3)
5. CI workflow exists (green pending push). ✅ (Task 3)

---

## Self-Review Notes (for whoever executes this plan)

- **Spec coverage:** Every 🔴 item in CHECKLIST.md Phases 0-3 that applies to the scalar-only, no-codegen scope has a task above. Deferred-by-design-doc items (array/`@vectorize` grammar, full differential-testing infra, CI matrix beyond macOS-arm64) are intentionally absent — see the design doc, not an oversight.
- **The shadowing fix (Task 6) is the trickiest part of this plan.** If a future change touches `builder.rs`'s `write_variable`/`read_variable`, re-check that `resolve.rs`'s alpha-renaming invariant (every bare name is unambiguous) still holds — that invariant is what lets the builder use a flat, non-scoped name→Value map safely.
- **Type consistency check:** `RtValue`/`Ty`/`Inst` names and shapes are identical across `ir.rs`, `builder.rs`, `lower.rs`, `verify.rs`, `print.rs`, `interp.rs` — confirmed by re-reading each task's code side by side during planning.
