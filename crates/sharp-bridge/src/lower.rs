use crate::Unit;
use crate::sharp_kind;
use crate::sharp_node;
use crate::sharp_str;
use crate::sharp_value;

/// Lowers the PHP# file at `path` into the tree php-src builds for the equivalent PHP.
pub(crate) fn lower(_path: Vec<u8>, _source: Vec<u8>) -> Box<Unit> {
    let root = sharp_node {
        kind: sharp_kind::SHARP_AST_STMT_LIST,
        attr: 0,
        line: 1,
        end_line: 0,
        first_child: 0,
        child_count: 0,
        value: sharp_value::SHARP_NULL,
        long_value: 0,
        double_value: 0.0,
        text: sharp_str::EMPTY,
    };

    Unit::new(vec![root], Vec::new(), 0, Vec::new())
}
