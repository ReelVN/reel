# V03 shared-session Task boundary correction

Supersedes the single-task scheduling paragraph in the initial 40d1b9d draft.
One Vm owns **one Heap, FrameSlots, immutable program and module/global set**.
`VmLimits.max_tasks` bounds a preallocated, capacity-charged TaskState pool.
Tasks execute sequentially on that shared heap; no threads or per-task VM exist.

Public changes:

- `start(function, arguments, owner, context) -> Result<TaskId, VmError>` and
  `start_closure(...) -> Result<TaskId, VmError>` create independent live tasks.
- `run_slice(task: TaskId, budget, now) -> Result<RunOutcome, VmError>` explicitly
  selects a task. Waiting A does not block starting/running UI task B.
- HostTicket, WaitId and DeliveryId carry TaskId as well as heap/epoch/serial.
  Host argument/result reads locate that task; respond/complete resume only it.
- `cancel_task(TaskId)` revokes exactly one task. `cancel(owner)` revokes every
  task belonging to that owner and the owner's external single roots.
  Selective task cancellation does not release another task's or shared module roots.
- Delivery disposal frees that task slot for reuse; old IDs/tickets never validate
  against the next occupant. A terminal delivery stays rooted until transfer/drop.

TaskState owns its Script/Native stack, unwind/caught envelopes, waiting/terminal/
cancel state, delivery root and equality cursor/root state. Frame and native
serials remain VM-global. Root windows are private to a task; all are provided
through the same FrameSlots scanner and budget. Capture/byte scratch used only
inside an atomic operation can remain VM-wide. Pool storage and retained windows
remain charged when tasks become idle. Program/global objects stay shared.

Implementation reuses the existing single-task state machine by selecting/swapping
one TaskState at a time, with no allocation or copying of Values during selection.
The bounded pool stores all other states; switching never changes logical root
identities. Borrowing host arguments prevents mutable selection/reentry.

Required cross-task check: A holds a shared mutable cell/object across native
Pending. B writes that same cell/global and returns. GC with both task root sets
present preserves aliasing; A resumes and reads B's value. Cancelling B, including
its outstanding ticket, leaves A's wait and roots usable. Also cover multiple
unclaimed deliveries and task-slot generation reuse.

Finally correction: select the innermost exception target before discarding an
active cleanup record. If the target lies inside that active finally, retain the
outer pending return/throw. If it escapes, replace that exit and retain its value
as cause. Tests cover explicit throw, type error, native Error and callback Error,
each with pending return and pending throw. Normal return/Leave still requires the
missing B01 shared cleanup selector already reported; no format/opcode is copied.
