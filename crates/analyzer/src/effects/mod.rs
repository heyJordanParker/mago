//! The effects of PHP# code, spec section 29.
//!
//! Mago analyzes files in parallel against signatures only, so no body knows its callees' effects while it is
//! analyzed. Each body records what it does by itself in an [`EffectSummary`] during its file's analysis, and
//! [`Effects::solve`] combines every summary once all files are analyzed.

use std::collections::BTreeSet;
use std::fmt;

use foldhash::HashMap;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_reporting::IssueCollection;
use mago_span::Span;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::empty_word;
use mago_word::word;

pub(crate) mod check;
pub(crate) mod summary;

/// A reason a body is impure other than a change of state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Effect {
    /// An effect an `extern` declaration lists for the plain PHP called, named by its class, such as `Sharp\Http`.
    Foreign(Word),
    /// A call into plain PHP that no `extern` declaration covers, named as PHP# writes the callee.
    Unknown(Word),
}

/// The state a body changes, named by the root of the place it writes. The same roots name the values a call passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Changed {
    /// The object the body runs on.
    This,
    /// What the caller passed as the parameter at this index.
    Parameter(u32),
    /// State every body reaches, such as a static property.
    Shared,
}

/// The roots of a value. A value with no root is fresh: the body made it, so changing it changes nothing given.
pub(crate) type Roots = BTreeSet<Changed>;

/// A PHP# body: a method by its lowercase class and method names, or an accessor by its lowercase class, its
/// property's PHP name (`$total`) and `get` or `set`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Body {
    Method(Word, Word),
    Accessor(Word, Word, Word),
}

/// A call from one PHP# body to another, with the roots of what it passes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Call {
    pub(crate) callee: Body,
    pub(crate) span: Span,
    pub(crate) receiver: Roots,
    /// The roots of each argument, by the callee's parameter index.
    pub(crate) arguments: Vec<Roots>,
}

/// What one PHP# body does by itself: the effects of the plain PHP it calls, the PHP# bodies it calls, and the state
/// it changes. A lambda is part of the body that calls it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectSummary {
    pub(crate) body: Body,
    /// The body as messages name it: `Order.price`, or `Cart.total` for an accessor of `total`.
    pub(crate) name: Word,
    /// Each effect, with the call that has it and that call's callee as PHP# writes it.
    pub(crate) effects: Vec<(Effect, Span, Word)>,
    pub(crate) calls: Vec<Call>,
    /// Each change, with the write and the place written as the source writes it.
    pub(crate) changes: Vec<(Changed, Span, Word)>,
}

/// Why a body is impure, as the first call or write in it that leads there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Impurity {
    /// The call or write in the checked body.
    pub span: Span,
    /// The PHP# bodies the call reaches, from the one the checked body calls down to the one that is impure by
    /// itself. Empty when the checked body is impure by itself.
    pub path: Vec<Word>,
    /// The plain PHP callee as PHP# writes it, such as `Clock.now`, or the place written, such as `this.count`.
    pub cause: Word,
    /// The effect of the call, or `None` when the cause is a place written.
    pub effect: Option<Effect>,
}

impl fmt::Display for Impurity {
    /// Writes the reason as a sentence's predicate, such as "calls `getenv`, which has no `extern` declaration".
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cause = self.cause;
        match (self.path.last(), self.effect) {
            (None, Some(Effect::Foreign(effect))) => {
                write!(f, "calls `{cause}`, which has the effect `{}`", short_name(effect))
            }
            (None, Some(Effect::Unknown(_))) => write!(f, "calls `{cause}`, which has no `extern` declaration"),
            (None, None) => write!(f, "changes `{cause}`"),
            (Some(body), Some(Effect::Foreign(effect))) => {
                write!(f, "reaches `{body}`, which calls `{cause}` with the effect `{}`", short_name(effect))
            }
            (Some(body), Some(Effect::Unknown(_))) => {
                write!(f, "reaches `{body}`, which calls `{cause}` with no `extern` declaration")
            }
            (Some(body), None) => write!(f, "reaches `{body}`, which changes `{cause}`"),
        }
    }
}

/// What a body does: an effect, or a change of a root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Item {
    Effect(Effect),
    Changed(Changed),
}

/// Where an item of a body comes from: its own call or write, or a call whose callee has `via`'s item.
#[derive(Clone, Copy, Debug)]
struct Origin {
    span: Span,
    cause: Word,
    via: Option<(usize, Item)>,
}

/// The solved effects and changes of every PHP# body.
#[derive(Debug)]
pub struct Effects {
    bodies: Vec<Body>,
    names: Vec<Word>,
    index: HashMap<Body, usize>,
    /// Each body's items, its own in source order first, and each once.
    solved: Vec<Vec<(Item, Origin)>>,
}

impl Effects {
    /// Combines `summaries` into what each body does through every call: one solve over the strongly connected
    /// parts of the call graph, callees first, each part iterated until nothing grows.
    #[must_use]
    pub fn solve(codebase: &CodebaseMetadata, summaries: &[EffectSummary]) -> Effects {
        let mut index = HashMap::default();
        for (position, summary) in summaries.iter().enumerate() {
            index.entry(summary.body).or_insert(position);
        }

        // A call to a method that can be overridden runs any override, so it reaches every override's body.
        let mut overrides: HashMap<(Word, Word), Vec<usize>> = HashMap::default();
        for (position, summary) in summaries.iter().enumerate() {
            if let Body::Method(class, method) = summary.body
                && index.get(&summary.body) == Some(&position)
                && let Some(metadata) = codebase.get_class_like(class.as_bytes())
            {
                for parent in &metadata.all_parent_classes {
                    overrides.entry((ascii_lowercase_word(parent.as_bytes()), method)).or_default().push(position);
                }
            }
        }

        let targets: Vec<Vec<Vec<usize>>> = summaries
            .iter()
            .map(|summary| {
                summary
                    .calls
                    .iter()
                    .map(|call| {
                        // A bodiless callee contributes nothing until PR 2 declares what it uses.
                        let Some(&callee) = index.get(&call.callee) else {
                            return Vec::new();
                        };
                        let mut targets = vec![callee];
                        if let Body::Method(class, method) = call.callee {
                            targets.extend(overrides.get(&(class, method)).into_iter().flatten());
                        }

                        targets
                    })
                    .collect()
            })
            .collect();

        let successors: Vec<Vec<usize>> =
            targets.iter().map(|calls| calls.iter().flatten().copied().collect()).collect();

        let mut solved: Vec<Vec<(Item, Origin)>> = summaries.iter().map(own_items).collect();
        for part in strongly_connected_parts(&successors) {
            loop {
                let mut grew = false;
                for &body in &part {
                    for (call, call_targets) in summaries[body].calls.iter().zip(&targets[body]) {
                        for &target in call_targets {
                            let reached: Vec<(Item, Item)> = solved[target]
                                .iter()
                                .flat_map(|(item, _)| through(*item, call).map(move |mapped| (*item, mapped)))
                                .collect();

                            for (callee_item, item) in reached {
                                if !solved[body].iter().any(|(known, _)| *known == item) {
                                    let origin = Origin {
                                        span: call.span,
                                        cause: empty_word(),
                                        via: Some((target, callee_item)),
                                    };
                                    solved[body].push((item, origin));
                                    grew = true;
                                }
                            }
                        }
                    }
                }

                if !grew {
                    break;
                }
            }
        }

        Effects {
            bodies: summaries.iter().map(|summary| summary.body).collect(),
            names: summaries.iter().map(|summary| summary.name).collect(),
            index,
            solved,
        }
    }

    /// Why `method` is impure, or `None` when it has no effect and changes neither `this`, a parameter nor shared
    /// state. A constructor's changes to `this` do not count, because its object is new.
    ///
    /// It answers only for a PHP# method with a body. It returns `None` for a function, a closure, a plain PHP method
    /// and a native `extern` method, whose effects no summary records.
    #[must_use]
    pub fn impurity(&self, method: &FunctionLikeIdentifier) -> Option<Impurity> {
        let FunctionLikeIdentifier::Method(class, method) = method else {
            return None;
        };

        self.body_impurity(Body::Method(
            ascii_lowercase_word(class.as_bytes()),
            ascii_lowercase_word(method.as_bytes()),
        ))
    }

    /// The issues of every effect rule. The getter rule reads only the solve. PR 2's rules read the declared `uses`
    /// from `codebase`.
    #[must_use]
    pub fn issues(&self, _codebase: &CodebaseMetadata) -> IssueCollection {
        check::getters_must_be_pure(self)
    }

    fn body_impurity(&self, body: Body) -> Option<Impurity> {
        let position = *self.index.get(&body)?;
        let constructor = matches!(body, Body::Method(_, method) if method.as_bytes() == b"__construct");
        let &(mut item, mut origin) = self.solved[position]
            .iter()
            .filter(|(item, _)| !(constructor && *item == Item::Changed(Changed::This)))
            .min_by_key(|(_, origin)| origin.span.start.offset)?;

        let span = origin.span;
        let mut path = Vec::new();
        // Each item a call brings in was already its callee's, so the chain ends at a body's own call or write.
        while let Some((callee, callee_item)) = origin.via
            && let Some((_, callee_origin)) = self.solved[callee].iter().find(|(known, _)| *known == callee_item)
        {
            path.push(self.names[callee]);
            item = callee_item;
            origin = *callee_origin;
        }

        let effect = match item {
            Item::Effect(effect) => Some(effect),
            Item::Changed(_) => None,
        };

        Some(Impurity { span, path, cause: origin.cause, effect })
    }

    /// Each body with its message name and why it is impure.
    fn impure_bodies(&self) -> impl Iterator<Item = (Body, Word, Impurity)> + '_ {
        self.bodies.iter().enumerate().filter(|(position, body)| self.index.get(body) == Some(position)).filter_map(
            |(position, body)| self.body_impurity(*body).map(|impurity| (*body, self.names[position], impurity)),
        )
    }
}

/// A body's own effects and changes, in source order, an effect before a change at the same place.
fn own_items(summary: &EffectSummary) -> Vec<(Item, Origin)> {
    let mut items: Vec<(Item, Origin)> = Vec::new();
    let effects = summary.effects.iter().map(|(effect, span, cause)| (Item::Effect(*effect), *span, *cause));
    let changes = summary.changes.iter().map(|(changed, span, place)| (Item::Changed(*changed), *span, *place));
    for (item, span, cause) in effects.chain(changes) {
        if !items.iter().any(|(known, _)| *known == item) {
            items.push((item, Origin { span, cause, via: None }));
        }
    }

    items.sort_by_key(|(_, origin)| origin.span.start.offset);
    items
}

/// What a callee's item is in the caller: an effect stays, and a change maps through the roots the call passes.
fn through(item: Item, call: &Call) -> impl Iterator<Item = Item> + '_ {
    let roots: Vec<Changed> = match item {
        Item::Effect(_) => Vec::new(),
        Item::Changed(Changed::This) => call.receiver.iter().copied().collect(),
        Item::Changed(Changed::Parameter(index)) => {
            call.arguments.get(index as usize).map(|roots| roots.iter().copied().collect()).unwrap_or_default()
        }
        Item::Changed(Changed::Shared) => vec![Changed::Shared],
    };

    let effect = match item {
        Item::Effect(_) => Some(item),
        Item::Changed(_) => None,
    };

    effect.into_iter().chain(roots.into_iter().map(Item::Changed))
}

/// The strongly connected parts of a graph, each after every part it reaches (Tarjan, without recursion so a long
/// chain of calls cannot overflow the stack).
fn strongly_connected_parts(successors: &[Vec<usize>]) -> Vec<Vec<usize>> {
    const UNVISITED: usize = usize::MAX;

    let mut order = vec![UNVISITED; successors.len()];
    let mut low = vec![0; successors.len()];
    let mut on_stack = vec![false; successors.len()];
    let mut stack = Vec::new();
    let mut parts = Vec::new();
    let mut next = 0;

    for root in 0..successors.len() {
        if order[root] != UNVISITED {
            continue;
        }

        let mut work = vec![(root, 0)];
        while let Some((node, mut position)) = work.pop() {
            if position == 0 {
                order[node] = next;
                low[node] = next;
                next += 1;
                stack.push(node);
                on_stack[node] = true;
            }

            let mut descended = false;
            while let Some(&successor) = successors[node].get(position) {
                position += 1;
                if order[successor] == UNVISITED {
                    work.push((node, position));
                    work.push((successor, 0));
                    descended = true;
                    break;
                }

                if on_stack[successor] {
                    low[node] = low[node].min(order[successor]);
                }
            }

            if descended {
                continue;
            }

            if low[node] == order[node] {
                let mut part = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack[member] = false;
                    part.push(member);
                    if member == node {
                        break;
                    }
                }

                parts.push(part);
            }

            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[node]);
            }
        }
    }

    parts
}

/// The last segment of a class name: `Clock` for `Sharp\Clock`.
pub(crate) fn short_name(name: Word) -> Word {
    let bytes = name.as_bytes();

    word(bytes.rsplit(|byte| *byte == b'\\').next().unwrap_or(bytes))
}
