use std::fmt::Debug;
use std::process::{Child, Command, Stdio};
use anyhow::{anyhow, bail, Context};
use crate::machine::{Machine, MachineError};
use crate::dataset::{Dataset, find_mrcud};
use crate::dataset::MRCUD::*;
use crate::progressbar::do_progressbar_from_zfs_send_stderr;

#[derive(Clone, Debug)]
pub struct ReplicateDatasetOpts {
    pub use_rollback_flag_on_recv: bool,
    pub allow_divergent_destination: bool,
    pub init_nonexistent_destination: bool,
    pub simple_incremental: bool,
    pub app_verbose: bool,
    pub take_snap_now: Option<String>,
    pub ratelimit: Option<String>
}

pub fn replicate_dataset_cli(
    src_machine : &mut Machine,
    src_ds : &mut Dataset,
    dst_machine : &mut Machine,
    dst_ds: &mut Dataset,
    mut opts: ReplicateDatasetOpts,
) -> Result<String, anyhow::Error> {
    dst_ds.append_relative(src_ds);

    src_machine.get_snaps(src_ds).context(format!(r#"Unable to get snapshots for "{src_machine}:{src_ds}"."#))?;  // No handling it if this fails.
    let dst_dataset_existed = match dst_machine.get_snaps(dst_ds) {
        Ok(_) => true,
        Err(MachineError::NoDataset) => false,
        Err(e) => return Err(e).context(format!(r#"Unable to get snapshots for "{dst_machine}:{dst_ds}"."#))
    };
    if opts.app_verbose {
        eprintln!(r#"There are {} snapshot(s) in "{src_machine}:{src_ds}"."#, src_ds.snaps.len());
        if dst_dataset_existed {
            eprintln!(r#"There are {} snapshot(s) in "{dst_machine}:{dst_ds}"."#, dst_ds.snaps.len());
        } else {
            eprintln!(r#"Dataset "{dst_machine}:{dst_ds}" not found; continuing."#);
        }
    }

    if !dst_dataset_existed && !opts.init_nonexistent_destination {
        return Err(anyhow!(r#"Dataset "{dst_machine}:{dst_ds}" does not exist and full send (--init-empty) not requested."#));
    }
    if !dst_dataset_existed && opts.init_nonexistent_destination {
        if dst_ds.is_pool_root() {
            bail!(r#"Dataset "{dst_machine}:{dst_ds}" does not exist and it cannot be created via full send because it is top-level."#);
        }
        if opts.app_verbose {
            eprintln!(r#"Ensuring "{dst_machine}:{dst_ds}"'s ancestors exist."#);
        }
        dst_machine.create_ancestors(dst_ds).context(format!(r#"Failed to create "{dst_machine}:{dst_ds}"'s ancestors!"#))?;
        if let Some(snap_name) = opts.take_snap_now.take() {
            eprintln!(r#"Taking snapshot "{src_machine}:{src_ds}@{snap_name}" (requested by --take-snap-now)."#);
            src_machine.create_snap_with_name(src_ds, &snap_name).context("Failed to take snapshot (requested by --take-snap-now).")?;
        }
        let mut source_send_cmd = src_machine.fullsend_s(&src_ds, src_ds.oldest_snap());
        let mut destination_recv_cmd = dst_machine.recv(&dst_ds, opts.use_rollback_flag_on_recv);
        let (mut source_send_process,
            mut destination_recv_process,
            pv_ratelimit_option
        ) = pipe_with_ratelimit(&mut source_send_cmd, &mut destination_recv_cmd, &opts.ratelimit)?;
        do_progressbar_from_zfs_send_stderr(source_send_process.stderr.take().unwrap());
        let source_send_finished = source_send_process.wait().unwrap();
        let destination_recv_finished = destination_recv_process.wait().unwrap();
        if let Some(mut pv_process) = pv_ratelimit_option {
            pv_process.wait().unwrap();
        }
        if !source_send_finished.success() || !destination_recv_finished.success() {
            return Err(anyhow!("There was a problem with the zfs-send|zfs-recv processes. Exit status: send {source_send_finished}, recv {destination_recv_finished}"));
        }
        if opts.app_verbose {
            eprintln!(r#"Full-send of "{src_machine}:{src_ds}@{src_oldest_name}" successful."#, src_oldest_name=&src_ds.oldest_snap().name);
        }
        dst_machine.get_snaps(dst_ds).expect("Application bug: no snaps in destination after full-send successfully performed.");
    }

    let mrcud = find_mrcud(&src_ds, &dst_ds);
    // Check for reasons to bail early.
    match mrcud {
        NoneInCommon =>
            return Err(anyhow!(r#"Datasets "{src_machine}:{src_ds}" and "{dst_machine}:{dst_ds}" have no snapshots in common."#)),

        UpToDate(mrc) if !opts.take_snap_now.is_some() =>
            return Ok(format!(r#"Nothing to do: datasets "{src_machine}:{src_ds}" and "{dst_machine}:{dst_ds}" are already up-to-date at snapshot "{mrc}"."#)),

        DestinationHasMore(mrc) => {
            if !opts.take_snap_now.is_some() {
                return Err(anyhow!(r#"Source dataset "{src_machine}:{src_ds}"'s most recent snapshot, "{mrc}", is also found in destination dataset "{dst_machine}:{dst_ds}", but there are additional, newer snapshots at the destination.
Hint: perhaps you meant to send from "{dst_machine}:{dst_ds}" to "{src_machine}:{src_ds}"?"#));
            } else {
                if !opts.allow_divergent_destination {
                    return Err(anyhow!(r#"Datasets "{src_machine}:{src_ds}" and "{dst_machine}:{dst_ds}" would diverge after taking snapshot "{}" and --allow-divergent-destination not given."#, opts.take_snap_now.unwrap()))
                }
            }
        }

        Divergence(mrc) if !opts.allow_divergent_destination =>
            return Err(anyhow!(r#"Datasets "{src_machine}:{src_ds}" and "{dst_machine}:{dst_ds}" diverge after "{mrc}" and --allow-divergent-destination not given."#)),
        _ => ()
    }

    let most_recent_common_snap = match mrcud {
        Divergence(s) | SourceHasMore(s) | UpToDate(s) | DestinationHasMore(s) => s,
        _ => unreachable!()
    }.clone();

    if opts.app_verbose {  // TODO: use log::info!() or something instead of checking conditionals.
        eprintln!(r#"Figured out "{}" as the most recent common snapshot."#, most_recent_common_snap.name);
    }

    if let Some(snap_name) = opts.take_snap_now {
        eprintln!(r#"Taking snapshot "{src_machine}:{src_ds}@{snap_name}" (requested by --take-snap-now)."#);
        src_machine.create_snap_with_name(src_ds, &snap_name).context("Failed to take snapshot (requested by --take-snap-now).")?;
    }

    if opts.app_verbose {
        match opts.simple_incremental {
            true => eprintln!(r#"Now sending delta between "{}" to "{}"."#, most_recent_common_snap.name, src_ds.newest_snap()),
            false => eprintln!(r#"Now sending deltas of all intervening snapshots between "{}" to "{}"."#, most_recent_common_snap.name, src_ds.newest_snap())
        }
    }

    let mut source_send_cmd = src_machine.send_from_s_till_newest(&src_ds, &most_recent_common_snap, opts.simple_incremental);
    let mut destination_recv_cmd = dst_machine.recv(&dst_ds, opts.use_rollback_flag_on_recv);

    let (mut source_send_process,
        mut destination_recv_process,
        pv_ratelimit_option
    ) = pipe_with_ratelimit(&mut source_send_cmd, &mut destination_recv_cmd, &opts.ratelimit)?;

    // At this point the transfer process is underway and we're not involved in moving data.
    // We do have to draw a progress bar. To do so take the standard error stream from the
    // sending process, where we find a header with the estimated amount of data to send as well
    // as periodic updates of progress.
    do_progressbar_from_zfs_send_stderr(source_send_process.stderr.take().unwrap());

    let source_send_finished = source_send_process.wait().unwrap();
    let destination_recv_finished = destination_recv_process.wait().unwrap();
    if let Some(mut pv_process) = pv_ratelimit_option {
        pv_process.wait().unwrap();
    }

    if !source_send_finished.success() || !destination_recv_finished.success() {
        return Err(anyhow!("There was a problem with the zfs-send|zfs-recv processes. Exit status: send {source_send_finished}, recv {destination_recv_finished}"));
    }

    Ok(format!(r#"Successfully synchronized "{src_ds}" to "{dst_ds}"."#))
}

/// Returns the zfs-send process, the zfs-recv process, and (if requested) the pv process, in this order.
fn pipe_with_ratelimit(
    source_send_cmd: &mut Command,
    destination_recv_cmd: &mut Command,
    ratelimit: &Option<String>
) -> Result<(Child, Child, Option<Child>), anyhow::Error>
{
    let mut source_send_process;
    let destination_recv_process;
    let mut pv_ratelimit_option = None;
    // Pipe the sending process into the receiving process, and spawn them both.
    // It's a bit of a shame that there's no natural way (using std::process) to set up the pipes
    // before spawning any of the child processes, but oh well.
    match ratelimit{
        None => {
            source_send_process = source_send_cmd.spawn().context("Failed to spawn source-side send process.")?;
            destination_recv_cmd.stdin(source_send_process.stdout.take().unwrap());
            destination_recv_process = destination_recv_cmd.spawn().context("Failed to spawn destination-side recv process.")?;
        }
        Some(lim) => {
            let mut pv_ratelimit_cmd = std::process::Command::new("pv");
            pv_ratelimit_cmd.args(["-q", "-L", lim.as_str()])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped());
            source_send_process = source_send_cmd.spawn().context("Failed to spawn source-side send process.")?;
            pv_ratelimit_cmd.stdin(source_send_process.stdout.take().unwrap());
            let mut pv_ratelimit_process = pv_ratelimit_cmd.spawn().context("Failed to sneed.")?;
            destination_recv_cmd.stdin(pv_ratelimit_process.stdout.take().unwrap());
            pv_ratelimit_option = Some(pv_ratelimit_process);
            destination_recv_process = destination_recv_cmd.spawn().context("Failed to spawn destination-side recv process.")?;
        }
    }
    Ok((source_send_process, destination_recv_process, pv_ratelimit_option))
}