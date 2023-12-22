# 🎡 Rummelplatz

A low level library to interact with the [io-uring](https://en.wikipedia.org/wiki/Io_uring) api with Rust 🦀.

This is an attempt to reduce boilerplate.
This library **does not** provide special safety guards.

## 📖 How to

Checkout this [very basic example](examples/basic.rs).

0. [RTFM¹](https://man.archlinux.org/man/io_uring.7.en)
    - ¹ **R**ead **T**he **F**abulous [**M**anpage]((https://man.archlinux.org/man/io_uring.7.en))
1. Implement the `RingOperation` trait for your ring operation.
   ```rust
    #[derive(Debug)]
    pub struct MyRingOp;
     
    impl RingOperation for MyRingOp {
        // Data passed with an io_uring request (io_urings user data)
        // Example: this may hold connection specific data for a TCP connection
        type RingData = ();
   
        // Error type that may occur during setup
        type SetupError = ();
        // Error type that may occur during tear down
        type TeardownError = ();
        // Error type indicate a warning during `on_completion`
        type ControlFlowWarn = ();
        // Error type indicate an error during `on_completion`
        type ControlFlowError = ();
    
        fn setup<W: Fn(&mut Entry, Self::RingData)>(&mut self, submitter: SubmissionQueueSubmitter<Self::RingData, W>) -> Result<(), Self::SetupError> {
            // This function is called once before the ring enters its "normal" operation loop.
            // Example: here you may setup a TCP socket and submit a sqe to accept clients
            Ok(())
        }
    
        fn on_completion<W: Fn(&mut Entry, Self::RingData)>(&mut self, completion_entry: io_uring::cqueue::Entry, ring_data: Self::RingData, submitter: SubmissionQueueSubmitter<Self::RingData, W>) -> (ControlFlow<Self::ControlFlowWarn, Self::ControlFlowError>, Option<Self::RingData>) {
            // This function is called whenever we got a cqe for `MyRingOp`.
            // The *magic* should happen here.
   
            // Example: here you may process any cqe
   
            // depending on your operation you can set the control flow of this ring
            //     V
            (ControlFlow::Continue, None)
            //                       /\
            // There are multi-shot operations. Whenever you expect to receive `ring_data` again in the future you have
            // to give it back after mutating it.
        }
    
        fn on_teardown_completion<W: Fn(&mut Entry, Self::RingData)>(&mut self, _completion_entry: io_uring::cqueue::Entry, _ring_data: Self::RingData, _submitter: SubmissionQueueSubmitter<Self::RingData, W>) -> Result<(), Self::TeardownError> {
            // When the ring is shutting down all requests are canceled.
            // This function is basically the same as `on_completion` except it is only called once for a specific request.
   
            // Example: here you may release/free/drop your resources
   
            Ok(())
        }
    }
    ```
2. Create an IoUring with your RingOperation
   ```rust
    rummelplatz::ring! {
        my_ring,
        my_ring_op: super::MyRingOp,
        my_other_op: super::MyOtherRingOp
    }
   ```
3. Run it
   ```rust
    // create your operations
    let my_op = MyRingOp;
   
    // create the ring
    let mut ring = my_ring::Ring::new(
        my_ring::Ring::new_raw_ring(NonZeroU32::new(128).unwrap())?,
        None,
        my_op,
    );
    
    // run it
    ring.run();
   ```

## ⚠️ Foot guns

- `opcode::MsgRingData` cqe flags should always contain `IORING_CQE_F_MORE`
   ```rust
    let msg = opcode::MsgRingData::new(
        Fd, 0, user_data,
        Some(IORING_CQE_F_MORE),
    )
    .build()
    .user_data(0);

    if let Err(e) = unsafe { submitter.push_raw(msg) } {
        //...
    }
   ```
  Rummelplatz is keeping track of requests currently in flight. To prevent `MsgRingData`
  cqes to affect that internal counter they have to be flagged as if they were coming from a
  multi-shot request.
- There are probably more :))

## ❤️ Special thanks to

- everyone creating and maintaining io-uring and the Linux kernel
- [tokio-rs/io-uring](https://github.com/tokio-rs/io-uring)