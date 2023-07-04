use std::fmt::Debug;
use std::io::{BufRead, BufReader};
use std::os::fd::{IntoRawFd, RawFd};
use std::thread;
use std::time::Duration;
use anyhow::{anyhow, bail, Context};
use indicatif::ProgressBar;
use nix::errno::Errno;
use nix::fcntl::{splice, SpliceFFlags};
use crate::machine::{Machine, MachineError};
use crate::dataset::{Dataset, find_mrcud};
use crate::dataset::MRCUD::*;

#[derive(Clone, Debug)]
pub struct ReplicateDatasetOpts {
    pub use_rollback_flag_on_recv: bool,
    pub allow_divergent_destination: bool,
    pub allow_nonexistent_destination: bool,
    pub simple_incremental: bool,
    pub verbose_send: bool,
    pub verbose_recv: bool,
    pub dryrun_recv: bool,
    pub app_verbose: bool,
    /// Some(snap_name) means that the user wants this program to take a snapshot.
    pub take_snap_now: Option<String>,
}

fn splice_with_progressbar(w: RawFd, r: RawFd, estimated: u64) -> Result<(), anyhow::Error> {
    use indicatif::ProgressBar;
    let pb = ProgressBar::new(estimated);
    loop {
        let data_moved = splice(w, None, r, None, 128*1024, SpliceFFlags::empty());
        match data_moved {
            Ok(0) => break,
            Ok(n) => pb.inc(n as u64),
            Err(Errno::EPIPE) => break,
            Err(e) => return Err(e).context("splice syscall failed.") // Wrap that errno into an anyhow::Error and return.
        }
    }
    pb.finish();
    Ok(())
}

/// Read from a `zfs-send -vP` stderr stream and draw a fancy progress bar.
/// Sample output to process (fields are tab-separated):
/// ```
/// full tank/old@topiso	1200354683528
/// size	1200354683528
/// 19:39:53	307603528	tank/old@topiso
/// 19:39:54	655744176	tank/old@topiso
/// [...]
/// ```
/// In the simple case where there's only a single snapshot to send, it appears that the first two
/// header lines are redundant. This probably changes in recursive or intermediary-snapshot
/// stream packages.
/// Returns Ok
fn do_progressbar_from_zfs_send_stderr<R: std::io::Read>(stream: R) -> Result<(), anyhow::Error>{
    // Buffer the stderr stream to take advantage of line-oriented processing.
    let mut stream = BufReader::new(stream);
    let mut _first_header = String::new();
    let mut second_header = String::new();
    stream.read_line(&mut _first_header)?;
    stream.read_line(&mut second_header)?;
    let size : u64 = second_header
        .trim_end().split("\t").nth(1).unwrap().parse().expect("Failed to parse zfs send size.");
    let pb = ProgressBar::new(size);
    for line in stream.lines() {
        let line = line.expect("What do you mean, it wasn't UTF-8!?");
        let xfer_so_far : u64 = line
            .split("\t").nth(1).unwrap().parse().expect("Failed to parse zfs xfer'd-so-far.");
        pb.set_position(xfer_so_far);
    }
    pb.finish();
    thread::sleep(Duration::from_secs(1));
    Ok(())
}

pub fn replicate_dataset(
    src_machine : &mut Machine,
    src_ds : &mut Dataset,
    dst_machine : &mut Machine,
    dst_ds: &mut Dataset,
    opts: ReplicateDatasetOpts,
) -> Result<String, anyhow::Error> {
    dst_ds.append_relative(src_ds);

    if let Some(snap_name) = opts.take_snap_now {
        eprintln!(r#"Taking snapshot "{}:{}@{}" (requested by --take-snap-now)."#, src_machine, src_ds.fullname(), snap_name);
        src_machine.create_snap_with_name(src_ds, &snap_name).context("Failed to take snapshot (requested by --take-snap-now).")?;
    }
    // TODO: The first thing we do in 'replicate' is to take the src snapshot if requested by -t
    //  If subsequent steps fail for whatever reason and the uses retries 'replicate' after fixing the underlying causes,
    //  then 'replicate' will fail because the -t snapshot will already exist.
    //  SOLUTION: Add logic so that if "Failed to take snapshot" happens, 'replicate' checks whether it was because
    //  such snapshot already existed, and, check that it is less than 1 day old or whatever; if so proceed with 'replicate'.

    src_machine.get_snaps(src_ds).context(format!(r#"Unable to get snapshots for "{}""#, src_ds))?;  // No handling it if this fails.
    let dst_dataset_existed = match dst_machine.get_snaps(dst_ds) {
        Ok(_) => true,
        Err(MachineError::NoDataset) => false,
        Err(e) => return Err(e).context(format!(r#"Unable to get snapshots for "{}"."#, dst_ds))
    };
    if opts.app_verbose {
        eprintln!(r#"There are {} snapshot(s) in "{}:{}"."#, src_ds.snaps.len(), src_machine, src_ds.fullname());
        if dst_dataset_existed {
            eprintln!(r#"There are {} snapshot(s) in "{}:{}"."#, dst_ds.snaps.len(), dst_machine, dst_ds.fullname());
        } else {
            eprintln!(r#"Dataset "{}" not found in "{}"; continuing."#, dst_ds.fullname(), dst_machine);
        }
    }

    if !dst_dataset_existed && !opts.allow_nonexistent_destination {
        return Err(anyhow!(r#"Dataset "{}" does not exist in host "{}" and full send (--init-empty) not requested."#, dst_ds.fullname(), dst_machine));
    }
    if !dst_dataset_existed && opts.allow_nonexistent_destination {
        // TODO do full send. The following commented-out block of code was lifted out of the old version.

        //     eprintln!("Will begin with a full-send of {}.", most_recent_common_snap.name);
        //     if opts.app_verbose {
        //         eprintln!("Ensuring the destination dataset's ancestors exist.");
        //     }
        //     dst_machine.create_ancestors(dst_ds).context(format!("Failed to create {}:{}'s ancestors!", dst_machine, dst_ds.fullname()))?;
        //     let sendside_full = src_machine.fullsend_s(&src_ds, src_ds.oldest_snap(), opts.verbose_send);
        //     let recvside_full = dst_machine.recv(&dst_ds, opts.do_rollback, opts.dryrun_recv, opts.verbose_recv);
        //     let pipeline = sendside_full | recvside_full;
        //     let tmp_cmdline_string = format!("{:?}", pipeline);
        //     if opts.app_verbose {
        //         eprintln!("{}", tmp_cmdline_string);
        //     }
        //     let fullsend_result = pipeline.join();
        //     match fullsend_result {
        //         Ok(statuscode) => if !statuscode.success() { bail!("The commandline {:?} spawned successfully but exited with an error.", tmp_cmdline_string) },
        //         Err(e) => return Err(e).context(format!("Failed to spawn commandline {}.", tmp_cmdline_string))
        //     }
        //     if opts.app_verbose {
        //         eprintln!("Full-send of {} successful.", most_recent_common_snap.name);
        //     }
        // }
        // TODO retry dst_machine.get_snaps(dst_ds)
    }

    let mrcud = find_mrcud(&src_ds, &dst_ds);
    // Check for reasons to bail early.
    match mrcud {
        NoneInCommon => return Err(anyhow!(r#"Datasets "{}:{}" and "{}:{}" have no snapshots in common."#, src_machine, src_ds.fullname(), dst_machine, dst_ds.fullname())),
        UpToDate(mrc) => return Ok(format!(r#"Nothing to do: datasets "{}:{}" and "{}:{}" are already up-to-date at snapshot "{}"."#, src_machine, src_ds.fullname(), dst_machine, dst_ds.fullname(), mrc)),
        DestinationHasMore(mrc) => return Err(anyhow!(
            r#"Source dataset "{src_machine}:{srcds}"'s most recent snapshot, "{mrc}", is also found in destination dataset "{dst_machine}:{dstds}", but there are additional, newer snapshots at the destination.\n\
            Hint: perhaps you meant to send from "{dst_machine}:{dstds}" to "{src_machine}:{srcds}"?"#, srcds = src_ds.fullname(), dstds = dst_ds.fullname())),
        Divergence(mrc) => {
            if !opts.use_rollback_flag_on_recv {
                return Err(anyhow!(r#"Datasets "{}:{}" and "{}:{}" diverge after "{mrc}". Please inspect both datasets and, if appropriate, retry with --allow-divergent-destination."#, src_machine, src_ds.fullname(), dst_machine, dst_ds.fullname()))
            }
        }
        SourceHasMore(_) => ()
    }

    let most_recent_common_snap = match mrcud {
        Divergence(s) | SourceHasMore(s) => s,
        _ => unreachable!()
    };

    if opts.app_verbose {  // TODO: use log::info!() or something instead of checking conditionals.
        eprintln!(r#"Figured out "{}" as the most recent common snapshot."#, most_recent_common_snap.name);
    }

    if opts.app_verbose {
        eprintln!(r#"Now doing incremental send from "{}" to "{}"."#, most_recent_common_snap.name, src_ds.newest_snap());
    }

    let mut source_send_cmd = src_machine.send_from_s_till_newest(&src_ds, most_recent_common_snap, opts.simple_incremental, opts.verbose_send);
    let mut destination_recv_cmd = dst_machine.recv(&dst_ds, opts.use_rollback_flag_on_recv, opts.dryrun_recv, opts.verbose_recv);

    // Pipe the sending process into the receiving process, and spawn them both.
    // It's a bit of a shame that there's no natural way (in Rust) to set up the pipes before
    // spawning any of the child processes, but oh well.
    let mut source_send_process = source_send_cmd.spawn().context("Failed to spawn source-side send process.")?;
    destination_recv_cmd.stdin(source_send_process.stdout.take().unwrap());
    let mut destination_recv_process = destination_recv_cmd.spawn().context("Failed to spawn destination-side recv process.")?;

    // At this point the transfer process is underway and we're not involved in moving data.
    // We do have to draw a progress bar. To do so take the standard error stream from the
    // sending process, where we find a header with the estimated amount of data to send as well
    // as periodic updates of progress.


    do_progressbar_from_zfs_send_stderr(source_send_process.stderr.take().unwrap())
        .context("There was a problem with drawing the progress bar.")?;

    let source_send_finished = source_send_process.wait().unwrap();
    let destination_recv_finished = destination_recv_process.wait().unwrap();

    if !source_send_finished.success() {
        eprintln!("There was a problem with the zfs-send process. Exit status: {source_send_finished}");
    }
    if !destination_recv_finished.success() {
        eprintln!("There was a problem with the zfs-recv process. Exit status: {destination_recv_finished}");
    }

    Ok(format!(r#"Successfully synchronized "{}" to "{}"."#, src_ds, dst_ds))
}