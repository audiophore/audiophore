#![no_main]
//! Fuzz the OSC ingress path with arbitrary bytes.
//!
//! `dispatch_packet` is the untrusted-input surface: it takes a raw UDP
//! datagram straight off the wire, decodes it via `rosc`, and applies every
//! contained message to an `AudioFrameBuilder`. A decode *error* is an expected,
//! valid outcome — the invariant under test is that no input, however
//! malformed or adversarial, makes it panic. We also `snapshot()` afterwards to
//! exercise the frame-production / clamping path on whatever state landed.

use audiophore_audio::{AudioFrameBuilder, dispatch_packet};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut builder = AudioFrameBuilder::new();
    let _ = dispatch_packet(data, &mut builder);
    let _ = builder.snapshot();
});
