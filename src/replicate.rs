use std::fmt::Debug;
use anyhow::{bail, Context};
use crate::machine::{Machine, MachineError};
use crate::dataset::{Dataset, CommonOrDivergence::*};
use crate::S;

#[derive(Clone, Debug)]
pub struct ReplicateDatasetOpts {
    pub do_rollback: bool,
    pub verbose_send: bool,
    pub verbose_recv: bool,
    pub simple_incremental: bool,
    pub dryrun_recv: bool,
    pub app_verbose: bool,
    pub take_snap_now: Option<String>,  // If there is Some(name), it is implied that the user requests zfs-rs to take a snapshot at this time.
}

pub fn replicate_dataset(
    src_machine : &mut Machine,
    src_ds : &mut Dataset,
    dst_machine : &mut Machine,
    dst_ds: &mut Dataset,
    opts: ReplicateDatasetOpts,
) -> Result<String, anyhow::Error> {
    //Ok in case of successful send or nothing to do (both up-to-date)

    dst_ds.append_relative(src_ds);

    if let Some(rand_snap_name) = opts.take_snap_now {
        eprintln!("Taking snapshot {}:{}@{} (requested by --take-snap-now).", src_machine, src_ds.fullname(), rand_snap_name);
        src_machine.create_snap(src_ds, &rand_snap_name).context("Failed to take snapshot (requested by --take-snap-now).")?;
    }
    src_machine.get_snaps(src_ds)?;  // No handling it if this fails.
    let dst_dataset_existed = match dst_machine.get_snaps(dst_ds) {
        Ok(_) => true,
        Err(MachineError::NoDataset) => false,
        Err(e) => return Err(e).context("Failed to get dst_ds snapshot list.")
    };
    if opts.app_verbose {
        eprintln!("Got {} snapshot(s) from {}:{}.", src_ds.snaps.len(), src_machine, src_ds.fullname());
        if dst_dataset_existed {
            eprintln!("Got {} snapshot(s) from {}:{}.", dst_ds.snaps.len(), dst_machine, dst_ds.fullname());
        } else {
            eprintln!("Dataset {} not found in {}; continuing.", dst_ds.fullname(), dst_machine);
        }
    }

    let last_comm = if dst_dataset_existed {
        match src_ds.last_common_or_divergence(&dst_ds) {
            NoneInCommon => bail!(format!("Datasets {}:{} and {}:{} have no snapshots in common :c", src_machine, src_ds.fullname(), dst_machine, dst_ds.fullname())),
            Divergence(s) => {
                if !opts.do_rollback { bail!(format!("Datasets {}:{} and {}:{} diverge; you'll have to destroy dst_ds manually and try again.", src_machine, src_ds.fullname(), dst_machine, dst_ds.fullname())) }
                s
            }
            Common(s) => s
        }
    } else { src_ds.oldest_snap() };  // Not exactly true at this point, **but it will be** once we do fullsend_s(&src_ds, ds.oldest_snap())
    if opts.app_verbose {
        if dst_dataset_existed {
            eprintln!("Figured out {} as the last common snapshot.", last_comm.name);
        } else {
            eprintln!("Will begin with a full-send of {}.", last_comm.name);
        }
    }

    if !dst_dataset_existed {
        if opts.app_verbose {
            eprintln!("Ensuring the destination dataset's ancestors exist.");
        }
        dst_machine.create_ancestors(dst_ds).context(format!("Failed to create {}:{}'s ancestors!", dst_machine, dst_ds.fullname()))?;
        let sendside_full = src_machine.fullsend_s(&src_ds, src_ds.oldest_snap(), opts.verbose_send);
        let recvside_full = dst_machine.recv(&dst_ds, opts.do_rollback, opts.dryrun_recv, opts.verbose_recv);
        let pipeline = sendside_full | recvside_full;
        let tmp_cmdline_string = format!("{:?}", pipeline);
        if opts.app_verbose {
            eprintln!("{}", tmp_cmdline_string);
        }
        let fullsend_result = pipeline.join();
        match fullsend_result {
            Ok(statuscode) => if !statuscode.success() { bail!("The commandline {:?} spawned successfully but exited with an error.", tmp_cmdline_string) },
            Err(e) => return Err(e).context(format!("Failed to spawn commandline {}.", tmp_cmdline_string))
        }
        if opts.app_verbose {
            eprintln!("Full-send of {} successful.", last_comm.name);
        }
    }

    if last_comm == src_ds.newest_snap() {
        return if dst_dataset_existed {
            Ok(format!("Nothing to send because last common snap {} is the last snap in {}.", last_comm, src_ds))
        }
        else {
            Ok(format!("Successfully sent {} to {}.", src_ds, dst_ds))
        }
    }

    if opts.app_verbose {
        eprintln!("Now doing incremental send from {} to {}.", last_comm.name, src_ds.newest_snap());
    }
    let sendside = src_machine.send_from_s_till_last(&src_ds, last_comm, opts.simple_incremental, opts.verbose_send);
    let destside = dst_machine.recv(&dst_ds, opts.do_rollback, opts.dryrun_recv, opts.verbose_recv);
    let pipeline = sendside | destside;
    let tmp_cmdline_string = format!("{:?}", pipeline);
    if opts.app_verbose {
        eprintln!("{}", tmp_cmdline_string);
    }
    // eprintln!("Executing {} | {}", sendside.to_cmdline_lossy(), destside.to_cmdline_lossy());
    let send_result = pipeline.join();
    match send_result {
        Ok(statuscode) => if !statuscode.success() { bail!("The commandline {} spawned successfully but exited with an error.", tmp_cmdline_string) },
        Err(e) => return Err(e).context(format!("Failed to spawn commandline {}.", tmp_cmdline_string)),
    }

    Ok(format!("Successfully synched {} to {}.", src_ds, dst_ds))
}