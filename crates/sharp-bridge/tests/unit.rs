//! Each test lowers a PHP# file and checks the compiled file the bridge encodes from it, or the unit `sharp_lower`
//! returns, against the layout `ext/sharp` reads.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::big_endian_bytes,
    clippy::little_endian_bytes
)]

use std::ffi::c_char;
use std::fmt::Write;
use std::fs;
use std::mem::align_of;
use std::mem::offset_of;
use std::mem::size_of;
use std::path::Path;
use std::process::Command;
use std::slice;

use mago_build_id::BUILD_ID;
use mago_sharp_bridge::SHARP_UNIT_ABI;
use mago_sharp_bridge::Unit;
use mago_sharp_bridge::sharp_kind;
use mago_sharp_bridge::sharp_lower;
use mago_sharp_bridge::sharp_node;
use mago_sharp_bridge::sharp_str;
use mago_sharp_bridge::sharp_unit;
use mago_sharp_bridge::sharp_unit_free;
use mago_sharp_bridge::sharp_value;
use mago_sharp_bridge::unit::FormatError;
use mago_sharp_bridge::unit::Input;
use mago_sharp_bridge::unit::Read;
use mago_sharp_bridge::unit::Reads;
use mago_sharp_bridge::unit::SHARP_UNIT_MAGIC;
use mago_sharp_bridge::unit::encode;
use mago_sharp_bridge::unit::header;
use mago_sharp_bridge::unit::key;
use mago_sharp_bridge::unit::sharp_input;
use mago_sharp_bridge::unit::sharp_unit_header;
use mago_sharp_bridge::unit::source_hash;
use xxhash_rust::xxh3::xxh3_128;

const SOURCE: &str = "namespace App.Tenant;\n\nclass Report\n{\n    public string title()\n    {\n        return \"weekly\";\n    }\n}\n";

/// A unit `sharp_lower` returned, freed on drop.
struct Lowered(*mut sharp_unit);

impl Lowered {
    fn new(source: &str) -> Self {
        let path = "src/Report.sharp";

        // SAFETY: both pointers point to as many bytes as their lengths say.
        Self(unsafe {
            sharp_lower(path.as_ptr().cast::<c_char>(), path.len(), source.as_ptr().cast::<c_char>(), source.len())
        })
    }

    fn unit(&self) -> &sharp_unit {
        // SAFETY: `sharp_lower` returns a valid unit, freed only on drop.
        unsafe { &*self.0 }
    }

    fn nodes(&self) -> &[sharp_node] {
        // SAFETY: the unit owns `node_count` nodes.
        unsafe { slice::from_raw_parts(self.unit().nodes, self.unit().node_count) }
    }

    fn texts(&self) -> &[u8] {
        // SAFETY: the unit owns `texts_size` bytes of texts.
        unsafe { slice::from_raw_parts(self.unit().texts.cast::<u8>(), self.unit().texts_size) }
    }

    fn children(&self) -> &[u32] {
        // SAFETY: the unit owns `children_count` children.
        unsafe { slice::from_raw_parts(self.unit().children, self.unit().children_count) }
    }

    fn lowered(&self) -> &Unit {
        // SAFETY: `sharp_lower` returns the `sharp_unit` that is the first field of a boxed, `#[repr(C)]` `Unit`.
        unsafe { &*self.0.cast::<Unit>() }
    }

    fn encode(&self, source: &str) -> Vec<u8> {
        encode(self.lowered(), source.as_bytes(), KEY, &inputs(), FACTS)
    }

    fn text(&self, text: sharp_str) -> &[u8] {
        &self.texts()[text.offset as usize..(text.offset + text.len) as usize]
    }
}

const KEY: [u8; 16] = *b"0123456789abcdef";

const FACTS: &[u8] = b"facts the engine skips";

fn inputs() -> Vec<Input> {
    vec![
        Input { path: b"src/Report.sharp".to_vec(), size: 98, mtime_ns: 1_700_000_000_123_456_789, hash: [7; 16] },
        Input { path: b"composer.lock".to_vec(), size: 4096, mtime_ns: -5, hash: [9; 16] },
    ]
}

/// A compiled file read back through the C layout, as `ext/sharp` reads it: copied to memory aligned for every
/// section, then cast.
struct Decoded {
    header: sharp_unit_header,
    inputs: Vec<sharp_input>,
    nodes: Vec<sharp_node>,
    children: Vec<u32>,
    texts: Vec<u8>,
    facts: Vec<u8>,
    /// Where each section starts: inputs, nodes, children, texts, facts, and the end.
    offsets: [usize; 6],
}

impl Decoded {
    fn new(bytes: &[u8]) -> Self {
        let mut memory = vec![0u64; bytes.len().div_ceil(8)];
        // SAFETY: `memory` holds at least `bytes.len()` bytes, and the two do not overlap.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), memory.as_mut_ptr().cast::<u8>(), bytes.len()) };

        assert!(bytes.len() >= size_of::<sharp_unit_header>(), "the file holds a whole header");
        // SAFETY: `memory` is 8-aligned, as the header is, and holds a whole header.
        let header = unsafe { *memory.as_ptr().cast::<sharp_unit_header>() };

        let inputs_at = size_of::<sharp_unit_header>();
        let nodes_at = inputs_at + size_of::<sharp_input>() * header.input_count as usize;
        let children_at = nodes_at + size_of::<sharp_node>() * header.node_count as usize;
        let texts_at = children_at + size_of::<u32>() * header.children_count as usize;
        let facts_at = texts_at + header.texts_size as usize;
        let end = facts_at + header.facts_size as usize;
        assert_eq!(end, bytes.len(), "the file is as long as its header says");

        Self {
            header,
            inputs: section(&memory, inputs_at, header.input_count as usize),
            nodes: section(&memory, nodes_at, header.node_count as usize),
            children: section(&memory, children_at, header.children_count as usize),
            texts: section(&memory, texts_at, header.texts_size as usize),
            facts: section(&memory, facts_at, header.facts_size as usize),
            offsets: [inputs_at, nodes_at, children_at, texts_at, facts_at, end],
        }
    }

    fn text(&self, text: sharp_str) -> &[u8] {
        &self.texts[text.offset as usize..(text.offset + text.len) as usize]
    }
}

/// The `count` values of `T` from `offset` in `memory`, after checking that `offset` is aligned for `T` and that the
/// values lie inside `memory`.
fn section<T>(memory: &[u64], offset: usize, count: usize) -> Vec<T>
where
    T: Copy,
{
    assert_eq!(offset % align_of::<T>(), 0, "a section starts aligned for its type");
    assert!(offset + count * size_of::<T>() <= size_of_val(memory), "a section lies inside the file");
    let start = memory.as_ptr().cast::<u8>().wrapping_add(offset).cast::<T>();

    // SAFETY: `start` is aligned for `T`, and `count` values of `T` lie inside `memory` from it.
    unsafe { slice::from_raw_parts(start, count) }.to_vec()
}

impl Drop for Lowered {
    fn drop(&mut self) {
        // SAFETY: `sharp_lower` returned the unit, and only this drop frees it.
        unsafe { sharp_unit_free(self.0) };
    }
}

#[test]
fn every_text_of_a_lowered_unit_is_an_offset_into_its_texts() {
    let lowered = Lowered::new(SOURCE);
    let texts = lowered.texts();

    let resolved: Vec<&[u8]> = lowered
        .nodes()
        .iter()
        .filter(|node| node.text.len != 0)
        .map(|node| &texts[node.text.offset as usize..(node.text.offset + node.text.len) as usize])
        .collect();

    assert_eq!(resolved, [&b"strict_types"[..], b"App\\Tenant", b"weekly", b"string", b"title", b"Report"]);
    assert!(lowered.nodes().iter().any(|node| node.kind == sharp_kind::SHARP_AST_ZVAL
        && node.value == sharp_value::SHARP_STRING
        && node.text.len == 6));
}

#[test]
fn a_compiled_file_reads_back_through_the_c_layout() {
    let lowered = Lowered::new(SOURCE);
    let decoded = Decoded::new(&lowered.encode(SOURCE));
    let header = decoded.header;

    assert_eq!(header.magic, *b"SHARPC\0\0");
    assert_eq!(header.magic, SHARP_UNIT_MAGIC);
    assert_eq!(header.abi, SHARP_UNIT_ABI);
    assert_eq!(header.checker, BUILD_ID.to_be_bytes());
    assert_eq!(header.key, KEY);
    assert_eq!(header.source_hash, xxh3_128(SOURCE.as_bytes()).to_be_bytes());
    assert_eq!(header.source_size, SOURCE.len() as u64);
    assert_eq!(header.root, lowered.unit().root);

    let read_inputs: Vec<(&[u8], u64, i64, [u8; 16])> =
        decoded.inputs.iter().map(|input| (decoded.text(input.path), input.size, input.mtime_ns, input.hash)).collect();
    assert_eq!(
        read_inputs,
        [
            (&b"src/Report.sharp"[..], 98, 1_700_000_000_123_456_789, [7; 16]),
            (&b"composer.lock"[..], 4096, -5, [9; 16]),
        ]
    );

    assert_eq!(decoded.nodes.len(), lowered.nodes().len());
    for (read, written) in decoded.nodes.iter().zip(lowered.nodes()) {
        assert_eq!(
            (read.kind, read.attr, read.line, read.end_line, read.first_child, read.child_count, read.value),
            (
                written.kind,
                written.attr,
                written.line,
                written.end_line,
                written.first_child,
                written.child_count,
                written.value
            ),
        );
        assert_eq!(
            (read.long_value, read.double_value.to_bits()),
            (written.long_value, written.double_value.to_bits())
        );
        assert_eq!(decoded.text(read.text), lowered.text(written.text));
    }

    let texts: Vec<&[u8]> =
        decoded.nodes.iter().filter(|node| node.text.len != 0).map(|node| decoded.text(node.text)).collect();
    assert_eq!(texts, [&b"strict_types"[..], b"App\\Tenant", b"weekly", b"string", b"title", b"Report"]);
    assert_eq!(decoded.children, lowered.children());
    assert_eq!(decoded.facts, FACTS);
}

#[test]
fn every_node_child_and_text_lies_inside_its_section() {
    let decoded = Decoded::new(&Lowered::new(SOURCE).encode(SOURCE));
    let nodes = decoded.nodes.len() as u64;
    let children = decoded.children.len() as u64;
    let texts = decoded.texts.len() as u64;
    let inside = |text: sharp_str| u64::from(text.offset) + u64::from(text.len) <= texts;

    assert!(u64::from(decoded.header.root) < nodes);
    for node in &decoded.nodes {
        assert!(u64::from(node.first_child) + u64::from(node.child_count) <= children, "{node:?}");
        assert!(inside(node.text), "{node:?}");
    }
    for &child in &decoded.children {
        assert!(child == u32::MAX || u64::from(child) < nodes, "child {child}");
    }
    for input in &decoded.inputs {
        assert!(inside(input.path), "{input:?}");
    }
    assert!(decoded.children.contains(&u32::MAX), "the source lowers a null child");
}

#[test]
fn every_header_field_and_section_starts_aligned_for_its_type() {
    for (field, offset, align) in [
        ("source_size", offset_of!(sharp_unit_header, source_size), align_of::<u64>()),
        ("input_count", offset_of!(sharp_unit_header, input_count), align_of::<u32>()),
        ("node_count", offset_of!(sharp_unit_header, node_count), align_of::<u32>()),
        ("children_count", offset_of!(sharp_unit_header, children_count), align_of::<u32>()),
        ("root", offset_of!(sharp_unit_header, root), align_of::<u32>()),
        ("texts_size", offset_of!(sharp_unit_header, texts_size), align_of::<u32>()),
        ("facts_size", offset_of!(sharp_unit_header, facts_size), align_of::<u32>()),
        ("sharp_input.size", offset_of!(sharp_input, size), align_of::<u64>()),
        ("sharp_input.mtime_ns", offset_of!(sharp_input, mtime_ns), align_of::<i64>()),
    ] {
        assert_eq!(offset % align, 0, "{field}");
    }

    let lowered = Lowered::new(SOURCE);
    for count in 0..=inputs().len() {
        let decoded = Decoded::new(&encode(lowered.lowered(), SOURCE.as_bytes(), KEY, &inputs()[..count], FACTS));
        let [inputs_at, nodes_at, children_at, ..] = decoded.offsets;

        assert_eq!(inputs_at % align_of::<sharp_input>(), 0, "{count} inputs");
        assert_eq!(nodes_at % align_of::<sharp_node>(), 0, "{count} inputs");
        assert_eq!(children_at % align_of::<u32>(), 0, "{count} inputs");
    }
}

#[test]
fn encoding_one_lowering_twice_gives_the_same_bytes_with_zero_padding() {
    let first = Lowered::new(SOURCE).encode(SOURCE);
    let second = Lowered::new(SOURCE).encode(SOURCE);
    assert_eq!(first, second);

    let decoded = Decoded::new(&first);
    let after_kind = offset_of!(sharp_node, kind) + size_of::<u16>()..offset_of!(sharp_node, attr);
    let after_value = offset_of!(sharp_node, value) + size_of::<u8>()..offset_of!(sharp_node, long_value);
    for index in 0..decoded.nodes.len() {
        let node = &first[decoded.offsets[1] + index * size_of::<sharp_node>()..][..size_of::<sharp_node>()];

        assert!(node[after_kind.clone()].iter().all(|&byte| byte == 0), "node {index}");
        assert!(node[after_value.clone()].iter().all(|&byte| byte == 0), "node {index}");
    }
}

#[test]
#[should_panic(expected = "a refused file gets no compiled file")]
fn encoding_a_refused_file_panics() {
    let source = "class Report\n{\n    public int run()\n    {\n        echo 1;\n    }\n}\n";

    let _ = Lowered::new(source).encode(source);
}

#[test]
fn header_reads_the_fields_of_a_current_file() {
    let bytes = Lowered::new(SOURCE).encode(SOURCE);
    let header = header(&bytes).unwrap();

    assert_eq!(header.abi, SHARP_UNIT_ABI);
    assert_eq!(header.checker, BUILD_ID.to_be_bytes());
    assert_eq!(header.key, KEY);
    assert_eq!(header.source_hash, source_hash(SOURCE.as_bytes()));
    assert_eq!(header.source_hash, xxh3_128(SOURCE.as_bytes()).to_be_bytes());
    assert_eq!(header.source_size, SOURCE.len() as u64);
}

#[test]
fn header_refuses_a_short_file_a_wrong_magic_a_wrong_abi_and_a_wrong_length() {
    let bytes = Lowered::new(SOURCE).encode(SOURCE);
    let changed = |offset: usize| {
        let mut changed = bytes.clone();
        changed[offset] ^= 1;
        changed
    };
    let mut longer = bytes.clone();
    longer.push(0);

    assert_eq!(header(&[]), Err(FormatError::TooShort));
    assert_eq!(header(&bytes[..size_of::<sharp_unit_header>() - 1]), Err(FormatError::TooShort));
    assert_eq!(header(&changed(0)), Err(FormatError::Magic));
    assert_eq!(header(&changed(offset_of!(sharp_unit_header, abi) + 15)), Err(FormatError::Abi));
    assert_eq!(header(&longer), Err(FormatError::Length));
    assert_eq!(header(&bytes[..bytes.len() - 1]), Err(FormatError::Length));
    assert_eq!(header(&changed(offset_of!(sharp_unit_header, node_count))), Err(FormatError::Length));
    assert_eq!(header(&changed(offset_of!(sharp_unit_header, facts_size))), Err(FormatError::Length));
}

fn read(name: &str, fingerprint: u64) -> Read {
    Read { name: name.as_bytes().to_vec(), fingerprint }
}

fn reads() -> Reads {
    Reads {
        signatures: vec![read("App\\Shared\\Money", 11), read("App\\Shared\\Money::add", 12)],
        bodies: vec![read("App\\Shared\\Money::total", 13)],
        inlined: vec![read("Sharp\\List::count", 31)],
    }
}

#[test]
fn changing_only_an_inlined_body_changes_the_key() {
    let source = source_hash(b"namespace App.Orders;");
    let mut changed = reads();
    changed.inlined[0].fingerprint = xxhash_rust::xxh3::xxh3_64(b"return \\count($this->items);");

    assert_ne!(key(source, &changed), key(source, &reads()));
}

#[test]
fn the_key_changes_with_the_source_and_with_each_read() {
    let source = source_hash(b"namespace App.Orders;");
    let base = key(source, &reads());
    assert_eq!(base, key(source, &reads()));

    let mut changes: Vec<(&str, [u8; 16], Reads)> = vec![("the source", source_hash(b"namespace App.Bills;"), reads())];
    for (change, edit) in [
        ("a signature's fingerprint", (|reads: &mut Reads| reads.signatures[0].fingerprint = 21) as fn(&mut Reads)),
        ("a signature's name", |reads| reads.signatures[0].name = b"App\\Shared\\Price".to_vec()),
        ("one more signature", |reads| reads.signatures.push(read("App\\Shared\\Currency", 14))),
        ("one signature fewer", |reads| drop(reads.signatures.pop())),
        ("an inferred return", |reads| reads.bodies[0].fingerprint = 23),
        ("a body's name", |reads| reads.bodies[0].name = b"App\\Shared\\Money::sum".to_vec()),
        ("a read moved between the lists", |reads| {
            let moved = reads.signatures.remove(1);
            reads.bodies.push(moved);
        }),
    ] {
        let mut edited = reads();
        edit(&mut edited);
        changes.push((change, source, edited));
    }

    for (change, source, reads) in changes {
        assert_ne!(key(source, &reads), base, "{change}");
    }
}

#[test]
fn the_key_ignores_the_order_and_repeats_of_reads() {
    let source = source_hash(b"namespace App.Orders;");
    let mut shuffled = reads();
    shuffled.signatures.reverse();
    shuffled.signatures.push(read("App\\Shared\\Money", 11));
    shuffled.bodies.push(read("App\\Shared\\Money::total", 13));

    assert_eq!(key(source, &shuffled), key(source, &reads()));
}

#[test]
fn a_body_edit_keeps_the_key_until_the_inferred_return_changes() {
    let source = source_hash(b"namespace App.Orders;");
    let total = |inferred_return: &str| Reads {
        signatures: vec![read("App\\Shared\\Money", 11)],
        bodies: vec![read("App\\Shared\\Money::total", xxhash_rust::xxh3::xxh3_64(inferred_return.as_bytes()))],
        inlined: Vec::new(),
    };
    let before = key(source, &total("int"));

    assert_eq!(key(source, &total("int")), before, "the body changed and still returns int");
    assert_ne!(key(source, &total("float")), before, "the body now returns float");
}

/// The C header cbindgen writes for `ext/sharp`.
const HEADER: &str = include_str!(concat!(env!("OUT_DIR"), "/sharp_unit.h"));

#[test]
#[cfg(unix)]
fn the_c_header_lays_out_every_type_as_the_bridge_does() {
    let check = tempfile::tempdir().unwrap();
    fs::write(check.path().join("sharp_unit.h"), HEADER).unwrap();
    let assertions = [
        ("sizeof(sharp_unit_header)", size_of::<sharp_unit_header>()),
        ("offsetof(sharp_unit_header, abi)", offset_of!(sharp_unit_header, abi)),
        ("offsetof(sharp_unit_header, checker)", offset_of!(sharp_unit_header, checker)),
        ("offsetof(sharp_unit_header, key)", offset_of!(sharp_unit_header, key)),
        ("offsetof(sharp_unit_header, source_hash)", offset_of!(sharp_unit_header, source_hash)),
        ("offsetof(sharp_unit_header, source_size)", offset_of!(sharp_unit_header, source_size)),
        ("offsetof(sharp_unit_header, input_count)", offset_of!(sharp_unit_header, input_count)),
        ("offsetof(sharp_unit_header, node_count)", offset_of!(sharp_unit_header, node_count)),
        ("offsetof(sharp_unit_header, children_count)", offset_of!(sharp_unit_header, children_count)),
        ("offsetof(sharp_unit_header, root)", offset_of!(sharp_unit_header, root)),
        ("offsetof(sharp_unit_header, texts_size)", offset_of!(sharp_unit_header, texts_size)),
        ("offsetof(sharp_unit_header, facts_size)", offset_of!(sharp_unit_header, facts_size)),
        ("sizeof(sharp_input)", size_of::<sharp_input>()),
        ("offsetof(sharp_input, size)", offset_of!(sharp_input, size)),
        ("offsetof(sharp_input, mtime_ns)", offset_of!(sharp_input, mtime_ns)),
        ("offsetof(sharp_input, hash)", offset_of!(sharp_input, hash)),
        ("sizeof(sharp_node)", size_of::<sharp_node>()),
        ("offsetof(sharp_node, attr)", offset_of!(sharp_node, attr)),
        ("offsetof(sharp_node, value)", offset_of!(sharp_node, value)),
        ("offsetof(sharp_node, long_value)", offset_of!(sharp_node, long_value)),
        ("offsetof(sharp_node, text)", offset_of!(sharp_node, text)),
        ("sizeof(sharp_str)", size_of::<sharp_str>()),
        ("sizeof(SHARP_UNIT_MAGIC) - 1", SHARP_UNIT_MAGIC.len()),
        ("sizeof(SHARP_UNIT_ABI) - 1", SHARP_UNIT_ABI.len()),
        ("sizeof(SHARP_MAGO_COMMIT) - 1", 40),
    ];
    let mut source = String::from("#include <stddef.h>\n#include \"sharp_unit.h\"\n");
    for (expression, value) in assertions {
        let _ = writeln!(source, "_Static_assert({expression} == {value}, \"{expression}\");");
    }
    fs::write(check.path().join("check.c"), source).unwrap();

    let output = Command::new("cc")
        .args(["-std=c11", "-fsyntax-only", "-Werror"])
        .arg(check.path().join("check.c"))
        .output()
        .expect("cc runs");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn the_c_header_carries_the_magic_the_abi_and_the_mago_commit() {
    let define = |name: &str| {
        HEADER
            .lines()
            .find_map(|line| line.strip_prefix(&format!("#define {name} ")))
            .unwrap_or_else(|| panic!("the header defines {name}"))
            .to_owned()
    };
    let bytes = |bytes: &[u8]| -> String {
        let mut literal = String::from("\"");
        for byte in bytes {
            let _ = write!(literal, "\\x{byte:02x}");
        }
        literal.push('"');

        literal
    };
    let commit = String::from_utf8(
        Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repository()).output().unwrap().stdout,
    )
    .unwrap();

    assert_eq!(define("SHARP_UNIT_MAGIC"), bytes(b"SHARPC\0\0"));
    assert_eq!(define("SHARP_UNIT_ABI"), bytes(&SHARP_UNIT_ABI));
    assert_eq!(define("SHARP_MAGO_COMMIT"), format!("\"{}\"", commit.trim()));
    assert!(HEADER.contains("#error"), "the header refuses a big-endian target");
}

fn repository() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Whether `php` runs. `MAGO_REQUIRE_PHP_SDK_TESTS` makes its absence a failure, as in Mago's PHP SDK tests.
fn php_is_available(test: &str) -> bool {
    let available = Command::new("php").arg("--version").output().is_ok_and(|output| output.status.success());
    assert!(available || std::env::var_os("MAGO_REQUIRE_PHP_SDK_TESTS").is_none(), "PHP is required for {test}");

    available
}

const ZEND_AST_H: &str = "#define ZEND_AST_SPECIAL_SHIFT      6
#define ZEND_AST_IS_LIST_SHIFT      7

enum _zend_ast_kind {
\t/* special nodes */
\tZEND_AST_ZVAL = 1 << ZEND_AST_SPECIAL_SHIFT,
\tZEND_AST_CONSTANT,

\t/* list nodes */
\tZEND_AST_ARG_LIST = 1 << ZEND_AST_IS_LIST_SHIFT,
};
";

const ZEND_COMPILE_H: &str = "#define ZEND_ISEMPTY\t\t\t(1<<0)

/* PHP# marks
 *
 * PHP# marks AST nodes and oplines in fields php-src already fills.
 *
 * ZEND_SHARP_OPERATOR_SYNTAX: the operator follows PHP#'s rules.
 *   attr of ZEND_AST_BINARY_OP, ZEND_AST_ASSIGN_OP          upstream: the opcode
 *
 * ZEND_SHARP_OPERATOR: the compiler turns ZEND_SHARP_OPERATOR_SYNTAX into it.
 *   extended_value of ZEND_ADD, ZEND_SUB                    upstream: none
 */
#define ZEND_SHARP_OPERATOR_SYNTAX\t(1<<15)
#define ZEND_SHARP_OPERATOR\t(1<<30)

#define ZEND_LAST_CATCH\t\t\t(1<<0)
";

/// The `SHARP_UNIT_ABI` line `just regen-sharp-kinds` prints for the two headers, run in a copy of the repository's
/// script and bridge sources after `edit` changes one of those sources.
fn generated_abi(zend_ast: &str, zend_compile: &str, edit: Option<(&str, &str, &str)>) -> String {
    let root = tempfile::tempdir().unwrap();
    let sources = root.path().join("crates/sharp-bridge/src");
    fs::create_dir_all(root.path().join("scripts")).unwrap();
    fs::create_dir_all(&sources).unwrap();
    fs::copy(repository().join("scripts/regen-sharp-kinds.php"), root.path().join("scripts/regen-sharp-kinds.php"))
        .unwrap();
    for file in ["lib.rs", "unit.rs"] {
        fs::copy(repository().join("crates/sharp-bridge/src").join(file), sources.join(file)).unwrap();
    }
    if let Some((file, from, to)) = edit {
        let source = fs::read_to_string(sources.join(file)).unwrap();
        assert!(source.contains(from), "{file} contains `{from}`");
        fs::write(sources.join(file), source.replacen(from, to, 1)).unwrap();
    }
    fs::write(root.path().join("zend_ast.h"), zend_ast).unwrap();
    fs::write(root.path().join("zend_compile.h"), zend_compile).unwrap();

    let output = Command::new("php")
        .arg(root.path().join("scripts/regen-sharp-kinds.php"))
        .arg(root.path().join("zend_ast.h"))
        .arg(root.path().join("zend_compile.h"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .find(|line| line.starts_with("pub const SHARP_UNIT_ABI: [u8; 16] = 0x"))
        .expect("the generator prints SHARP_UNIT_ABI")
        .to_owned()
}

#[test]
fn the_unit_abi_changes_with_the_kind_table_the_layouts_and_the_mark_register() {
    if !php_is_available("the_unit_abi_changes_with_the_kind_table_the_layouts_and_the_mark_register") {
        return;
    }

    let base = generated_abi(ZEND_AST_H, ZEND_COMPILE_H, None);
    assert_eq!(generated_abi(ZEND_AST_H, ZEND_COMPILE_H, None), base);
    assert_eq!(
        generated_abi(ZEND_AST_H, ZEND_COMPILE_H, Some(("lib.rs", "/// First line.", "/// The first line."))),
        base,
        "a comment is not layout"
    );

    for (change, abi) in [
        (
            "a kind",
            generated_abi(
                &ZEND_AST_H.replace("ZEND_AST_CONSTANT,", "ZEND_AST_CONSTANT,\n\tZEND_AST_ZNODE,"),
                ZEND_COMPILE_H,
                None,
            ),
        ),
        (
            "a sharp_node field",
            generated_abi(ZEND_AST_H, ZEND_COMPILE_H, Some(("lib.rs", "pub line: u32,", "pub line: u64,"))),
        ),
        (
            "a sharp_value case",
            generated_abi(ZEND_AST_H, ZEND_COMPILE_H, Some(("lib.rs", "SHARP_LONG,", "SHARP_LONG,\n    SHARP_ARRAY,"))),
        ),
        (
            "a sharp_str field",
            generated_abi(ZEND_AST_H, ZEND_COMPILE_H, Some(("lib.rs", "pub len: u32,", "pub len: u64,"))),
        ),
        (
            "a header field",
            generated_abi(
                ZEND_AST_H,
                ZEND_COMPILE_H,
                Some(("unit.rs", "pub facts_size: u32,", "pub facts_size: u64,")),
            ),
        ),
        (
            "a sharp_input field",
            generated_abi(ZEND_AST_H, ZEND_COMPILE_H, Some(("unit.rs", "pub mtime_ns: i64,", "pub mtime_ns: u64,"))),
        ),
        ("a mark's bit", generated_abi(ZEND_AST_H, &ZEND_COMPILE_H.replace("(1<<15)", "(1<<16)"), None)),
        (
            "a mark's field",
            generated_abi(
                ZEND_AST_H,
                &ZEND_COMPILE_H.replace("extended_value of ZEND_ADD", "result_type of ZEND_ADD"),
                None,
            ),
        ),
    ] {
        assert_ne!(abi, base, "{change}");
    }
}
