use std::error::Error;
use std::num::NonZeroU32;

use io_uring::squeue::Entry;
use io_uring::types::Timespec;
use tracing::{info, Level};

use rummelplatz::{ring, ControlFlow, RingOperation, SubmissionQueueSubmitter};

const TIMEOUT: Timespec = Timespec::new().sec(1);

#[derive(Debug)]
pub struct TimeoutOp;

impl RingOperation for TimeoutOp {
    type RingData = u32;
    type SetupError = ();
    type TeardownError = ();
    type ControlFlowWarn = ();
    type ControlFlowError = ();

    fn setup<W: Fn(&mut Entry, Self::RingData)>(
        &mut self,
        mut submitter: SubmissionQueueSubmitter<Self::RingData, W>,
    ) -> Result<(), Self::SetupError> {
        info!("[TimeoutOp] Setup with 0");

        let entry = io_uring::opcode::Timeout::new(&TIMEOUT).build();
        submitter.push(entry, 0).unwrap();

        Ok(())
    }

    fn on_completion<W: Fn(&mut Entry, Self::RingData)>(
        &mut self,
        _completion_entry: io_uring::cqueue::Entry,
        ring_data: Self::RingData,
        mut submitter: SubmissionQueueSubmitter<Self::RingData, W>,
    ) -> (
        ControlFlow<Self::ControlFlowWarn, Self::ControlFlowError>,
        Option<Self::RingData>,
    ) {
        info!("[TimeoutOp] got {ring_data}");
        if ring_data == 3 {
            let entry = io_uring::opcode::Timeout::new(&TIMEOUT).count(10).build();

            submitter.push(entry, ring_data + 1).unwrap();
            return (ControlFlow::Exit, None);
        }

        let entry = io_uring::opcode::Timeout::new(&TIMEOUT).build();

        submitter.push(entry, ring_data + 1).unwrap();
        (ControlFlow::Continue, None)
    }

    fn on_teardown_completion<W: Fn(&mut Entry, Self::RingData)>(
        &mut self,
        _completion_entry: io_uring::cqueue::Entry,
        _ring_data: Self::RingData,
        _submitter: SubmissionQueueSubmitter<Self::RingData, W>,
    ) -> Result<(), Self::TeardownError> {
        info!("[TimeoutOp] teardown");
        Ok(())
    }
}

ring! {
    test_ring,
    timout_op: super::TimeoutOp
}

fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .compact()
        .with_max_level(Level::TRACE)
        .init();

    let timout_op = TimeoutOp;
    let mut ring = test_ring::Ring::new(
        test_ring::Ring::new_raw_ring(NonZeroU32::new(128).unwrap())?,
        None,
        timout_op,
    );

    ring.run::<(), (), ()>()?;

    Ok(())
}
