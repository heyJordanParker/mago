use mago_extension::PayloadError;
use mago_extension::WorkerError;

#[derive(Debug)]
pub enum ExternalAnalyzerError {
    InitializationUnavailable,
    InitializationPanicked,
    Worker(WorkerError),
    Protocol(String),
    InconsistentRegistration,
    InconsistentInitialization,
    DuplicateExtension(String),
    DuplicatePluginSelector {
        selector: String,
        first: String,
        second: String,
    },
    /// The extension host at `index`, in the order its pool was given, failed to register.
    Host {
        index: usize,
        name: Option<String>,
        source: Box<ExternalAnalyzerError>,
    },
}

impl ExternalAnalyzerError {
    #[must_use]
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol(message.into())
    }

    /// Names the extension host a registration error came from, given every host's name in the
    /// order its pool was given.
    #[must_use]
    pub fn with_host_names(self, names: &[String]) -> Self {
        match self {
            Self::Host { index, source, .. } => Self::Host { index, name: names.get(index).cloned(), source },
            error => error,
        }
    }
}

impl std::fmt::Display for ExternalAnalyzerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InitializationUnavailable => {
                formatter.write_str("external analyzer initialization thread is unavailable")
            }
            Self::InitializationPanicked => formatter.write_str("external analyzer initialization thread panicked"),
            Self::Worker(error) => write!(formatter, "external analyzer worker failed: {error}"),
            Self::Protocol(message) => write!(formatter, "external analyzer protocol error: {message}"),
            Self::InconsistentRegistration => {
                formatter.write_str("workers in an extension pool advertised different analyzer registrations")
            }
            Self::InconsistentInitialization => {
                formatter.write_str("workers in an extension pool produced different analyzer initialization stubs")
            }
            Self::DuplicateExtension(identifier) => {
                write!(formatter, "external extension `{identifier}` is registered by more than one host")
            }
            Self::DuplicatePluginSelector { selector, first, second } => {
                write!(formatter, "analyzer plugin selector `{selector}` is shared by plugins `{first}` and `{second}`")
            }
            Self::Host { name: Some(name), source, .. } => write!(formatter, "extension host \"{name}\": {source}"),
            Self::Host { index, name: None, source } => write!(formatter, "extension host {index}: {source}"),
        }
    }
}

impl std::error::Error for ExternalAnalyzerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Worker(error) => Some(error),
            Self::Host { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<WorkerError> for ExternalAnalyzerError {
    fn from(error: WorkerError) -> Self {
        Self::Worker(error)
    }
}

impl From<PayloadError> for ExternalAnalyzerError {
    fn from(error: PayloadError) -> Self {
        Self::Protocol(error.to_string())
    }
}

pub(super) fn protocol(message: impl Into<String>) -> ExternalAnalyzerError {
    ExternalAnalyzerError::Protocol(message.into())
}
