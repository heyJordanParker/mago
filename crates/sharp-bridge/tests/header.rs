/// The bridge ABI in php-sharp's `sharp/agents/architecture.md`, as cbindgen writes it for C.
const ABI: &str = "#ifndef SHARP_BRIDGE_H
#define SHARP_BRIDGE_H

#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

// One value per `zend_ast_kind` the lowering emits, named as that kind without `ZEND_`. C maps them by table.
enum sharp_kind
#if __STDC_VERSION__ >= 202311L
  : uint16_t
#endif // __STDC_VERSION__ >= 202311L
 {
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
};
#if __STDC_VERSION__ >= 202311L
typedef enum sharp_kind sharp_kind;
#else
typedef uint16_t sharp_kind;
#endif // __STDC_VERSION__ >= 202311L

enum sharp_value
#if __STDC_VERSION__ >= 202311L
  : uint8_t
#endif // __STDC_VERSION__ >= 202311L
 {
  SHARP_NULL,
  SHARP_FALSE,
  SHARP_TRUE,
  SHARP_LONG,
  SHARP_DOUBLE,
  SHARP_STRING,
};
#if __STDC_VERSION__ >= 202311L
typedef enum sharp_value sharp_value;
#else
typedef uint8_t sharp_value;
#endif // __STDC_VERSION__ >= 202311L

enum sharp_severity
#if __STDC_VERSION__ >= 202311L
  : uint8_t
#endif // __STDC_VERSION__ >= 202311L
 {
  SHARP_PARSE_ERROR,
  SHARP_COMPILE_ERROR,
};
#if __STDC_VERSION__ >= 202311L
typedef enum sharp_severity sharp_severity;
#else
typedef uint8_t sharp_severity;
#endif // __STDC_VERSION__ >= 202311L

// UTF-8, not NUL-terminated.
typedef struct {
  const char *ptr;
  size_t len;
} sharp_str;

typedef struct {
  sharp_kind kind;
  // `zend_ast` attr: flags, modifiers, operator, `ZEND_NAME_FQ`.
  uint32_t attr;
  // First line.
  uint32_t line;
  // Closing line, for declarations.
  uint32_t end_line;
  // Index into `children[]`.
  uint32_t first_child;
  // List kinds are variadic.
  uint32_t child_count;
  // `SHARP_AST_ZVAL` only.
  sharp_value value;
  int64_t long_value;
  double double_value;
  // Names, string values, doc comments.
  sharp_str text;
} sharp_node;

typedef struct {
  uint32_t line;
  uint32_t column;
  sharp_severity severity;
  sharp_str message;
} sharp_diagnostic;

typedef struct {
  const sharp_node *nodes;
  size_t node_count;
  // `UINT32_MAX` is a null child.
  const uint32_t *children;
  size_t children_count;
  // A `SHARP_AST_STMT_LIST`.
  uint32_t root;
  // Non-empty: nodes are empty.
  const sharp_diagnostic *diagnostics;
  size_t diagnostic_count;
} sharp_unit;

// MINIT: installs the silent panic hook.
void sharp_init(void);

// Lowers one `.sharp` file. Free the result with `sharp_unit_free`.
//
// # Safety
//
// `path` and `source` point to `path_len` and `source_len` readable bytes.
sharp_unit *sharp_lower(const char *path, size_t path_len, const char *source, size_t source_len);

// # Safety
//
// `unit` is null, or a unit `sharp_lower` returned that is not freed yet.
void sharp_unit_free(sharp_unit *unit);

// For `php --ri sharp`.
sharp_str sharp_mago_commit(void);

#endif  /* SHARP_BRIDGE_H */
";

#[test]
fn the_header_in_out_dir_declares_the_bridge_abi() {
    let header = include_str!(concat!(env!("OUT_DIR"), "/sharp_bridge.h"));

    assert_eq!(header, ABI);
}
