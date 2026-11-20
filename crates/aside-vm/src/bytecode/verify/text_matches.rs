//! Index forward chains of short literal tests without changing bytecode.
use super::*;

const MIN_CASES: usize = 8;
const MAX_CASES: usize = 512;
const MAX_LITERAL_BYTES: usize = 64;
const MAX_FUNCTIONS: usize = 256;

/// Bounded candidates found during mandatory verification. Building the index
/// afterwards leaves the original package's admission budgets available first.
pub(super) struct MatchFunctions {
    entries: [(FunctionId, usize); MAX_FUNCTIONS],
    len: usize,
}

impl MatchFunctions {
    pub(super) fn new() -> Self {
        Self {
            entries: [(FunctionId(0), 0); MAX_FUNCTIONS],
            len: 0,
        }
    }

    pub(super) fn note(&mut self, function: FunctionId, literal_checks: usize) {
        if literal_checks >= MIN_CASES && self.len < self.entries.len() {
            self.entries[self.len] = (function, literal_checks);
            self.len += 1;
        }
    }

    pub(super) fn build(
        &self,
        package: &RuntimePackage,
        budget: &mut Budget,
    ) -> Result<Option<std::sync::Arc<TextMatches>>, Error> {
        if self.len == 0 {
            return Ok(None);
        }
        let mut optional = Budget {
            owned: budget
                .owned
                .saturating_add(package.text_pool.map_or(0, |pool| {
                    usize::try_from(pool.byte_len).unwrap_or(usize::MAX)
                }))
                .saturating_add(
                    package
                        .functions
                        .first()
                        .and_then(|f| f.code.store())
                        .map_or(0, |store| {
                            let code = store.cache_stats();
                            code.hot_limit_bytes + code.construction_scratch_bytes
                        }),
                )
                .saturating_add(TextMatches::HEADER_BYTES),
            ..*budget
        };
        let mut matches = TextMatches::default();
        let result = self.entries[..self.len]
            .iter()
            .try_for_each(|&(function, literal_checks)| {
                matches.add_function(
                    function,
                    &package.functions[function.index()].code,
                    &package.types,
                    literal_checks,
                    &mut optional,
                )
            });
        budget.peak = optional.peak;
        budget.work = optional.work.min(budget.limits.max_work);
        match result {
            Ok(()) => Ok((!matches.sites.is_empty()).then(|| std::sync::Arc::new(matches))),
            Err(error) if matches!(error.kind, ErrorKind::Limit | ErrorKind::Allocation) => {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct TextMatches {
    sites: Vec<Site>,
    cases: Vec<Case>,
}

#[derive(Debug)]
struct Site {
    function: FunctionId,
    entry: Pc,
    miss: Pc,
    cases: std::ops::Range<usize>,
}

#[derive(Clone, Copy, Debug)]
struct Case {
    literal: TypeId,
    branch: Pc,
}

struct Candidate {
    pc: Pc,
    branch: Pc,
    next: Pc,
    registers: [u32; 2],
    literal: TypeId,
    used: bool,
}

pub(crate) struct TextMatch<'a> {
    cases: &'a [Case],
    types: &'a [TypeDef],
    miss: Pc,
}

impl VerifiedProgram {
    pub(crate) fn text_match(&self, function: FunctionId, pc: Pc) -> Option<TextMatch<'_>> {
        let matches = self.2.as_ref()?;
        let index = matches
            .sites
            .binary_search_by_key(&(function, pc), |site| (site.function, site.entry))
            .ok()?;
        let site = &matches.sites[index];
        Some(TextMatch {
            cases: &matches.cases[site.cases.clone()],
            types: &self.0.package.types,
            miss: site.miss,
        })
    }

    pub(crate) fn text_match_capacity_bytes(&self) -> usize {
        self.2.as_ref().map_or(0, |index| index.capacity_bytes())
    }

    pub(crate) fn disable_text_matches(&mut self) {
        self.2 = None;
    }
}

impl TextMatch<'_> {
    pub(crate) fn accepts_length(&self, len: usize) -> bool {
        len <= MAX_LITERAL_BYTES
    }

    pub(crate) fn work(&self, len: usize) -> u64 {
        // Length comparison first: only an equal-length case reads subject bytes.
        3 + (2 + len as u64) * u64::from(self.cases.len().ilog2() + 2)
    }

    pub(crate) fn select(&self, subject: Option<&[u8]>) -> (Pc, bool) {
        if let Some(bytes) = subject
            && let Ok(index) = self.cases.binary_search_by(|case| {
                let text = literal(self.types, case.literal);
                (text.len(), text).cmp(&(bytes.len(), bytes))
            })
        {
            return (self.cases[index].branch, true);
        }
        (self.miss, false)
    }
}

pub(super) fn is_case(types: &[TypeDef], instruction: Instruction) -> bool {
    let [dest, source, ty] = instruction.operands;
    instruction.opcode == Opcode::Is as u16
        && source != dest
        && matches!(&types[ty as usize], TypeDef::StringLiteral(text) if text.len() <= MAX_LITERAL_BYTES)
}

impl TextMatches {
    const HEADER_BYTES: usize = size_of::<Self>() + 2 * size_of::<usize>();

    fn capacity_bytes(&self) -> usize {
        Self::HEADER_BYTES
            + self.sites.capacity() * size_of::<Site>()
            + self.cases.capacity() * size_of::<Case>()
    }

    fn add_function(
        &mut self,
        function: FunctionId,
        code: &impl InstructionSequence,
        types: &[TypeDef],
        literal_checks: usize,
        budget: &mut Budget,
    ) -> Result<(), Error> {
        let mut candidates = budget.vec::<Candidate>(literal_checks)?;
        let mut previous: Option<(Pc, [u32; 3])> = None;
        budget.work(code.len() as u64)?;
        for item in code.indexed_iter() {
            let (pc, instruction) = item?;
            let [reg, _, next] = instruction.operands;
            if let Some((test_pc, [dest, source, ty])) = previous.take()
                && instruction.opcode == Opcode::BranchBool as u16
                && reg == dest
            {
                candidates.push(Candidate {
                    pc: test_pc,
                    branch: Pc(pc as u32),
                    next: Pc(next),
                    registers: [dest, source],
                    literal: TypeId(ty),
                    used: false,
                });
            }
            if is_case(types, instruction) {
                previous = Some((Pc(pc as u32), instruction.operands));
            }
        }
        let mut chain = budget.vec::<usize>(candidates.len().min(MAX_CASES))?;
        let lookup_work = 2 + u64::from(candidates.len().checked_ilog2().unwrap_or(0));
        for start in 0..candidates.len() {
            budget.work(1)?;
            if candidates[start].used {
                continue;
            }
            chain.clear();
            let mut cursor = start;
            loop {
                let candidate = &mut candidates[cursor];
                candidate.used = true;
                chain.push(cursor);
                let next = candidate.next;
                if next <= candidate.pc || chain.len() == MAX_CASES {
                    break;
                }
                budget.work(lookup_work)?;
                let Ok(index) = candidates.binary_search_by_key(&next, |item| item.pc) else {
                    break;
                };
                if candidates[index].used
                    || candidates[index].registers != candidates[start].registers
                {
                    break;
                }
                cursor = index;
            }
            if chain.len() < MIN_CASES {
                continue;
            }
            reserve(&mut self.sites, 1, budget)?;
            reserve(&mut self.cases, chain.len(), budget)?;
            let miss = candidates[*chain.last().unwrap()].branch;
            budget.work(
                chain.len() as u64
                    * u64::from(chain.len().ilog2() + 1)
                    * 4
                    * (MAX_LITERAL_BYTES as u64 + 1),
            )?;
            chain.sort_unstable_by_key(|&index| {
                let case = &candidates[index];
                let bytes = literal(types, case.literal);
                (bytes.len(), bytes, case.branch)
            });
            // Keep the earliest branch in the original chain for repeated literals.
            chain.dedup_by_key(|index| literal(types, candidates[*index].literal));
            let first = self.cases.len();
            for &index in &chain {
                let case = &candidates[index];
                self.cases.push(Case {
                    literal: case.literal,
                    branch: case.branch,
                });
            }
            self.sites.push(Site {
                function,
                entry: candidates[start].pc,
                miss,
                cases: first..self.cases.len(),
            });
        }
        budget.release(chain);
        budget.release(candidates);
        Ok(())
    }
}

fn literal(types: &[TypeDef], ty: TypeId) -> &[u8] {
    let TypeDef::StringLiteral(text) = &types[ty.index()] else {
        unreachable!("verified text match")
    };
    text.as_bytes()
}

fn reserve<T>(values: &mut Vec<T>, count: usize, budget: &mut Budget) -> Result<(), Error> {
    let needed = values.len() + count;
    if needed > values.capacity() {
        let mut grown = budget.vec(needed.max(values.capacity() * 2))?;
        grown.append(values);
        budget.release(std::mem::replace(values, grown));
    }
    Ok(())
}
