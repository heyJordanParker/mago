use crate::binding::Local;

/// The locals of one PHP# method, block by block.
#[derive(Debug)]
struct Frame<'arena> {
    /// The open blocks, innermost last. The first block holds the parameters.
    blocks: Vec<Vec<(&'arena [u8], Local)>>,
    /// The locals of blocks that have already closed.
    closed: Vec<(&'arena [u8], Local)>,
}

/// The locals the binder can see, one frame per method being walked.
///
/// A method never sees the locals of the method or file around it.
#[derive(Debug)]
pub struct LocalScopes<'arena> {
    frames: Vec<Frame<'arena>>,
}

impl<'arena> LocalScopes<'arena> {
    pub fn enter_method(&mut self) {
        self.frames.push(Frame { blocks: vec![Vec::new()], closed: Vec::new() });
    }

    pub fn exit_method(&mut self) {
        self.frames.pop();
    }

    pub fn enter_block(&mut self) {
        self.frame().blocks.push(Vec::new());
    }

    pub fn exit_block(&mut self) {
        let frame = self.frame();
        if let Some(block) = frame.blocks.pop() {
            frame.closed.extend(block);
        }
    }

    /// Declares a local in the innermost open block.
    ///
    /// Returns the local an open block already declares under the same name, if any.
    pub fn declare(&mut self, name: &'arena [u8], local: Local) -> Option<Local> {
        let earlier = self.lookup(name);
        if let Some(block) = self.frame().blocks.last_mut() {
            block.push((name, local));
        }

        earlier
    }

    /// Returns the local an open block declares under `name`, innermost first.
    pub fn lookup(&self, name: &[u8]) -> Option<Local> {
        self.frames
            .last()?
            .blocks
            .iter()
            .rev()
            .flat_map(|block| block.iter().rev())
            .find_map(|(declared, local)| if *declared == name { Some(*local) } else { None })
    }

    /// Returns the latest local of this method declared under `name` whose block has closed.
    pub fn lookup_closed(&self, name: &[u8]) -> Option<Local> {
        self.frames
            .last()?
            .closed
            .iter()
            .rev()
            .find_map(|(declared, local)| if *declared == name { Some(*local) } else { None })
    }

    fn frame(&mut self) -> &mut Frame<'arena> {
        if self.frames.is_empty() {
            self.enter_method();
        }

        let last = self.frames.len() - 1;
        &mut self.frames[last]
    }
}

impl Default for LocalScopes<'_> {
    fn default() -> Self {
        let mut scopes = Self { frames: Vec::new() };
        scopes.enter_method();
        scopes
    }
}
