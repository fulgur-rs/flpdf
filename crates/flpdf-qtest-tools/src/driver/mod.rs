// Task 3 wires this adapter into the production driver binary. Keep this
// independently tested intermediate commit warning-free until that consumer
// lands, then remove the allowance.
#[allow(dead_code)]
pub(crate) mod handle;
