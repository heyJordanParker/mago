use std::io::Write;

use mago_database::ReadDatabase;

use crate::IssueCollection;
use crate::Level;
use crate::color::ColorChoice;
use crate::error::ReportingError;

pub mod ariadne;
pub mod checkstyle;
pub mod code_count;
pub mod count;
pub mod emacs;
pub mod github;
#[cfg(feature = "serde")]
pub mod gitlab;
#[cfg(feature = "serde")]
pub mod json;
pub mod medium;
pub mod rich;
#[cfg(feature = "serde")]
pub mod sarif;
pub mod short;
pub mod utils;

/// Configuration for formatters.
#[derive(Debug, Clone)]
pub struct FormatterConfig {
    /// Choice for colorizing output.
    pub color_choice: ColorChoice,
    /// Whether to sort issues before formatting.
    pub sort: bool,
    /// Minimum report level (filter out lower severity issues).
    pub minimum_level: Option<Level>,
    /// Whether to filter to only fixable issues.
    pub filter_fixable: bool,
    /// Optional editor URL template for OSC 8 terminal hyperlinks.
    ///
    /// Supported placeholders: `%file%` (absolute path), `%rel_file%` (workspace-relative path), `%line%`, `%column%`.
    /// Example: `"phpstorm://open?file=%file%&line=%line%"`
    pub editor_url: Option<String>,
}

/// Trait for formatting issues to a writer.
pub trait Formatter {
    /// Format issues and write them to the provided writer.
    ///
    /// # Arguments
    ///
    /// * `writer` - The writer to output formatted issues to
    /// * `issues` - The collection of issues to format
    /// * `database` - The read database for accessing source files
    /// * `config` - Configuration for formatting behavior
    ///
    /// # Errors
    ///
    /// Returns an error if formatting or writing fails.
    fn format(
        &self,
        writer: &mut dyn Write,
        issues: &IssueCollection,
        database: &ReadDatabase,
        config: &FormatterConfig,
    ) -> Result<(), ReportingError>;
}

/// The format to use for reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::EnumString, strum::VariantNames, Default)]
#[strum(serialize_all = "kebab-case")]
pub enum ReportingFormat {
    /// Rich diagnostic format with full context.
    #[default]
    Rich,
    /// Medium diagnostic format with balanced context.
    Medium,
    /// Short diagnostic format with minimal context.
    Short,
    /// Ariadne diagnostic format.
    Ariadne,
    /// GitHub Actions format.
    Github,
    /// GitLab Code Quality format.
    #[cfg(feature = "serde")]
    Gitlab,
    /// JSON format.
    #[cfg(feature = "serde")]
    Json,
    /// Issue count by severity.
    Count,
    /// Issue count by code.
    CodeCount,
    /// Checkstyle XML format.
    Checkstyle,
    /// Emacs compilation mode format.
    Emacs,
    /// SARIF format (Static Analysis Results Interchange Format).
    #[cfg(feature = "serde")]
    Sarif,
}

impl ReportingFormat {
    #[must_use]
    pub const fn requires_output_when_empty(self) -> bool {
        match self {
            Self::Checkstyle => true,
            #[cfg(feature = "serde")]
            Self::Gitlab | Self::Json | Self::Sarif => true,
            _ => false,
        }
    }
}

/// Dispatch to the appropriate formatter based on the format type.
///
/// This function performs static dispatch using enum matching for optimal performance.
pub(crate) fn dispatch_format(
    format: ReportingFormat,
    writer: &mut dyn Write,
    issues: &IssueCollection,
    database: &ReadDatabase,
    config: &FormatterConfig,
) -> Result<(), ReportingError> {
    match format {
        ReportingFormat::Rich => rich::RichFormatter.format(writer, issues, database, config),
        ReportingFormat::Medium => medium::MediumFormatter.format(writer, issues, database, config),
        ReportingFormat::Short => short::ShortFormatter.format(writer, issues, database, config),
        ReportingFormat::Ariadne => ariadne::AriadneFormatter.format(writer, issues, database, config),
        #[cfg(feature = "serde")]
        ReportingFormat::Json => json::JsonFormatter.format(writer, issues, database, config),
        ReportingFormat::Github => github::GithubFormatter.format(writer, issues, database, config),
        #[cfg(feature = "serde")]
        ReportingFormat::Gitlab => gitlab::GitlabFormatter.format(writer, issues, database, config),
        ReportingFormat::Checkstyle => checkstyle::CheckstyleFormatter.format(writer, issues, database, config),
        ReportingFormat::Emacs => emacs::EmacsFormatter.format(writer, issues, database, config),
        ReportingFormat::Count => count::CountFormatter.format(writer, issues, database, config),
        ReportingFormat::CodeCount => code_count::CodeCountFormatter.format(writer, issues, database, config),
        #[cfg(feature = "serde")]
        ReportingFormat::Sarif => sarif::SarifFormatter.format(writer, issues, database, config),
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::path::Path;
    use std::path::PathBuf;

    use diffy::PatchFormatter;
    use mago_database::Database;
    use mago_database::DatabaseConfiguration;
    use mago_database::file::File;
    use mago_database::file::FileType;
    use mago_span::Position;
    use mago_span::Span;

    use crate::Annotation;
    use crate::Issue;
    use crate::IssueCollection;
    use crate::color::ColorChoice;

    use super::FormatterConfig;
    use super::ReportingFormat;
    use super::dispatch_format;

    #[test]
    fn the_renderers_that_show_notes_color_a_type_diff_note_when_colors_are_on() {
        let file = File::new(
            Cow::Borrowed(b"src/Foo.php"),
            FileType::Host,
            Some(PathBuf::from("/workspace/src/Foo.php")),
            Cow::Borrowed(b"<?php\ntake([]);\n"),
        );
        let span = Span::new(file.id, Position::new(6), Position::new(14));
        let configuration =
            DatabaseConfiguration::new(Path::new("/workspace"), vec![], vec![], vec![], vec![]).into_static();
        let database = Database::single(file, configuration).read_only();

        let patch = diffy::create_patch("list<\n    int|string|float|bool\n>", "list<\n    int|string|float|null\n>");
        let formatter = PatchFormatter::new().missing_newline_message(false).suppress_blank_empty(false);
        let plain = formatter.fmt_patch(&patch).to_string();
        let colored = formatter.with_color().fmt_patch(&patch).to_string();
        let issues = IssueCollection::from(vec![
            Issue::error("Argument type mismatch.").with_annotation(Annotation::primary(span)).with_note(plain),
        ]);
        let config = FormatterConfig {
            color_choice: ColorChoice::Always,
            sort: false,
            minimum_level: None,
            filter_fixable: false,
            editor_url: None,
        };

        for format in [ReportingFormat::Rich, ReportingFormat::Medium, ReportingFormat::Ariadne] {
            let mut output = Vec::new();
            let Ok(()) = dispatch_format(format, &mut output, &issues, &database, &config) else {
                panic!("{format} should format the issue");
            };
            let output = String::from_utf8_lossy(&output);

            for line in colored.lines().filter(|line| line.contains('\x1b')) {
                assert!(output.contains(line), "{format} output lacks the colored line {line:?}:\n{output}");
            }
        }
    }
}
