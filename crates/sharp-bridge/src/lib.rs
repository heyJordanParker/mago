//! The bridge from Mago's PHP# front end to the php-sharp engine.
//!
//! `sharp_lower` runs one `.sharp` file through the parser, the binder and the semantic checks, and lowers it into
//! the tree php-src's own parser builds for the equivalent PHP. The tree is a flat node array behind a C ABI, which
//! `ext/sharp` turns into `zend_ast` the way HHVM's `hackc-translator.cpp` turns hackc's unit into runtime structures.
//! cbindgen writes this file's ABI to `sharp_bridge.h` in `OUT_DIR`.

#![allow(non_camel_case_types)]

use std::ffi::c_char;
use std::panic;
use std::panic::AssertUnwindSafe;
use std::ptr;
use std::slice;

mod lower;

/// UTF-8, not NUL-terminated.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct sharp_str {
    pub ptr: *const c_char,
    pub len: usize,
}

/// One value per `zend_ast_kind` the lowering emits, named as that kind without `ZEND_`. C maps them by table.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum sharp_kind {
    SHARP_AST_ZVAL,
    SHARP_AST_METHOD,
    SHARP_AST_CLASS,
    SHARP_AST_ARG_LIST,
    SHARP_AST_STMT_LIST,
    SHARP_AST_PARAM_LIST,
    SHARP_AST_CONST_DECL,
    SHARP_AST_VAR,
    SHARP_AST_CONST,
    SHARP_AST_UNARY_PLUS,
    SHARP_AST_UNARY_MINUS,
    SHARP_AST_UNARY_OP,
    SHARP_AST_PRE_INC,
    SHARP_AST_PRE_DEC,
    SHARP_AST_POST_INC,
    SHARP_AST_POST_DEC,
    SHARP_AST_RETURN,
    SHARP_AST_PROP,
    SHARP_AST_ASSIGN,
    SHARP_AST_ASSIGN_OP,
    SHARP_AST_BINARY_OP,
    SHARP_AST_GREATER,
    SHARP_AST_GREATER_EQUAL,
    SHARP_AST_AND,
    SHARP_AST_OR,
    SHARP_AST_DECLARE,
    SHARP_AST_NAMESPACE,
    SHARP_AST_NAMED_ARG,
    SHARP_AST_METHOD_CALL,
    SHARP_AST_STATIC_CALL,
    SHARP_AST_CONST_ELEM,
    SHARP_AST_PARAM,
    SHARP_AST_IF,
    SHARP_AST_IF_ELEM,
    SHARP_AST_WHILE,
    SHARP_AST_DO_WHILE,
    SHARP_AST_BREAK,
    SHARP_AST_CONTINUE,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum sharp_value {
    SHARP_NULL,
    SHARP_FALSE,
    SHARP_TRUE,
    SHARP_LONG,
    SHARP_DOUBLE,
    SHARP_STRING,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct sharp_node {
    pub kind: sharp_kind,
    /// `zend_ast` attr: flags, modifiers, operator, `ZEND_NAME_FQ`.
    pub attr: u32,
    /// First line.
    pub line: u32,
    /// Closing line, for declarations.
    pub end_line: u32,
    /// Index into `children[]`.
    pub first_child: u32,
    /// List kinds are variadic.
    pub child_count: u32,
    /// `SHARP_AST_ZVAL` only.
    pub value: sharp_value,
    pub long_value: i64,
    pub double_value: f64,
    /// Names, string values, doc comments.
    pub text: sharp_str,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum sharp_severity {
    SHARP_PARSE_ERROR,
    SHARP_COMPILE_ERROR,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct sharp_diagnostic {
    pub line: u32,
    pub column: u32,
    pub severity: sharp_severity,
    pub message: sharp_str,
}

#[repr(C)]
#[derive(Debug)]
pub struct sharp_unit {
    pub nodes: *const sharp_node,
    pub node_count: usize,
    /// `UINT32_MAX` is a null child.
    pub children: *const u32,
    pub children_count: usize,
    /// A `SHARP_AST_STMT_LIST`.
    pub root: u32,
    /// Non-empty: nodes are empty.
    pub diagnostics: *const sharp_diagnostic,
    pub diagnostic_count: usize,
}

/// MINIT: installs the silent panic hook.
#[unsafe(no_mangle)]
pub extern "C" fn sharp_init() {
    panic::set_hook(Box::new(|_| {}));
}

/// Lowers one `.sharp` file. Free the result with `sharp_unit_free`.
///
/// # Safety
///
/// `path` and `source` point to `path_len` and `source_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharp_lower(
    path: *const c_char,
    path_len: usize,
    source: *const c_char,
    source_len: usize,
) -> *mut sharp_unit {
    // SAFETY: the caller passes `path_len` readable bytes at `path`.
    let path = unsafe { bytes(path, path_len) };
    // SAFETY: the caller passes `source_len` readable bytes at `source`.
    let source = unsafe { bytes(source, source_len) };

    Box::into_raw(catch_panic(|| lower::lower(path, source))).cast::<sharp_unit>()
}

/// # Safety
///
/// `unit` is null, or a unit `sharp_lower` returned that is not freed yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharp_unit_free(unit: *mut sharp_unit) {
    if !unit.is_null() {
        // SAFETY: `sharp_lower` returned `unit` from `Box::into_raw` of a `Unit`, whose first field it points to.
        drop(unsafe { Box::from_raw(unit.cast::<Unit>()) });
    }
}

/// For `php --ri sharp`.
#[unsafe(no_mangle)]
pub extern "C" fn sharp_mago_commit() -> sharp_str {
    const COMMIT: &str = env!("SHARP_MAGO_COMMIT");

    sharp_str { ptr: COMMIT.as_ptr().cast::<c_char>(), len: COMMIT.len() }
}

/// A lowered file and every byte its `sharp_unit` points to, which the bridge owns until `sharp_unit_free`.
#[repr(C)]
struct Unit {
    abi: sharp_unit,
    nodes: Vec<sharp_node>,
    children: Vec<u32>,
    diagnostics: Vec<sharp_diagnostic>,
    texts: Vec<Box<[u8]>>,
}

/// A diagnostic before the bridge stores its message.
struct Diagnostic {
    line: u32,
    column: u32,
    severity: sharp_severity,
    message: String,
}

impl Unit {
    fn failed(diagnostics: Vec<Diagnostic>) -> Box<Self> {
        let mut texts = Vec::with_capacity(diagnostics.len());
        let diagnostics = diagnostics
            .into_iter()
            .map(|diagnostic| sharp_diagnostic {
                line: diagnostic.line,
                column: diagnostic.column,
                severity: diagnostic.severity,
                message: store_text(&mut texts, diagnostic.message.into_bytes()),
            })
            .collect();

        Self::boxed(Vec::new(), Vec::new(), 0, diagnostics, texts)
    }

    fn boxed(
        nodes: Vec<sharp_node>,
        children: Vec<u32>,
        root: u32,
        diagnostics: Vec<sharp_diagnostic>,
        texts: Vec<Box<[u8]>>,
    ) -> Box<Self> {
        let abi = sharp_unit {
            nodes: nodes.as_ptr(),
            node_count: nodes.len(),
            children: children.as_ptr(),
            children_count: children.len(),
            root,
            diagnostics: diagnostics.as_ptr(),
            diagnostic_count: diagnostics.len(),
        };

        Box::new(Self { abi, nodes, children, diagnostics, texts })
    }
}

/// Keeps `bytes` in `texts`, where they never move, and returns the string that points to them.
fn store_text(texts: &mut Vec<Box<[u8]>>, bytes: Vec<u8>) -> sharp_str {
    let text = bytes.into_boxed_slice();
    let stored = sharp_str { ptr: text.as_ptr().cast::<c_char>(), len: text.len() };
    texts.push(text);

    stored
}

/// Runs the lowering, and returns a panic inside it as one compile error, so the engine never unwinds.
fn catch_panic(lower: impl FnOnce() -> Box<Unit>) -> Box<Unit> {
    panic::catch_unwind(AssertUnwindSafe(lower)).unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|message| (*message).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default();

        Unit::failed(vec![Diagnostic {
            line: 0,
            column: 0,
            severity: sharp_severity::SHARP_COMPILE_ERROR,
            message: format!("internal error in the PHP# front end: {message}"),
        }])
    })
}

/// Copies `len` bytes from `pointer`, which may be null when `len` is 0.
///
/// # Safety
///
/// When `len` is not 0, `pointer` points to `len` readable bytes.
unsafe fn bytes(pointer: *const c_char, len: usize) -> Vec<u8> {
    if len == 0 {
        return Vec::new();
    }

    // SAFETY: the caller passes `len` readable bytes at `pointer`.
    unsafe { slice::from_raw_parts(pointer.cast::<u8>(), len) }.to_vec()
}

impl sharp_str {
    const EMPTY: Self = Self { ptr: ptr::null(), len: 0 };
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]

    use super::*;

    #[test]
    fn catch_panic_returns_one_compile_error() {
        let unit = catch_panic(|| panic!("forced"));

        assert_eq!(unit.abi.node_count, 0);
        assert_eq!(unit.diagnostics.len(), 1);
        assert_eq!(unit.diagnostics[0].severity, sharp_severity::SHARP_COMPILE_ERROR);
        assert_eq!(&*unit.texts[0], b"internal error in the PHP# front end: forced");
    }
}
