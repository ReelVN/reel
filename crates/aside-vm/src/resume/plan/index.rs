//! Current-function/call lookup is built once after the shared metadata proof.
use super::*;
use std::mem::size_of;

#[derive(Clone, Copy, Debug)]
struct Call {
    pc: Pc,
    site: PlannedSite,
    callee: Option<FunctionId>,
}
#[derive(Debug)]
pub(super) struct ResumeIndex {
    functions: Vec<(FunctionId, usize)>,
    flows: Vec<usize>,
    calls: Vec<Vec<Call>>,
    sites: Vec<Vec<PlannedSite>>,
    pub lookup_work: u64,
    pub key_bytes: u64,
}
fn depth(len: usize) -> u64 {
    u64::from(usize::BITS - len.leading_zeros()) + 1
}
fn reserve<T>(v: &mut Vec<T>, count: usize, b: &mut Budget) -> Result<(), ResumeError> {
    let bytes = count
        .checked_mul(size_of::<T>())
        .ok_or_else(|| ResumeError::new("resume index size"))?;
    b.bytes(bytes)?;
    v.try_reserve_exact(count)
        .map_err(|_| ResumeError::new("resume index allocation"))?;
    if v.capacity() * size_of::<T>() > bytes {
        b.bytes(v.capacity() * size_of::<T>() - bytes)?;
    }
    Ok(())
}
impl ResumeIndex {
    pub fn new(
        table: &ResumeTable,
        point_targets: &[FunctionId],
        b: &mut Budget,
    ) -> Result<Self, ResumeError> {
        b.bytes(size_of::<Self>() + 2 * size_of::<usize>())?;
        let mut functions = Vec::new();
        let mut flows = Vec::new();
        let mut calls = Vec::new();
        let mut sites = Vec::new();
        reserve(&mut functions, table.flows.len(), b)?;
        reserve(&mut flows, table.flows.len(), b)?;
        reserve(&mut calls, table.flows.len(), b)?;
        reserve(&mut sites, table.flows.len(), b)?;
        let mut longest_flow = 0;
        for (index, flow) in table.flows.iter().enumerate() {
            b.work(1)?;
            longest_flow = longest_flow.max(flow.key.0.len());
            flows.push(index);
            functions.push((flow.function, index));
        }
        b.work(functions.len() as u64 * depth(functions.len()))?;
        functions.sort_unstable_by_key(|v| v.0);
        b.work(
            (longest_flow as u64 + 1)
                .saturating_mul(flows.len() as u64)
                .saturating_mul(depth(flows.len())),
        )?;
        flows.sort_unstable_by(|&a, &b| table.flows[a].key.cmp(&table.flows[b].key));
        let mut lookup_work = 0;
        let mut key_bytes = 0;
        let mut point_targets = point_targets.iter().copied();
        for flow in &table.flows {
            let count = flow
                .points
                .len()
                .checked_add(flow.returns.len())
                .ok_or_else(|| ResumeError::new("resume index site count"))?;
            let mut current = Vec::new();
            let mut stable = Vec::new();
            reserve(&mut current, count, b)?;
            reserve(&mut stable, count, b)?;
            let mut longest = 0;
            for (index, site) in flow.points.iter().enumerate() {
                b.work(1)?;
                longest = longest.max(site.key.0.len());
                current.push(Call {
                    pc: site.call,
                    site: PlannedSite::Point(index),
                    callee: Some(
                        point_targets
                            .next()
                            .expect("each point has a verified target"),
                    ),
                });
                stable.push(PlannedSite::Point(index));
            }
            for (index, site) in flow.returns.iter().enumerate() {
                b.work(1)?;
                longest = longest.max(site.key.0.len());
                current.push(Call {
                    pc: site.call,
                    site: PlannedSite::Return(index),
                    callee: Some(site.callee),
                });
                stable.push(PlannedSite::Return(index));
            }
            b.work(count as u64 * depth(count))?;
            current.sort_unstable_by_key(|v| v.pc);
            b.work(
                (longest as u64 + 1)
                    .saturating_mul(count as u64)
                    .saturating_mul(depth(count)),
            )?;
            stable.sort_unstable_by(|&a, &b| key(flow, a).cmp(key(flow, b)));
            if current.windows(2).any(|pair| pair[0].pc == pair[1].pc) {
                return Err(ResumeError::new(
                    "one current call has multiple resume identities",
                ));
            }
            lookup_work = lookup_work.max(depth(functions.len()) + depth(count));
            key_bytes = key_bytes.max((flow.key.0.len() as u64).saturating_add(longest as u64));
            calls.push(current);
            sites.push(stable);
        }
        Ok(Self {
            functions,
            flows,
            calls,
            sites,
            lookup_work,
            key_bytes,
        })
    }
    pub fn stable_site(
        &self,
        table: &ResumeTable,
        frame: &FrameDescriptor,
    ) -> Option<PlannedFrame> {
        let flow = self
            .flows
            .binary_search_by(|&index| table.flows[index].key.cmp(&frame.flow))
            .ok()?;
        let flow = self.flows[flow];
        let expected = match &frame.site {
            ResumeSite::Point(key) => key.0.as_str(),
            ResumeSite::Return(key) => key.0.as_str(),
        };
        let index = self.sites[flow]
            .binary_search_by(|&site| key(&table.flows[flow], site).cmp(expected))
            .ok()?;
        let site = self.sites[flow][index];
        match (&frame.site, site) {
            (ResumeSite::Point(_), PlannedSite::Point(_))
            | (ResumeSite::Return(_), PlannedSite::Return(_)) => Some(PlannedFrame { flow, site }),
            _ => None,
        }
    }
    pub fn current_site(
        &self,
        function: FunctionId,
        call: Pc,
    ) -> Option<(PlannedFrame, Option<FunctionId>)> {
        let flow = self
            .functions
            .binary_search_by_key(&function, |v| v.0)
            .ok()
            .map(|i| self.functions[i].1)?;
        let site = self.calls[flow]
            .binary_search_by_key(&call, |v| v.pc)
            .ok()
            .map(|i| self.calls[flow][i])?;
        Some((
            PlannedFrame {
                flow,
                site: site.site,
            },
            site.callee,
        ))
    }
}
fn key(flow: &ResumeFlow, site: PlannedSite) -> &str {
    match site {
        PlannedSite::Point(index) => &flow.points[index].key.0,
        PlannedSite::Return(index) => &flow.returns[index].key.0,
    }
}
