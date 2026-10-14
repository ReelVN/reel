//! Iterative tree execution. A leaf owns properties only while it is active.
use super::*;
use action::Kind;
use graph::{Job, Phase, Slot, Track, get, int, kind, node, phase, set, track, track_source};

impl Activities {
    pub fn has_active(&self) -> bool {
        self.head.is_some()
    }
    pub fn needs_time(&self) -> bool {
        self.running != 0
    }
    pub fn pending(&self) -> bool {
        self.round.is_some()
    }

    /// Script turns may retire targets or write properties while every producer
    /// is paused. Settle those changes even when no clock wake is scheduled.
    pub fn after_script(&mut self, heap: &Heap) -> Result<(), AdapterError> {
        if self.has_active() {
            self.begin_round(heap, self.time)?;
        }
        Ok(())
    }

    pub(super) fn first(&self, heap: &Heap) -> Result<Value, AdapterError> {
        Ok(self
            .head
            .map(|root| heap.root_value(root))
            .transpose()?
            .unwrap_or(Value::NULL))
    }

    pub(super) fn job_head(&self, heap: &Heap) -> Result<Value, AdapterError> {
        Ok(self
            .jobs
            .map(|root| heap.root_value(root))
            .transpose()?
            .unwrap_or(Value::NULL))
    }

    pub(super) fn queue(
        &mut self,
        heap: &mut Heap,
        leaf: Value,
        job: Job,
        time: f64,
        front: bool,
    ) -> Result<(), AdapterError> {
        set(heap, leaf, Slot::JobKind, Value::int(job as i64))?;
        set(heap, leaf, Slot::JobTime, Value::float(time))?;
        if front {
            set(heap, leaf, Slot::JobNext, self.job_head(heap)?)?;
            heap.set_root(self.jobs.unwrap(), leaf)?;
            if self.job_tail.is_none() {
                self.job_tail = Some(leaf);
            }
        } else {
            set(heap, leaf, Slot::JobNext, Value::NULL)?;
            if let Some(tail) = self.job_tail {
                set(heap, tail, Slot::JobNext, leaf)?;
            } else {
                heap.set_root(self.jobs.unwrap(), leaf)?;
            }
            self.job_tail = Some(leaf);
        }
        Ok(())
    }

    fn pop_job(&mut self, heap: &mut Heap) -> Result<(Value, Job, f64), AdapterError> {
        let leaf = self.job_head(heap)?;
        let next = get(heap, leaf, Slot::JobNext)?;
        let job = match int(heap, leaf, Slot::JobKind)? {
            0 => Job::Activate,
            1 => Job::Complete,
            _ => unreachable!("private job schema"),
        };
        let time = get(heap, leaf, Slot::JobTime)?.as_float().unwrap();
        heap.set_root(self.jobs.unwrap(), next)?;
        if !present(next) {
            self.job_tail = None;
        }
        set(heap, leaf, Slot::JobNext, Value::NULL)?;
        set(heap, leaf, Slot::JobKind, Value::NULL)?;
        Ok((leaf, job, time))
    }

    fn event_key(heap: &Heap, leaf: Value, deadline: f64) -> Result<EventKey, AdapterError> {
        let activity = get(heap, leaf, Slot::Activity)?;
        Ok((
            deadline.to_bits(),
            root_get(heap, activity, RootSlot::Serial)?
                .as_int()
                .unwrap() as u64,
            int(heap, leaf, Slot::Index)? as u32,
        ))
    }

    fn remove_event(&mut self, heap: &mut Heap, leaf: Value) -> Result<(), AdapterError> {
        if let Some(deadline) = get(heap, leaf, Slot::Event)?.as_float() {
            self.events.remove(&Self::event_key(heap, leaf, deadline)?);
            set(heap, leaf, Slot::Event, Value::NULL)?;
        }
        Ok(())
    }

    fn schedule(&mut self, heap: &mut Heap, leaf: Value) -> Result<(), AdapterError> {
        let start = get(heap, leaf, Slot::StartedAt)?.as_float().unwrap();
        let duration = action::duration(heap, get(heap, leaf, Slot::Action)?)?;
        // A positive duration must move the clock even below its current ULP.
        let deadline = (start + duration).max(start.next_up());
        self.events
            .insert(Self::event_key(heap, leaf, deadline)?, leaf);
        set(heap, leaf, Slot::Event, Value::float(deadline))
    }

    fn sample(heap: &mut Heap, leaf: Value, time: f64, end: bool) -> Result<(), AdapterError> {
        let action = get(heap, leaf, Slot::Action)?;
        let duration = action::duration(heap, action)?;
        let start_time = get(heap, leaf, Slot::StartedAt)?.as_float().unwrap();
        let t = if end || duration == 0. {
            1.
        } else {
            ((time - start_time) / duration).clamp(0., 1.)
        };
        let t = match payload(heap, action, EASING)?.as_int().unwrap() {
            1 => t * t * t,
            2 => 1. - (1. - t).powi(3),
            3 if t < 0.5 => 4. * t * t * t,
            3 => 1. - (-2. * t + 2.).powi(3) / 2.,
            _ => t,
        };
        let target = payload(heap, action, TARGET)?;
        let fields = heap.host_fields(target)?;
        let tracks = get(heap, leaf, Slot::Tracks)?;
        for i in (0..heap.sequence_len(tracks)?).step_by(Track::LEN) {
            let field = track(heap, tracks, i, Track::Field)?.as_int().unwrap() as u32;
            let start = track(heap, tracks, i, Track::Start)?.as_float().unwrap();
            let end = track(heap, tracks, i, Track::End)?.as_float().unwrap();
            let value = Value::float(if t == 1. {
                end
            } else {
                start * (1. - t) + end * t
            });
            let (_, old) = heap.table_entry(fields, field)?;
            if !value.same_bits(old) {
                heap.write_host_property(
                    PropertyId {
                        host: target,
                        field,
                    },
                    value,
                    false,
                )?;
            }
        }
        Ok(())
    }

    fn release_claims(heap: &mut Heap, leaf: Value) -> Result<(), AdapterError> {
        let tracks = get(heap, leaf, Slot::Tracks)?;
        for i in (0..heap.sequence_len(tracks)?).step_by(Track::LEN) {
            if let Some(source) = track_source(heap, tracks, i)? {
                if heap.host_fields(source.host).is_ok()
                    && heap.property_claim(source)?.same_bits(leaf)
                {
                    heap.set_property_claim(source, Value::NULL)?;
                }
                heap.set_field(tracks, i + Track::SourceHost as u32, Value::NULL)?;
                heap.set_field(tracks, i + Track::SourceField as u32, Value::NULL)?;
            }
        }
        Ok(())
    }

    fn leaf_done(&mut self, heap: &mut Heap, leaf: Value, time: f64) -> Result<(), AdapterError> {
        if Self::sample(heap, leaf, time, true).is_err() {
            let activity = get(heap, leaf, Slot::Activity)?;
            return self.begin_cleanup(heap, activity, State::Failed, None);
        }
        self.remove_event(heap, leaf)?;
        Self::release_claims(heap, leaf)?;
        self.node_done(heap, leaf, time)
    }

    fn node_done(
        &mut self,
        heap: &mut Heap,
        current: Value,
        time: f64,
    ) -> Result<(), AdapterError> {
        set(heap, current, Slot::Phase, Value::int(Phase::Done as i64))?;
        self.queue(heap, current, Job::Complete, time, false)
    }

    fn activate(&mut self, heap: &mut Heap, current: Value, time: f64) -> Result<(), AdapterError> {
        let activity = get(heap, current, Slot::Activity)?;
        let action = get(heap, current, Slot::Action)?;
        let forcing = root_get(heap, activity, RootSlot::Forcing)?
            .as_bool()
            .unwrap();
        if phase(heap, current)? != Phase::Starting {
            set(
                heap,
                current,
                Slot::Phase,
                Value::int(Phase::Starting as i64),
            )?;
            set(
                heap,
                current,
                Slot::Remaining,
                get(heap, current, Slot::ChildCount)?,
            )?;
            let iteration = if forcing && kind(heap, current)? == Kind::Repeat {
                payload(heap, action, action::TIMES)?
                    .as_int()
                    .unwrap_or(1)
                    .saturating_sub(1)
            } else {
                0
            };
            set(heap, current, Slot::Iteration, Value::int(iteration))?;
            let parent_index = int(heap, current, Slot::Parent)?;
            let sibling = int(heap, current, Slot::Sibling)?;
            if parent_index >= 0
                && sibling >= 0
                && kind(heap, node(heap, activity, parent_index)?)? == Kind::Parallel
            {
                self.queue(
                    heap,
                    node(heap, activity, sibling)?,
                    Job::Activate,
                    time,
                    false,
                )?;
            }
        }
        if kind(heap, current)? != Kind::Tween {
            set(heap, current, Slot::Phase, Value::int(Phase::Active as i64))?;
            let child = int(heap, current, Slot::FirstChild)?;
            return if child < 0 {
                self.node_done(heap, current, time)
            } else {
                self.queue(
                    heap,
                    node(heap, activity, child)?,
                    Job::Activate,
                    time,
                    false,
                )
            };
        }
        let target = payload(heap, action, TARGET)?;
        let fields = heap.host_fields(target)?;
        let tracks = get(heap, current, Slot::Tracks)?;
        // Check this segment before stopping any other producer. Future segments
        // are intentionally read here, after prior segments and script changes.
        for i in (0..heap.sequence_len(tracks)?).step_by(Track::LEN) {
            let field = track(heap, tracks, i, Track::Field)?.as_int().unwrap() as u32;
            let end = track(heap, tracks, i, Track::End)?;
            if stage::number(heap.table_entry(fields, field)?.1).is_err()
                || heap
                    .validate_property_write(
                        PropertyId {
                            host: target,
                            field,
                        },
                        end,
                    )
                    .is_err()
            {
                return self.begin_cleanup(heap, activity, State::Failed, None);
            }
        }
        for i in (0..heap.sequence_len(tracks)?).step_by(Track::LEN) {
            let field = track(heap, tracks, i, Track::Field)?.as_int().unwrap() as u32;
            let (source, writable) = heap.property_source(PropertyId {
                host: target,
                field,
            })?;
            if !writable {
                return self.begin_cleanup(heap, activity, State::Failed, None);
            }
            for previous in (0..i).step_by(Track::LEN) {
                let field = track(heap, tracks, previous, Track::Field)?
                    .as_int()
                    .unwrap() as u32;
                let other = heap
                    .property_source(PropertyId {
                        host: target,
                        field,
                    })?
                    .0;
                if other.host.same_bits(source.host) && other.field == source.field {
                    return self.begin_cleanup(heap, activity, State::Failed, None);
                }
            }
            let old = heap.property_claim(source)?;
            if present(old) && !old.same_bits(current) {
                let old_activity = get(heap, old, Slot::Activity)?;
                if old_activity.same_bits(activity) {
                    return self.begin_cleanup(heap, activity, State::Failed, None);
                }
                self.begin_cleanup(heap, old_activity, State::Stopped, Some(time))?;
                return self.queue(heap, current, Job::Activate, time, true);
            }
        }
        for i in (0..heap.sequence_len(tracks)?).step_by(Track::LEN) {
            let field = track(heap, tracks, i, Track::Field)?.as_int().unwrap() as u32;
            let start = stage::number(heap.table_entry(fields, field)?.1)?;
            heap.set_field(tracks, i + Track::Start as u32, Value::float(start))?;
            heap.set_field(
                tracks,
                i + Track::Version as u32,
                Value::int(heap.host_write_version(target, field)? as i64),
            )?;
            let source = heap
                .property_source(PropertyId {
                    host: target,
                    field,
                })?
                .0;
            heap.set_field(tracks, i + Track::SourceHost as u32, source.host)?;
            heap.set_field(
                tracks,
                i + Track::SourceField as u32,
                Value::int(i64::from(source.field)),
            )?;
            heap.set_property_claim(source, current)?;
        }
        set(heap, current, Slot::StartedAt, Value::float(time))?;
        set(heap, current, Slot::Phase, Value::int(Phase::Active as i64))?;
        if forcing || action::duration(heap, action)? == 0. {
            self.leaf_done(heap, current, time)
        } else {
            self.schedule(heap, current)
        }
    }

    fn complete(&mut self, heap: &mut Heap, current: Value, time: f64) -> Result<(), AdapterError> {
        let activity = get(heap, current, Slot::Activity)?;
        let parent_index = int(heap, current, Slot::Parent)?;
        if parent_index < 0 {
            return self.begin_cleanup(heap, activity, State::Completed, None);
        }
        let parent = node(heap, activity, parent_index)?;
        match kind(heap, parent)? {
            Kind::Tween => unreachable!("tweens have no children"),
            Kind::Sequence => {
                let sibling = int(heap, current, Slot::Sibling)?;
                if sibling >= 0 {
                    self.queue(
                        heap,
                        node(heap, activity, sibling)?,
                        Job::Activate,
                        time,
                        false,
                    )
                } else {
                    self.node_done(heap, parent, time)
                }
            }
            Kind::Parallel => {
                let remaining = int(heap, parent, Slot::Remaining)? - 1;
                set(heap, parent, Slot::Remaining, Value::int(remaining))?;
                if remaining == 0 {
                    self.node_done(heap, parent, time)?;
                }
                Ok(())
            }
            Kind::Repeat => {
                let action = get(heap, parent, Slot::Action)?;
                let times = payload(heap, action, action::TIMES)?.as_int();
                let iteration = int(heap, parent, Slot::Iteration)?;
                if times.is_some_and(|count| iteration >= count - 1) {
                    return self.node_done(heap, parent, time);
                }
                if let Some(count) = times {
                    let next = if root_get(heap, activity, RootSlot::Forcing)?
                        .as_bool()
                        .unwrap()
                    {
                        count - 1
                    } else {
                        iteration + 1
                    };
                    set(heap, parent, Slot::Iteration, Value::int(next))?;
                }
                self.queue(heap, current, Job::Activate, time, false)
            }
        }
    }

    pub(super) fn begin_cleanup(
        &mut self,
        heap: &mut Heap,
        activity: Value,
        result: State,
        freeze: Option<f64>,
    ) -> Result<(), AdapterError> {
        let status = state(heap, activity)?;
        if terminal(status) || status == State::Cleaning {
            return Ok(());
        }
        debug_assert!(self.cleanup.is_none());
        if self
            .control
            .as_ref()
            .is_some_and(|control| control.activity.same_bits(activity))
        {
            self.control = None;
        }
        if status == State::Running {
            self.running -= 1;
        }
        root_set(
            heap,
            activity,
            RootSlot::State,
            Value::int(State::Cleaning as i64),
        )?;
        // Paused activities freeze at the time of their pause, not takeover time.
        let freeze = if freeze.is_some() && status == State::Paused {
            Some(
                root_get(heap, activity, RootSlot::PausedAt)?
                    .as_float()
                    .unwrap(),
            )
        } else {
            freeze
        };
        self.cleanup = Some(Cleanup {
            activity,
            index: 0,
            result,
            freeze,
        });
        Ok(())
    }

    fn cleanup_one(&mut self, heap: &mut Heap) -> Result<(), AdapterError> {
        let cleanup = self.cleanup.as_mut().unwrap();
        let activity = cleanup.activity;
        let graph = root_get(heap, activity, RootSlot::Graph)?;
        if cleanup.index < heap.sequence_len(graph)? {
            let current = heap.sequence_get(graph, cleanup.index)?;
            cleanup.index += 1;
            let freeze = cleanup.freeze;
            if kind(heap, current)? == Kind::Tween {
                if phase(heap, current)? == Phase::Active {
                    if let Some(time) = freeze
                        && Self::sample(heap, current, time, false).is_err()
                    {
                        cleanup.result = State::Failed;
                    }
                    Self::release_claims(heap, current)?;
                }
                self.remove_event(heap, current)?;
            }
            // Queued tombstones retain only JobNext until they are popped.
            for slot in [Slot::Activity, Slot::Action, Slot::Tracks] {
                set(heap, current, slot, Value::NULL)?;
            }
            return Ok(());
        }
        let result = cleanup.result;
        self.cleanup = None;
        let next = root_get(heap, activity, RootSlot::Next)?;
        let previous = root_get(heap, activity, RootSlot::Previous)?;
        if present(previous) {
            root_set(heap, previous, RootSlot::Next, next)?;
        } else {
            heap.set_root(self.head.unwrap(), next)?;
        }
        if present(next) {
            root_set(heap, next, RootSlot::Previous, previous)?;
        }
        if let Some(Round::Reconcile(scan) | Round::Sample(scan)) = &mut self.round
            && scan.is_some_and(|scan| scan.activity.same_bits(activity))
        {
            *scan = present(next).then_some(Scan {
                activity: next,
                index: 0,
            });
        }
        root_set(heap, activity, RootSlot::State, Value::int(result as i64))?;
        for slot in [
            RootSlot::Description,
            RootSlot::Owner,
            RootSlot::Graph,
            RootSlot::Next,
            RootSlot::Previous,
        ] {
            root_set(heap, activity, slot, Value::NULL)?;
        }
        if !present(self.first(heap)?)
            && let Some(root) = self.head.take()
        {
            heap.release_root(root)?;
        }
        Ok(())
    }

    fn reconcile(
        &mut self,
        heap: &mut Heap,
        activity: Value,
        current: Value,
    ) -> Result<(), AdapterError> {
        let owner = root_get(heap, activity, RootSlot::Owner)?;
        if present(owner) && heap.host_fields(owner).is_err() {
            return self.begin_cleanup(heap, activity, State::Cancelled, None);
        }
        if kind(heap, current)? != Kind::Tween {
            return Ok(());
        }
        let target = payload(heap, get(heap, current, Slot::Action)?, TARGET)?;
        if heap.host_fields(target).is_err() {
            return self.begin_cleanup(heap, activity, State::Cancelled, None);
        }
        if phase(heap, current)? == Phase::Active {
            let tracks = get(heap, current, Slot::Tracks)?;
            for i in (0..heap.sequence_len(tracks)?).step_by(Track::LEN) {
                let field = track(heap, tracks, i, Track::Field)?.as_int().unwrap() as u32;
                let version = track(heap, tracks, i, Track::Version)?.as_int().unwrap() as u64;
                if heap.host_write_version(target, field)? != version {
                    return self.begin_cleanup(heap, activity, State::Stopped, None);
                }
            }
        }
        Ok(())
    }

    pub(super) fn begin_round(&mut self, heap: &Heap, target: f64) -> Result<(), AdapterError> {
        if self.round.is_none() {
            self.clock_round = false;
            self.target_time = target;
            let first = self.first(heap)?;
            self.round = Some(Round::Reconcile(present(first).then_some(Scan {
                activity: first,
                index: 0,
            })));
        }
        Ok(())
    }

    fn scan_next(heap: &Heap, scan: Scan) -> Result<Option<Scan>, AdapterError> {
        let count = heap.sequence_len(root_get(heap, scan.activity, RootSlot::Graph)?)?;
        if scan.index + 1 < count {
            return Ok(Some(Scan {
                index: scan.index + 1,
                ..scan
            }));
        }
        let next = root_get(heap, scan.activity, RootSlot::Next)?;
        Ok(present(next).then_some(Scan {
            activity: next,
            index: 0,
        }))
    }

    fn control_one(&mut self, heap: &mut Heap) -> Result<(), AdapterError> {
        let control = self.control.as_mut().unwrap();
        let activity = control.activity;
        if state(heap, activity)? == State::Cleaning || terminal(state(heap, activity)?) {
            self.control = None;
            return Ok(());
        }
        let graph = root_get(heap, activity, RootSlot::Graph)?;
        if control.index == heap.sequence_len(graph)? {
            self.control = None;
            return Ok(());
        }
        let current = heap.sequence_get(graph, control.index)?;
        control.index += 1;
        let command = control.command;
        let shift = control.shift;
        // A direct assignment or retired future target cancels before control writes.
        self.reconcile(heap, activity, current)?;
        if self.cleanup.is_some() {
            return Ok(());
        }
        if kind(heap, current)? == Kind::Tween && phase(heap, current)? == Phase::Active {
            match command {
                Command::Pause => self.remove_event(heap, current)?,
                Command::Resume => {
                    let start = get(heap, current, Slot::StartedAt)?.as_float().unwrap();
                    set(heap, current, Slot::StartedAt, Value::float(start + shift))?;
                    self.schedule(heap, current)?;
                }
                Command::Finish => self.leaf_done(heap, current, self.time)?,
                _ => unreachable!("only pause/resume/finish scan leaves"),
            }
        }
        Ok(())
    }

    pub(in crate::aside) fn drain(
        &mut self,
        vm: &mut Vm,
        budget: u32,
    ) -> Result<ActivityProgress, AdapterError> {
        let mut work = 0;
        while self.round.is_some() && work < budget {
            work += 1;
            let heap = vm.heap_mut();
            if self.cleanup.is_some() {
                self.cleanup_one(heap)?;
                continue;
            }
            if self.control.is_some() && !matches!(self.round, Some(Round::Reconcile(_))) {
                self.control_one(heap)?;
                continue;
            }
            match self.round.unwrap() {
                Round::Reconcile(Some(scan)) => {
                    let current = node(heap, scan.activity, i64::from(scan.index))?;
                    self.round = Some(Round::Reconcile(Self::scan_next(heap, scan)?));
                    self.reconcile(heap, scan.activity, current)?;
                }
                Round::Reconcile(None) => self.round = Some(Round::Events),
                Round::Events => {
                    let head = self.job_head(heap)?;
                    let job_time = if present(head) {
                        get(heap, head, Slot::JobTime)?.as_float().unwrap()
                    } else {
                        f64::INFINITY
                    };
                    if let Some((&key, &leaf)) = self.events.first_key_value()
                        && f64::from_bits(key.0) <= self.target_time.min(job_time)
                    {
                        // All leaves ending at this instant release their claims
                        // before any following sequence segment can take over.
                        self.leaf_done(heap, leaf, f64::from_bits(key.0))?;
                    } else if present(head) {
                        let (current, job, time) = self.pop_job(heap)?;
                        let activity = get(heap, current, Slot::Activity)?;
                        if present(activity) && state(heap, activity)? == State::Running {
                            match job {
                                Job::Activate => self.activate(heap, current, time)?,
                                Job::Complete => self.complete(heap, current, time)?,
                            }
                        }
                    } else {
                        let first = self.first(heap)?;
                        self.round = Some(Round::Sample(present(first).then_some(Scan {
                            activity: first,
                            index: 0,
                        })));
                    }
                }
                Round::Sample(Some(scan)) => {
                    let current = node(heap, scan.activity, i64::from(scan.index))?;
                    self.round = Some(Round::Sample(Self::scan_next(heap, scan)?));
                    if state(heap, scan.activity)? == State::Running
                        && kind(heap, current)? == Kind::Tween
                        && phase(heap, current)? == Phase::Active
                        && Self::sample(heap, current, self.target_time, false).is_err()
                    {
                        self.begin_cleanup(heap, scan.activity, State::Failed, None)?;
                    }
                }
                Round::Sample(None) => {
                    self.time = self.target_time;
                    self.round = None;
                }
            }
        }
        let woke = if self.round.is_none() {
            if self.head.is_none()
                && !present(self.job_head(vm.heap())?)
                && let Some(root) = self.jobs.take()
            {
                vm.heap_mut().release_root(root)?;
            }
            self.maintain(vm)?
        } else {
            false
        };
        Ok(ActivityProgress {
            work,
            pending: self.round.is_some(),
            woke,
        })
    }

    pub fn advance(
        &mut self,
        vm: &mut Vm,
        seconds: f64,
        budget: u32,
    ) -> Result<ActivityProgress, AdapterError> {
        if !seconds.is_finite() || seconds < self.observed_time {
            return Err(AdapterError::Input(
                "activity clock must be finite and monotonic",
            ));
        }
        self.observed_time = seconds;
        let mut work = 0;
        let mut woke = false;
        // Maintenance has no elapsed time of its own. Finish it before sampling
        // this host clock, while a partially sampled clock round keeps its time.
        if self.round.is_some() && !self.clock_round {
            let progress = self.drain(vm, budget)?;
            if progress.pending {
                return Ok(progress);
            }
            work = progress.work;
            woke = progress.woke;
        }
        if self.round.is_none() {
            if !self.has_active() {
                self.time = seconds;
                return Ok(ActivityProgress {
                    work,
                    pending: false,
                    woke: woke | self.maintain(vm)?,
                });
            }
            self.begin_round(vm.heap(), seconds)?;
            self.clock_round = true;
        }
        let progress = self.drain(vm, budget - work)?;
        Ok(ActivityProgress {
            work: work + progress.work,
            pending: progress.pending,
            woke: woke | progress.woke,
        })
    }
}
