use std::io::{BufRead, BufReader};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

/// Draw a progress bar by consuming the diagnostic output of `zfs send -vP`
/// Samples of this output are included for developer reference under /misc.
pub fn do_progressbar_from_zfs_send_stderr<R: std::io::Read>(stream: R, ) {
    // Buffer the stderr stream to take advantage of line-oriented processing.
    let mut stream = BufReader::new(stream);
    // Process headers
    // itemized_header_lines = vec![
    //     ("second", 525195304),
    //     ("third", 574823742),
    //     [...]
    // ]
    let mut itemized_header_lines = Vec::new();
    let total_size : u64 = loop {
        let mut tmpline = String::new();
        let line = stream.read_line(&mut tmpline).unwrap();
        assert_ne!(line, 0);
        tmpline.truncate(tmpline.len() - 1);  // Strip line break in-place.
        let fields = tmpline.split("\t").collect::<Vec<_>>();
        if fields[0] == "size" {
            // We've stumbled upon the final header line, which contains the total size of the
            // stream to be sent.
            break fields[1].parse().unwrap();
        }
        let to;
        let size;
        match fields[0] {
            "full" => {to = fields[1]; size = fields[2];}
            "incremental" => {to = fields[2]; size = fields[3];}
            _ => unimplemented!("Unknown form of `zfs send -vP` output.")
        }
        let _from = fields[1].to_owned();
        let to = to.split("@").last().unwrap().to_owned();
        let size : u64 = size.parse().unwrap();
        itemized_header_lines.push((to, size));
    };

    let mut cur_xfer = 0;
    let mut cur_idx = 0;
    let mut cur_snap_name = itemized_header_lines[0].0.clone();
    let mut cur_snap_bytes = itemized_header_lines[0].1;
    // let mut cur_iter = itemized_header_lines.into_iter();
    // let _ = cur_iter.next();

    let group = MultiProgress::new();
    let pb_total_items = group.add(ProgressBar::new(itemized_header_lines.len() as u64));
    let pb_total_bytes = group.add(ProgressBar::new(total_size));
    let pb_current_bytes = group.add(ProgressBar::new(cur_snap_bytes));
    pb_total_items.set_style(ProgressStyle::with_template(
        "Sending snapshot {pos} of {len}:"
    ).unwrap());
    pb_total_bytes.set_style(ProgressStyle::with_template(
        "[{elapsed_precise}] {bar:40.cyan} {bytes:>12}/{total_bytes:<12} {binary_bytes_per_sec}"
    ).unwrap().progress_chars("##-"));
    pb_current_bytes.set_style(ProgressStyle::with_template(
        "[{elapsed_precise}] {bar:40.cyan} {bytes:>12}/{total_bytes:<12} {binary_bytes_per_sec}"
    ).unwrap().progress_chars("##-"));

    for line in stream.lines() {
        let progress = line.expect("What do you mean, it wasn't UTF-8!?");
        let fields = progress.split("\t").collect::<Vec<_>>();
        let name = fields[2].split("@").last().unwrap().to_owned();
        let xfer: u64 = fields[1].parse().unwrap();
        assert_eq!(fields.len(), 3);
        // Did we move onto a new snapshot, or are we still working the previous one?
        if cur_snap_name != name {
            // see how many snapshots we've advanced (probably one, but maybe more)
            // calculate how much total_size bytes we've advanced based on that
            let delta = cur_snap_bytes - cur_xfer;  // Remainder of the snap we were last working on.
            pb_current_bytes.set_position(0);
            pb_current_bytes.reset();
            pb_total_bytes.inc(delta);
            pb_total_items.inc(1);
            cur_idx += 1;
            // Search which snap we're on now.
            // Any snap that doesn't match the name has been sent in full and must be accounted.
            while itemized_header_lines[cur_idx].0 != name {
                pb_total_bytes.inc(itemized_header_lines[cur_idx].1);
                pb_total_items.inc(1);
                cur_idx += 1;
                assert!(cur_idx < itemized_header_lines.len());
            }
            // we've found the work item we're looking for. Update state to reflect we're working
            // on this snapshot now.
            cur_snap_name = itemized_header_lines[cur_idx].0.clone();
            cur_snap_bytes = itemized_header_lines[cur_idx].1;
            // and also move the byte-counting progress bars.
            // pb_current_bytes needs to be resized to the size of the snapshot now being transferred.
            // pb_total_bytes needs to be incremented by `next_bytes_xferd`
            pb_current_bytes.set_length(cur_snap_bytes);
            pb_current_bytes.set_position(xfer);
            pb_total_bytes.inc(xfer);
            cur_xfer = xfer;
        }
        else {
            let delta = xfer - cur_xfer;
            pb_current_bytes.inc(delta);
            pb_total_bytes.inc(delta);
            pb_total_items.tick();
            cur_xfer = xfer;
        }
    }
    pb_total_items.finish();
    pb_total_bytes.finish();
    pb_current_bytes.finish();
}