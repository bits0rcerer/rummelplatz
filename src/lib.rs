use std::collections::VecDeque;
use std::fmt::Debug;
use std::iter::zip;
use std::marker::PhantomData;
use std::num::NonZeroUsize;

pub use io_uring;
use io_uring::cqueue::Entry;
use io_uring::squeue::{EntryMarker, PushError};
use io_uring::SubmissionQueue;
use tracing::{trace, warn};

#[derive(Debug)]
#[allow(dead_code)]
pub enum ControlFlow<Warn, Error> {
    Continue,
    Exit,
    Warn(Warn),
    Error(Error),
}

type CompletionResult<W, E, D> = (ControlFlow<W, E>, Option<D>);

pub trait RingOperation: Debug {
    type RingData;
    type SetupError;
    type TeardownError;
    type ControlFlowWarn;
    type ControlFlowError;

    fn setup<W: Fn(&mut io_uring::squeue::Entry, Self::RingData)>(
        &mut self,
        submitter: SubmissionQueueSubmitter<Self::RingData, W>,
    ) -> Result<(), Self::SetupError>;
    fn on_completion<W: Fn(&mut io_uring::squeue::Entry, Self::RingData)>(
        &mut self,
        completion_entry: Entry,
        ring_data: Self::RingData,
        submitter: SubmissionQueueSubmitter<Self::RingData, W>,
    ) -> CompletionResult<Self::ControlFlowWarn, Self::ControlFlowError, Self::RingData>;
    fn on_teardown_completion<W: Fn(&mut io_uring::squeue::Entry, Self::RingData)>(
        &mut self,
        completion_entry: Entry,
        ring_data: Self::RingData,
        submitter: SubmissionQueueSubmitter<Self::RingData, W>,
    ) -> Result<(), Self::TeardownError>;
}

pub struct SubmissionQueueSubmitter<
    'a,
    'b,
    'c,
    D,
    W: Fn(&mut E, D),
    E: EntryMarker = io_uring::squeue::Entry,
> {
    sq: &'a mut SubmissionQueue<'b, E>,
    backlog: &'c mut VecDeque<Box<[E]>>,
    backlog_limit: Option<NonZeroUsize>,
    wrapper: W,
    marker: PhantomData<D>,
}

impl<'a, 'b, 'c, D, W: Fn(&mut E, D), E: EntryMarker>
    SubmissionQueueSubmitter<'a, 'b, 'c, D, W, E>
{
    pub fn new(
        sq: &'a mut SubmissionQueue<'b, E>,
        backlog: &'c mut VecDeque<Box<[E]>>,
        backlog_limit: Option<NonZeroUsize>,
        wrapper: W,
    ) -> Self {
        Self {
            sq,
            backlog,
            backlog_limit,
            wrapper,
            marker: Default::default(),
        }
    }

    #[inline]
    pub fn push(&mut self, entry: E, data: D) -> Result<(), PushError> {
        self.push_multiple([entry], [data])
    }

    /// # Safety
    /// The caller must ensure that the userdata is valid and can be understood by rummelplatz.
    #[inline]
    pub unsafe fn push_raw(&mut self, entry: E) -> Result<(), PushError> {
        self.push_multiple_raw([entry])
    }

    #[inline]
    pub fn push_multiple<const N: usize>(
        &mut self,
        mut entries: [E; N],
        data: [D; N],
    ) -> Result<(), PushError> {
        for (entry, data) in zip(entries.iter_mut(), data.into_iter()) {
            (self.wrapper)(entry, data);
        }

        unsafe { self.push_multiple_raw(entries) }
    }

    /// # Safety
    /// The caller must ensure that userdata of all entries are valid and can be understood by rummelplatz.
    #[inline]
    pub unsafe fn push_multiple_raw<const N: usize>(
        &mut self,
        entries: [E; N],
    ) -> Result<(), PushError> {
        trace!("push sqes: {entries:?}");

        match self.sq.push_multiple(entries.as_slice()) {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!(
                    "exceeding ring submission queue, using backlog... (may degrade performance)"
                );

                match self.backlog_limit {
                    None => {
                        self.backlog.push_back(entries.into());
                        Ok(())
                    }
                    Some(limit) => {
                        if self.backlog.len() + entries.len() <= limit.get() {
                            self.backlog.push_back(entries.into());
                            Ok(())
                        } else {
                            Err(e)
                        }
                    }
                }
            }
        }
    }
}

#[allow(dead_code)]
impl<'a, 'b, 'c, D, W: Fn(&mut E, D), E: EntryMarker>
    SubmissionQueueSubmitter<'a, 'b, 'c, D, W, E>
{
    #[inline]
    pub fn push_slice(&mut self, mut entries: Box<[E]>, data: Box<[D]>) -> Result<(), PushError> {
        for (entry, data) in zip(entries.iter_mut(), Vec::from(data).into_iter()) {
            (self.wrapper)(entry, data);
        }

        unsafe { self.push_slice_raw(entries) }
    }

    /// # Safety
    /// The caller must ensure that userdata of all entries are valid and can be understood by rummelplatz.
    #[inline]
    pub unsafe fn push_slice_raw(&mut self, entries: Box<[E]>) -> Result<(), PushError> {
        match self.sq.push_multiple(&entries) {
            Ok(()) => Ok(()),
            Err(e) => match self.backlog_limit {
                None => {
                    self.backlog.push_back(entries);
                    Ok(())
                }
                Some(limit) => {
                    if self.backlog.len() + entries.len() <= limit.get() {
                        self.backlog.push_back(entries);
                        Ok(())
                    } else {
                        Err(e)
                    }
                }
            },
        }
    }
}

#[macro_export]
macro_rules! ring {
    ($ring_name:ident, $($ring_op_name:ident: $ring_op:path),+) => {
        pub mod $ring_name {
            use std::num::{NonZeroU32, NonZeroUsize};
            use std::collections::VecDeque;
            use std::fmt::{Debug, Formatter};
            use std::marker::PhantomData;
            use std::os::fd::{AsRawFd, RawFd};
            use tracing::{debug, error, trace, warn};
            use $crate::io_uring::squeue::PushError;
            use $crate::io_uring::types::Timespec;
            use $crate::io_uring::squeue::Flags;
            use $crate::{ControlFlow, RingOperation, SubmissionQueueSubmitter};

            // Enforce trait on $ring_op
            const _: () = {
                fn assert_ring_operation<T: RingOperation>() {}
                fn assert_all() {
                    $(assert_ring_operation::<$ring_op>());+
                }
            };

            #[derive(Debug)]
            #[allow(non_camel_case_types)]
            pub enum UserData {
                $($ring_op_name(<$ring_op as RingOperation>::RingData)),+,
                Cancel(u64),
            }

            impl From<UserData> for u64 {
                #[inline]
                fn from(value: UserData) -> u64 {
                    Box::new(value).into()
                }
            }

            impl From<Box<UserData>> for u64 {
                #[inline]
                fn from(value: Box<UserData>) -> u64 {
                    unsafe { std::mem::transmute(value) }
                }
            }

            impl UserData {
                #[inline]
                unsafe fn from_raw(user_data: u64) -> Box<Self> {
                    std::mem::transmute(user_data)
                }
            }

            #[derive(Debug, thiserror::Error)]
            pub enum RingError<SetupError, CompletionError, TeardownError> {
                #[error("ring operation setup failed: {}", 0)]
                Setup(SetupError),

                #[error("ring operation failed to complete: {}", 0)]
                Completion(CompletionError),

                #[error("ring teardown failed: {}", 0)]
                Teardown(TeardownError),

                #[error("ring api error: {}", 0)]
                Api(#[from] std::io::Error),

                #[error("unable to push to submission queue: {}", 0)]
                Push(#[from] PushError),
            }

            pub struct Ring {
                ring: $crate::io_uring::IoUring,
                backlog: VecDeque<Box<[$crate::io_uring::squeue::Entry]>>,
                backlog_limit: Option<NonZeroUsize>,
                $($ring_op_name: $ring_op),+,
            }

            impl Debug for Ring {
                fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                    let operations = ($(&self.$ring_op_name),+);

                    if f.alternate() {
                        write!(f, r"Ring: {{
    backlog_limit: {:#?},
    backlog: {:#?},
    operations: {:#?},
}}", self.backlog_limit, self.backlog, operations)
                    } else {
                        write!(f, r"Ring: {{ backlog_limit: {:?}, backlog: {:?}, operations: {:?} }}", self.backlog_limit, self.backlog, operations)
                    }
                }
            }

            impl Ring
            {
                pub fn new_raw_ring(ring_size: NonZeroU32) -> std::io::Result<$crate::io_uring::IoUring> {
                    $crate::io_uring::IoUring::builder()
                        .setup_single_issuer()
                        .setup_coop_taskrun()
                        .setup_defer_taskrun()
                        .build(ring_size.get())
                }

                #[tracing::instrument(skip_all)]
                pub fn new(ring: $crate::io_uring::IoUring, backlog_limit: Option<NonZeroUsize>, $($ring_op_name: $ring_op),+) -> Self {
                    Self {
                        ring,
                        backlog: Default::default(),
                        backlog_limit,
                        $($ring_op_name),+
                    }
                }

                #[inline]
                fn sqe_wrapper(e: &mut $crate::io_uring::squeue::Entry, user_data: UserData) {
                    take_mut::take(e, |e| e.user_data(user_data.into()));
                }

                #[tracing::instrument(skip_all)]
                pub fn run<SetupError, CompletionError, TeardownError>(&mut self) -> Result<(), RingError<SetupError, CompletionError, TeardownError>>
                where
                    SetupError: Debug $(+ std::convert::From<<$ring_op as RingOperation>::SetupError>)+,
                    CompletionError: Debug $(+ std::convert::From<<$ring_op as RingOperation>::ControlFlowError>)+,
                    TeardownError: Debug $(+ std::convert::From<<$ring_op as RingOperation>::TeardownError>)+,
                {
                    let mut result = Ok(());
                    let (submit, mut sq, mut cq) = self.ring.split();

                    $(if let Err(e) = self.$ring_op_name.setup(SubmissionQueueSubmitter::new(
                        &mut sq,
                        &mut self.backlog,
                        self.backlog_limit,
                        |e, d| Self::sqe_wrapper(e, UserData::$ring_op_name(d)),
                    )) {
                        return Err(RingError::Setup(e.into()));
                    })+

                    unsafe {
                        'ring_loop: loop {
                            sq.sync();
                            submit.submit_and_wait(1)?;

                            while let Some(entries) = self.backlog.pop_front() {
                                trace!("push from backlog");
                                if let Err(_) = sq.push_multiple(&entries) {
                                    self.backlog.push_front(entries);
                                    break;
                                }
                            }

                            cq.sync();
                            'completion_loop: for cqe in cq.by_ref() {
                                trace!("> CQE: {cqe:?}");
                                if cqe.user_data() == 0 {
                                    trace!("dropped {cqe:?}");

                                    // ignore
                                    continue;
                                }

                                let mut user_data = UserData::from_raw(cqe.user_data());
                                trace!("> CQE userdata: {user_data:?}");
                                let flow = match *user_data {
                                    $(UserData::$ring_op_name(data) => {
                                        let (flow, new_data) = self.$ring_op_name.on_completion(
                                            cqe,
                                            data,
                                            SubmissionQueueSubmitter::new(
                                                &mut sq,
                                                &mut self.backlog,
                                                self.backlog_limit, |e, d| Self::sqe_wrapper(e, UserData::$ring_op_name(d)),
                                            ),
                                        );
                                        if let Some(new_data) = new_data {
                                            *user_data = UserData::$ring_op_name(new_data);
                                            std::mem::forget(std::hint::black_box(user_data));
                                        }

                                        flow
                                    }),+
                                    UserData::Cancel(_) => unreachable!(),
                                };

                                match flow {
                                    ControlFlow::Exit => break 'ring_loop,
                                    ControlFlow::Error(e) => {
                                        result = Err(RingError::Completion(e.into()));
                                        break 'ring_loop;
                                    }
                                    ControlFlow::Warn(e) => {
                                        warn!("unable to handle ring completion entry: {e:?}");
                                        continue 'completion_loop;
                                    }
                                    ControlFlow::Continue => {}
                                }
                            }
                        }
                    }

                    debug!("shutting down ring...");
                    unsafe {
                        let cancel = $crate::io_uring::opcode::AsyncCancel2::new($crate::io_uring::types::CancelBuilder::any())
                            .build()
                            .user_data(0);
                        sq.push(&cancel)?;

                        let cancel_timeout = $crate::io_uring::opcode::Nop::new()
                            .build()
                            .flags(Flags::IO_DRAIN)
                            .user_data(UserData::Cancel(u64::MAX).into());

                        sq.push(&cancel_timeout)?;
                    }

                    unsafe {
                        'cancel_loop: loop {
                            sq.sync();
                            submit.submit_and_wait(1)?;

                            cq.sync();
                            for cqe in cq.by_ref() {
                                trace!("> CQE: {cqe:?}");
                                if cqe.user_data() == 0 {
                                    trace!("dropped {cqe:?}");

                                    // ignore
                                    continue;
                                }

                                let user_data = UserData::from_raw(cqe.user_data());
                                trace!("> CQE userdata: {user_data:?}");
                                let teardown_result = match *user_data {
                                    $(UserData::$ring_op_name(data) => self.$ring_op_name.on_teardown_completion(cqe, data, SubmissionQueueSubmitter::new(
                                        &mut sq,
                                        &mut self.backlog,
                                        self.backlog_limit,
                                        |e, d| Self::sqe_wrapper(e, UserData::$ring_op_name(d)),
                                    ))),+,
                                    UserData::Cancel(u64::MAX) => break 'cancel_loop,
                                    UserData::Cancel(_) => unreachable!(),
                                };

                                if let Err(e) = teardown_result {
                                    error!("unable to handle ring completion entry on teardown: {e:?}");
                                    result = Err(RingError::Teardown(e.into()));
                                }
                            }
                        }
                    }

                    debug!("ring finished: {result:?}");
                    result
                }
            }

            impl AsRawFd for Ring {
                fn as_raw_fd(&self) -> RawFd {
                    self.ring.as_raw_fd()
                }
            }
        }
    }
}
