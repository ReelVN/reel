//! Recoverable content uses stable business paths, independently of rendered text.
use super::*;
use aside_vm::resume::{ResumeDescriptorBound, ResumeLimits, ResumeSiteRef, VerifiedResumeTable};

const DOMAIN: &[u8] = b"reel.dialogue.call.v1\0";
const LOGICAL_DOMAIN: &[u8] = b"reel.dialogue.flow.v1\0";
const PREFIX: &str = "call:v1:";
const LOGICAL_PREFIX: &str = "flow:v1:";
const SITE_BYTES: u64 = 16;
const HASH_BLOCK_BYTES: u64 = 64;
const FIXED_WORK: u64 = 2;

pub(super) enum Preparation {
    Ready { id: String, work: u32 },
    BudgetYield { required_work: u32 },
}

struct OrdinaryPath<'a> {
    sites: aside_vm::executor::HostCallSites<'a>,
    namespace: &'a str,
}

impl OrdinaryPath<'_> {
    fn hash_bytes(&self, sites: u64) -> u64 {
        DOMAIN.len() as u64 + 8 + self.namespace.len() as u64 + sites * SITE_BYTES
    }

    fn work(&self, sites: u64) -> u32 {
        // One unit per inspected frame and per hash block, plus the fixed
        // hasher/digest output. Native bridges count as inspected frames too.
        u32::try_from(
            FIXED_WORK
                + self.sites.frame_bound() as u64
                + self.hash_bytes(sites).div_ceil(HASH_BLOCK_BYTES),
        )
        .unwrap_or(u32::MAX)
    }

    fn required_work(&self) -> u32 {
        self.work(self.sites.frame_bound() as u64)
    }

    fn finish(mut self) -> (String, u32) {
        let mut hasher = blake3::Hasher::new();
        hasher.update(DOMAIN);
        hasher.update(&(self.namespace.len() as u64).to_le_bytes());
        hasher.update(self.namespace.as_bytes());
        let mut count = 0;
        for site in &mut self.sites {
            hasher.update(&site.code.version.to_le_bytes());
            hasher.update(&site.code.function.0.to_le_bytes());
            hasher.update(&site.pc.0.to_le_bytes());
            count += 1;
        }
        let work = self.work(count);
        let mut id = String::with_capacity(PREFIX.len() + 64);
        id.push_str(PREFIX);
        id.push_str(hasher.finalize().to_hex().as_str());
        (id, work)
    }
}

struct LogicalPath<'a> {
    vm: &'a Vm,
    table: &'a VerifiedResumeTable,
    ticket: &'a HostTicket,
    boundary: aside_vm::executor::FrameId,
    root: &'a str,
    bound: ResumeDescriptorBound,
}
impl LogicalPath<'_> {
    fn work(&self, frames: usize, key_bytes: u64) -> u32 {
        let hash_bytes = LOGICAL_DOMAIN.len() as u64
            + 8
            + self.root.len() as u64
            + frames as u64 * 17
            + key_bytes;
        u32::try_from(FIXED_WORK + self.bound.work + hash_bytes.div_ceil(HASH_BLOCK_BYTES))
            .unwrap_or(u32::MAX)
    }
    fn required_work(&self) -> u32 {
        self.work(self.bound.frames, self.bound.key_bytes)
    }
    fn finish(self) -> Result<(String, u32), AdapterError> {
        let descriptor = self.vm.resume_descriptor(
            self.table,
            *self.ticket,
            self.boundary,
            ResumeLimits {
                work: self.bound.work,
                ..Default::default()
            },
        )?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(LOGICAL_DOMAIN);
        hash_key(&mut hasher, self.root);
        for frame in descriptor.frames() {
            hash_key(&mut hasher, &frame.flow.0);
            match frame.site {
                ResumeSiteRef::Point(key) => {
                    hasher.update(&[0]);
                    hash_key(&mut hasher, &key.0);
                }
                ResumeSiteRef::Return(key) => {
                    hasher.update(&[1]);
                    hash_key(&mut hasher, &key.0);
                }
            }
        }
        let mut id = String::with_capacity(LOGICAL_PREFIX.len() + 64);
        id.push_str(LOGICAL_PREFIX);
        id.push_str(hasher.finalize().to_hex().as_str());
        Ok((
            id,
            self.work(descriptor.frame_count(), descriptor.key_bytes()),
        ))
    }
}
fn hash_key(hasher: &mut blake3::Hasher, key: &str) {
    hasher.update(&(key.len() as u64).to_le_bytes());
    hasher.update(key.as_bytes());
}
enum Path<'a> {
    Ordinary(OrdinaryPath<'a>),
    Logical(LogicalPath<'a>, OrdinaryPath<'a>),
}
impl Path<'_> {
    fn required_work(&self) -> u32 {
        match self {
            Self::Ordinary(path) => path.required_work(),
            Self::Logical(path, ordinary) => path
                .required_work()
                .saturating_add(ordinary.required_work()),
        }
    }
    fn finish(self) -> (String, u32) {
        match self {
            Self::Ordinary(path) => path.finish(),
            Self::Logical(path, ordinary) => {
                let proof_work = u32::try_from(path.bound.work).unwrap_or(u32::MAX);
                path.finish().unwrap_or_else(|_| {
                    let (id, work) = ordinary.finish();
                    (id, work.saturating_add(proof_work))
                })
            }
        }
    }
}

impl SessionVm {
    fn content_path<'a>(&'a self, ticket: &'a HostTicket) -> Result<Path<'a>, AdapterError> {
        let p = self.vm.program().package();
        let function = &p.functions[ticket.location.code.function.index()];
        let ordinary = OrdinaryPath {
            sites: self.vm.host_call_sites(*ticket)?,
            namespace: &p.symbols[p.modules[function.module.index()].path.index()],
        };
        if let Some((task, boundary)) = self.flow_boundary()
            && task == ticket.task
            && self
                .resume
                .has_point_at(ticket.location.code.function, ticket.location.pc)
        {
            let logical = LogicalPath {
                vm: &self.vm,
                table: &self.resume,
                ticket,
                boundary,
                root: &self
                    .flow_root_key()
                    .ok_or(AdapterError::Input(
                        "active business flow has no Root identity",
                    ))?
                    .0,
                bound: self.vm.resume_descriptor_bound(&self.resume, *ticket)?,
            };
            return Ok(Path::Logical(logical, ordinary));
        }
        Ok(Path::Ordinary(ordinary))
    }

    pub(super) fn content_id_required_work(&self, ticket: HostTicket) -> Result<u32, AdapterError> {
        Ok(self.content_path(&ticket)?.required_work())
    }

    pub(super) fn prepare_content_id(
        &self,
        ticket: HostTicket,
        budget: NativeBudget,
    ) -> Result<Preparation, AdapterError> {
        let path = self.content_path(&ticket)?;
        let required_work = path.required_work();
        if budget.with_minimum(required_work) < required_work {
            return Ok(Preparation::BudgetYield { required_work });
        }
        let (id, work) = path.finish();
        Ok(Preparation::Ready { id, work })
    }
}
