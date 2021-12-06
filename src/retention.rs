use std::fmt::Debug;
use anyhow::{bail, Context};
use crate::machine::{Machine, MachineError};
use crate::dataset::{Dataset, CommonOrDivergence::*};
use crate::S;

#[derive(Copy, Clone, Debug)]
pub struct RetentionOpts {
    pub keep_unusual: bool,
    pub run_directly: bool,
}

pub fn apply_retention(
    machine : &mut Machine,
    ds : &mut Dataset,
    opts: RetentionOpts
) -> Result<String, anyhow::Error> {
    unimplemented!()
}
