use diffy::PatchFormatter;

use mago_codex::ttype::TType;
use mago_codex::ttype::union::TUnion;

pub mod availability;
pub mod casing;
pub mod conditional;
pub mod docblock;
pub mod experimental;
pub mod expression;
pub mod misc;
pub mod missing_type_hints;
pub mod names;
pub mod php_emulation;
pub mod symbol_existence;
pub mod template;

/// Generates a diff between two complex types if both are complex.
///
/// The diff is plain text, so it reads the same in every output format. A renderer colors it.
///
/// # Arguments
///
/// * `container` - The first type to compare (e.g., a parameter type).
/// * `input` - The second type to compare (e.g., an argument type).
///
/// # Returns
///
/// An `Option<String>` containing the formatted diff if both types are complex, or `None` otherwise.
pub fn get_type_diff(container: &TUnion, input: &TUnion) -> Option<String> {
    if !container.is_complex() || !input.is_complex() {
        return None;
    }

    let formatter = PatchFormatter::new().missing_newline_message(false).suppress_blank_empty(false);

    let container_id = container.get_pretty_id().to_string();
    let input_id = input.get_pretty_id().to_string();
    let patch = diffy::create_patch(&container_id, &input_id);
    let diff = formatter.fmt_patch(&patch);

    Some(format!("{diff}"))
}

#[cfg(test)]
mod tests {
    use mago_codex::ttype::get_bool;
    use mago_codex::ttype::get_float;
    use mago_codex::ttype::get_int;
    use mago_codex::ttype::get_list;
    use mago_codex::ttype::get_null;
    use mago_codex::ttype::get_string;
    use mago_codex::ttype::union::TUnion;

    use super::get_type_diff;

    fn list_of(element_types: Vec<TUnion>) -> TUnion {
        get_list(TUnion::from_vec(element_types.into_iter().map(TUnion::get_single_owned).collect()))
    }

    #[test]
    fn a_type_diff_is_a_plain_unified_diff_whatever_the_output_colors() {
        let container = list_of(vec![get_int(), get_string(), get_float(), get_bool()]);
        let input = list_of(vec![get_int(), get_string(), get_float(), get_null()]);

        let Some(diff) = get_type_diff(&container, &input) else {
            panic!("both types are complex");
        };

        assert!(diff.starts_with("--- original\n+++ modified\n"), "{diff}");
        assert!(!diff.contains('\x1b'), "{diff:?}");
    }
}
