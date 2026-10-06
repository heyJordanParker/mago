//! Each test lowers a PHP# file and checks the compiled file the bridge encodes from it, or the unit `sharp_lower`
//! returns, against the layout `ext/sharp` reads.

#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use std::ffi::c_char;
use std::slice;

use mago_sharp_bridge::sharp_kind;
use mago_sharp_bridge::sharp_lower;
use mago_sharp_bridge::sharp_node;
use mago_sharp_bridge::sharp_unit;
use mago_sharp_bridge::sharp_unit_free;
use mago_sharp_bridge::sharp_value;

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
