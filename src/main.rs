#![deny(unused_must_use)]
// #![allow(unused_imports)]  // TODO: REMOVE WITH FINAL PRODUCTION CODE!

mod dataset;
mod machine;
mod replicate;
mod retention;
mod progressbar;
mod cutting_floor;
mod comm;

use std::process::exit;
use clap::{Command, Arg, ArgAction};
use crate::comm::CommOpts;
use crate::dataset::{parse_spec};
use crate::replicate::{*};
use crate::retention::{*};


fn get_n_random_chars(n: usize) -> String {
    use std::iter;
    use rand::{Rng, thread_rng};
    use rand::distributions::Alphanumeric;

    let mut rng = thread_rng();
    let chars: String = iter::repeat(())
        .map(|()| rng.sample(Alphanumeric))
        .map(char::from)
        .take(n)
        .collect();

    chars
}

fn verify_pv_rate(rate: &str) -> Result<(),()> {
    let suffixes = "KGMT";
    let num;
    let last = rate.chars().last().ok_or(())?;
    if last.is_numeric() {
        num = rate;
    } else {
        if suffixes.contains(last) == false {
            return Err(());
        }
        let mut t = rate.chars();
        t.next_back();
        num = t.as_str();
    }
    let parseable = num.parse::<u64>();
    return match parseable {
        Ok(_) => Ok(()),
        Err(_) => Err(())
    }
}

#[test]
fn test_verify_pv_rate() {
    assert_eq!(verify_pv_rate("1234M"), Ok(()));
    assert_eq!(verify_pv_rate("1234j"), Err(()));
    assert_eq!(verify_pv_rate("-1234M"), Err(()));
    assert_eq!(verify_pv_rate("50M"), Ok(()));
    assert_eq!(verify_pv_rate("50"), Ok(()));
}

fn main() {
    let replicate = Command::new("replicate")
        .about("Synchronize snapshots between two copies of the same dataset.")
        .arg(
            Arg::new("source")
                .help("Source dataset to replicate.")
                .required(true)
        )
        .arg(
            Arg::new("destination")
                .help("Destination dataset into which to replicate.")
                .required(true)
        )
        .arg(
            Arg::new("verbose")
                .action(ArgAction::SetTrue)
                .help("Increase verbosity and display ZFS commands as they are executed.")
                .short('v')
                .long("verbose")
        )
        .arg(
            Arg::new("simple-incremental")
                .action(ArgAction::SetTrue)
                .help(
"If set, the replication stream will not include intervening snapshots; i.e. zfs send -i will be used, instead of -I.
Defaults to sending all intervening snapshots between the last snapshot in common between <source> and <destination> and the last snapshot in <source>."
                )
                .short('i')
                .long("simple-incremental")
        )
        .arg(
            Arg::new("rollback")
                .action(ArgAction::SetTrue)
                .help("Use the rollback flag (-F) in the zfs-recv command. May cause data loss; see manual.")
                .short('F')
                .long("rollback")
        )
        .arg(
            Arg::new("allow-divergent-destination")
                .action(ArgAction::SetTrue)
                .help("Don't abort the process if zfs-rs detects the destination side diverges. May casue data loss; see manual.")
                .short('D')
                .long("allow-divergent-destination")
                .requires("rollback")
        )
        .arg(
            Arg::new("init-nonexistent-destination")
                .action(ArgAction::SetTrue)
                .help("Initialize the destination by first sending a base snapshot in full if the dataset to be synchronized does not exist in the destination.")
                .long("init")
        )
        .arg(
            Arg::new("ratelimit")
                .help("Limit the transfer rate as per `pv -L`")
                .long("ratelimit")
        )
        .arg(
            Arg::new("take-snap-now")
                .action(ArgAction::SetTrue)
                .help("Take a snapshot of the source dataset prior to sending. Optionally specify a name with --snap-name; if not, a random name will be generated.")
                .short('t')
                .long("take-snap-now")
        )
        .arg(
            Arg::new("take-snap-now-name")
                .action(ArgAction::Set)
                .help("The user-supplied name to use for the snapshot created by --take-snap-now.")
                .num_args(1)
                .long("snap-name")
                .short('T')
                .requires("take-snap-now")  //TODO the auto-generated error message isn't very friendly; maybe we can move this into custom logic, or look into embettering the default message?
        );

    let apply_retention = Command::new("apply-retention")
        .about("Apply a retention policy to a dataset.")
        .arg(
            Arg::new("dataset")
                .help("Dataset on which to operate.")
                .required(true)
        )
        .arg(
            Arg::new("no-keep-unusual")
                .help("[Pangea specific] Also considers snapshots not named \"YYYY-MM-DD\" for deletion.")
                .long("no-keep-unusual")
        )
        .arg(
            Arg::new("run-directly")
                .help("Run the zfs-destroy command directly instead of printing it for manual review.")
                .long("run-directly")
        );

    let comm = Command::new("comm")
        .about("Run a comm(1)-like utility on the snapshots of two copies of the same dataset.")
        .arg(
            Arg::new("source")
                .help("Left-hand side, or source, dataset.")
                .required(true)
        )
        .arg(
            Arg::new("destination")
                .help("Right-hand side, or destination, dataset.")
                .required(true)
        )
        .arg(
            Arg::new("collapse")
                .help("Group consecutive runs for a terser output.")
                .short('c')
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("collapse-keep-both-ends")
                .help("Group consecutive runs for a terser output; display both first and last element in each group.")
                .short('C')
                .action(ArgAction::SetTrue)
                .conflicts_with("collapse")
        )
        .arg(
            Arg::new("reverse-sort")
                .help("Display snapshots in descending chronological order (newest first).")
                .short('r')
                .action(ArgAction::SetTrue)
        );

    let mut main_parser = Command::new("zfs-rs")
        .about("Toolkit for common ZFS administrative tasks.")
        .subcommand(replicate)
        .subcommand(apply_retention)
        .subcommand(comm);

    let main_matches = main_parser.get_matches_mut();

    let result : anyhow::Result<String> = match main_matches.subcommand() {
        Some(("replicate", sub_matches)) => {
            let (mut src_machine, mut src_ds) = parse_spec(sub_matches.get_one::<String>("source").unwrap()).unwrap_or_else(|err| {
                eprintln!("Can't parse {} as a valid ZFS dataset: {}", sub_matches.get_one::<String>("source").unwrap(), err );
                exit(1);
            });
            let (mut dst_machine, mut dst_ds) = parse_spec(sub_matches.get_one::<String>("destination").unwrap()).unwrap_or_else(|err| {
                eprintln!("Can't parse {} as a valid ZFS dataset: {}", sub_matches.get_one::<String>("destination").unwrap(), err);
                exit(1);
            });
            let take_snap_now: Option<String> =
                if sub_matches.get_flag("take-snap-now") {
                    if let Some(name) = sub_matches.get_one::<String>("take-snap-now-name") {
                        Some(name.to_owned())
                    } else {
                        Some(format!("zfs-rs-{}", get_n_random_chars(7)))
                    }
                } else {
                    None
                };
            let ratelimit = sub_matches.get_one::<String>("ratelimit");
            if let Some(rate) = ratelimit {
                if let Err(_) = verify_pv_rate(rate) {
                    eprintln!("{} isn't a valid rate limit for `pv -L`. Hint: use something like `50M`.", rate);
                    exit(1);
                }
            }
            let opts = ReplicateDatasetOpts {
                app_verbose: sub_matches.get_flag("verbose"),
                simple_incremental: sub_matches.get_flag("simple-incremental"),
                use_rollback_flag_on_recv: sub_matches.get_flag("rollback"),
                allow_divergent_destination: sub_matches.get_flag("allow-divergent-destination"),
                init_nonexistent_destination: sub_matches.get_flag("init-nonexistent-destination"),
                take_snap_now,
                ratelimit: ratelimit.map(|s| s.to_owned()),
            };
            replicate_dataset_cli(&mut src_machine, &mut src_ds, &mut dst_machine, &mut dst_ds, opts)
        }

        Some(("apply-retention", sub_matches)) => {
            let (mut machine, mut ds) = parse_spec(sub_matches.get_one::<String>("dataset").unwrap()).unwrap();
            let opts = RetentionOpts {
                keep_unusual: !sub_matches.get_flag("no-keep-unusual"),
                run_directly: sub_matches.get_flag("run-directly")
            };
            retention::apply_retention(&mut machine, &mut ds, opts)
        }

        Some(("comm", sub_matches)) => {
            let (src_machine, src_ds) = parse_spec(sub_matches.get_one::<String>("source").unwrap()).unwrap_or_else(|err| {
                eprintln!("Can't parse {} as a valid ZFS dataset: {}", sub_matches.get_one::<String>("source").unwrap(), err );
                exit(1);
            });
            let (dst_machine, dst_ds) = parse_spec(sub_matches.get_one::<String>("destination").unwrap()).unwrap_or_else(|err| {
                eprintln!("Can't parse {} as a valid ZFS dataset: {}", sub_matches.get_one::<String>("destination").unwrap(), err);
                exit(1);
            });
            let opts = CommOpts {
                order_asc: !sub_matches.get_flag("reverse-sort"),
                collapse: sub_matches.get_flag("collapse"),
                collapse_keep_both_ends: sub_matches.get_flag("collapse-keep-both-ends")
            };
            comm::comm_cli(src_machine, src_ds, dst_machine, dst_ds, opts)
        }

        None => {
            main_parser.print_long_help().unwrap();
            exit(0);
        }

        _ => unreachable!()
    };

    match result {
        Ok(reason) => {
            println!("{}", reason);
            exit(0);
        },
        Err(reason) => {
            println!("{:#}", reason);

            // match reason.
            exit(1);
        }
    }
}