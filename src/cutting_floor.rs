#![cfg(never)]

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