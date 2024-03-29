use std::fmt::Debug;

use crate::machine::{Machine};
use crate::dataset::{Dataset, };

#[derive(Copy, Clone, Debug)]
pub struct RetentionOpts {
    pub keep_unusual: bool,
    pub run_directly: bool,
}

#[allow(warnings)]
pub fn apply_retention(
    machine : &mut Machine,
    ds : &mut Dataset,
    opts: RetentionOpts
) -> Result<String, anyhow::Error> {
    unimplemented!()
}
