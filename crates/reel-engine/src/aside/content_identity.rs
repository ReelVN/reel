//! Content checkpoints use code positions, independently of rendered text.
use super::*;

const DOMAIN: &[u8] = b"reel.dialogue.call.v1\0";
const PREFIX: &str = "call:v1:";
const SITE_BYTES: u64 = 16;
const HASH_BLOCK_BYTES: u64 = 64;
const FIXED_WORK: u64 = 2;

pub(super) enum Preparation {
    Ready { id: String, work: u32 },
    BudgetYield { required_work: u32 },
}

struct Path<'a> {
    sites: aside_vm::executor::HostCallSites<'a>,
    namespace: &'a str,
}

impl Path<'_> {
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

impl SessionVm {
    fn content_path(&self, ticket: HostTicket) -> Result<Path<'_>, AdapterError> {
        let p = self.vm.program().package();
        let function = &p.functions[ticket.location.code.function.index()];
        Ok(Path {
            sites: self.vm.host_call_sites(ticket)?,
            namespace: &p.symbols[p.modules[function.module.index()].path.index()],
        })
    }

    pub(super) fn content_id_required_work(&self, ticket: HostTicket) -> Result<u32, AdapterError> {
        Ok(self.content_path(ticket)?.required_work())
    }

    pub(super) fn prepare_content_id(
        &self,
        ticket: HostTicket,
        budget: NativeBudget,
    ) -> Result<Preparation, AdapterError> {
        let path = self.content_path(ticket)?;
        let required_work = path.required_work();
        if budget.with_minimum(required_work) < required_work {
            return Ok(Preparation::BudgetYield { required_work });
        }
        let (id, work) = path.finish();
        Ok(Preparation::Ready { id, work })
    }
}
