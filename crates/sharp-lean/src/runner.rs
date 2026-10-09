//! The `sharp-lean` runner, spoken to over Mago's extension frames.

use std::io;
use std::num::NonZeroUsize;
use std::path::Path;
use std::time::Duration;

use mago_extension::PayloadReader;
use mago_extension::PayloadWriter;
use mago_extension::WorkerCommand;
use mago_extension::WorkerPool;
use mago_extension::WorkerPoolOptions;

use crate::library;

const LAW_PROTOCOL_MAGIC: [u8; 4] = *b"MLAW";
const LAW_PROTOCOL_MAJOR: u16 = 1;
const LAW_PROTOCOL_MINOR: u16 = 0;
const CHECK_REQUEST: u16 = 1;
const CHECKED_RESPONSE: u16 = 2;

/// The longest one request may take: importing a proof module and trying each proof step on each of its laws. Spec
/// section 28.1's `Money` takes about 2 s, the import and one step, so ten minutes holds a file of many hard laws.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(10);

/// One law a request asks about.
pub(crate) struct Question<'question> {
    /// The Lean name of the law's statement.
    pub(crate) statement: &'question str,
    /// Whether to look for a proof when the proof module has none.
    pub(crate) propose: bool,
    /// The definitions a proof step unfolds besides the law.
    pub(crate) unfold: Vec<&'question str>,
}

/// What a proof trusts beyond Lean's kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Proved,
    Gap,
    NativeDecide,
    Axiom(String),
}

/// A theorem whose type is a law's statement.
#[derive(Debug, Clone)]
pub(crate) struct Proof {
    pub(crate) line: u32,
    pub(crate) verdict: Verdict,
}

/// The answer for one law: its proofs, and the proof step that proves it when it had none and one was asked for.
#[derive(Debug, Clone)]
pub(crate) struct Answer {
    pub(crate) proofs: Vec<Proof>,
    pub(crate) proposal: Option<String>,
}

/// The `sharp-lean` process.
pub(crate) struct Runner {
    pool: WorkerPool,
}

impl Runner {
    /// Starts the runner of `library` on the modules Lake built in `package`.
    pub(crate) fn start(library: &Path, package: &Path) -> io::Result<Runner> {
        let lean_path = std::env::join_paths([library.join(library::LIBRARY), package.join(library::LIBRARY)])
            .map_err(io::Error::other)?;
        let command = WorkerCommand::new(library::runner(library))
            .with_current_directory(package)
            .with_environment("LEAN_PATH", lean_path);
        let options = WorkerPoolOptions { request_timeout: REQUEST_TIMEOUT, ..WorkerPoolOptions::default() };

        Ok(Runner { pool: WorkerPool::spawn(command, NonZeroUsize::MIN, options).map_err(io::Error::other)? })
    }

    /// Imports `module` and answers each question, counting only the theorems of `proof_module`, or none when it is
    /// empty.
    pub(crate) fn check(
        &self,
        module: &str,
        proof_module: &str,
        questions: &[Question<'_>],
    ) -> io::Result<Vec<Answer>> {
        let response = self.pool.request(encode(module, proof_module, questions)?).map_err(io::Error::other)?;

        decode(&response, questions.len()).map_err(io::Error::other)
    }
}

fn encode(module: &str, proof_module: &str, questions: &[Question<'_>]) -> io::Result<Vec<u8>> {
    let mut writer = PayloadWriter::new();
    writer.write_raw(&LAW_PROTOCOL_MAGIC);
    writer.write_u16(LAW_PROTOCOL_MAJOR);
    writer.write_u16(LAW_PROTOCOL_MINOR);
    writer.write_u16(CHECK_REQUEST);
    writer.write_u16(0);
    writer.write_string(module).map_err(io::Error::other)?;
    writer.write_string(proof_module).map_err(io::Error::other)?;
    writer.write_length(questions.len()).map_err(io::Error::other)?;
    for question in questions {
        writer.write_string(question.statement).map_err(io::Error::other)?;
        writer.write_u8(u8::from(question.propose));
        writer.write_length(question.unfold.len()).map_err(io::Error::other)?;
        for name in &question.unfold {
            writer.write_string(name).map_err(io::Error::other)?;
        }
    }

    Ok(writer.finish())
}

fn decode(payload: &[u8], questions: usize) -> Result<Vec<Answer>, String> {
    let mut reader = PayloadReader::new(payload);
    let error = |error: mago_extension::PayloadError| error.to_string();
    if reader.read_array::<4>("law message magic").map_err(error)? != LAW_PROTOCOL_MAGIC {
        return Err("the runner's answer does not start with MLAW".to_owned());
    }
    let major = reader.read_u16("law protocol major version").map_err(error)?;
    let minor = reader.read_u16("law protocol minor version").map_err(error)?;
    if major != LAW_PROTOCOL_MAJOR {
        return Err(format!("the runner speaks law protocol {major}.{minor}"));
    }
    let kind = reader.read_u16("law message kind").map_err(error)?;
    if kind != CHECKED_RESPONSE {
        return Err(format!("the runner answered with the unknown law message kind {kind}"));
    }
    reader.read_u16("law message reserved header").map_err(error)?;

    let count = reader.read_count("answer count", reader.remaining()).map_err(error)?;
    if count != questions {
        return Err(format!("the runner answered {count} laws of {questions}"));
    }
    let mut answers = Vec::with_capacity(count);
    for _ in 0..count {
        let proof_count = reader.read_count("proof count", reader.remaining()).map_err(error)?;
        let mut proofs = Vec::with_capacity(proof_count);
        for _ in 0..proof_count {
            let line = reader.read_u32("proof line").map_err(error)?;
            let code = reader.read_u8("proof verdict").map_err(error)?;
            let axiom = reader.read_string("proof axiom").map_err(error)?;
            let verdict = match code {
                0 => Verdict::Proved,
                1 => Verdict::Gap,
                2 => Verdict::NativeDecide,
                3 => Verdict::Axiom(axiom),
                code => return Err(format!("the runner answered with the unknown verdict {code}")),
            };
            proofs.push(Proof { line, verdict });
        }
        let proposal = reader.read_string("proposal").map_err(error)?;
        answers.push(Answer { proofs, proposal: (!proposal.is_empty()).then_some(proposal) });
    }
    reader.finish().map_err(error)?;

    Ok(answers)
}
