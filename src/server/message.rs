use std::io::Read;
use std::io::Write;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;

use mago_database::file::File;
use mago_extension::Frame;
use mago_reporting::IssueCollection;

use crate::config::Configuration;
use crate::consts::CURRENT_DIR;
use crate::error::Error;

/// The version of the request and response bodies below. A daemon answers only clients of its own
/// build, so the version changes only with the build.
pub(crate) const PROTOCOL: u16 = 1;

const MAXIMUM_BODY_SIZE: usize = u32::MAX as usize;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Request {
    Hello { protocol: u16, build: u128 },
    Analyze(Check),
    Verify(Check),
    Status,
    Stop,
}

/// One check of a worktree: the client's resolved configuration and analysis switches, and the
/// paths whose issues it reports, or none for the whole worktree.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Check {
    pub configuration: Box<Configuration>,
    pub config_file: Option<PathBuf>,
    pub stubs: bool,
    pub paths: Vec<PathBuf>,
}

impl Check {
    /// A check of `configuration`'s worktree. The configuration file travels as an absolute path,
    /// because the daemon resolves extension host commands from its folder.
    pub(crate) fn new(configuration: Configuration, stubs: bool, paths: Vec<PathBuf>) -> Self {
        let config_file = configuration.config_file.as_deref().map(|file| CURRENT_DIR.join(file));

        Self { configuration: Box::new(configuration), config_file, stubs, paths }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Response {
    Ready,
    Stale { reason: String },
    Analyzed(Analyzed),
    Verified(Verification),
    Status(Status),
    Stopped,
    Failed { message: String },
}

/// The issues of one check, and the contents of every file an issue annotates.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Analyzed {
    pub issues: IssueCollection,
    pub files: Vec<File>,
}

/// A fresh whole-worktree analysis compared with the warm state: the issues only one of them
/// reports, each rendered as JSON.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Verification {
    pub issues: usize,
    pub only_warm: Vec<String>,
    pub only_fresh: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Status {
    pub pid: u32,
    pub build: String,
    pub worktrees: Vec<WorktreeStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct WorktreeStatus {
    pub workspace: PathBuf,
    pub warm: bool,
}

/// Writes `body` as one extension frame with a JSON payload.
pub(crate) fn write(stream: &mut impl Write, body: &impl Serialize) -> Result<(), Error> {
    let payload = serde_json::to_vec(body)?;
    Frame::request(0, payload)
        .write_to(stream, MAXIMUM_BODY_SIZE)
        .map_err(|error| Error::Server(format!("cannot write to the analysis server connection: {error}")))?;

    stream.flush().map_err(|error| Error::Server(format!("cannot write to the analysis server connection: {error}")))
}

/// Reads one frame's JSON body, or `None` when the peer closed the connection.
pub(crate) fn read<T: DeserializeOwned>(stream: &mut impl Read) -> Result<Option<T>, Error> {
    let Some(frame) = Frame::read_from(stream, MAXIMUM_BODY_SIZE)
        .map_err(|error| Error::Server(format!("cannot read from the analysis server connection: {error}")))?
    else {
        return Ok(None);
    };

    Ok(Some(serde_json::from_slice(&frame.payload)?))
}
