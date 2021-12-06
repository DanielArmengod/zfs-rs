#![deny(unused_must_use)]
#![allow(unused_imports, dead_code)]  // TODO: REMOVE WITH FINAL PRODUCTION CODE!

mod dataset;
mod machine;
mod replicate;
mod retention;

use std::str::FromStr;
use clap::{App, Arg};
use crate::dataset::{Dataset, parse_spec};
use crate::machine::Machine;
use crate::replicate::{*};
use crate::retention::{*};

#[allow(non_snake_case)]
#[inline(always)]
fn S(s: &str) -> String {s.to_owned()}


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

fn main() {
    let replicate = App::new("replicate")
        .about("Synchronize snapshots between two copies of the same dataset.")
        .arg(
            Arg::new("source")
                .about("Source dataset to replicate.")
                .index(1)
                .required(true)
        )
        .arg(
            Arg::new("destination")
                .about("Destination dataset into which to replicate.")
                .index(2)
                .required(true)
        )
        .arg(
            Arg::new("verbose")
                .about("Increase verbosity.")
                .short('v')
                .long("verbose")
        )
        .arg(
            Arg::new("simple-incremental")
                .about("If set, the replication stream will not include intervening snapshots; i.e. zfs send -i will be used, instead of -I. Defaults to sending all intervening snapshots between the last snapshot in common between <source> and <destination> and the last snapshot in <source>.")
                .short('i')
                .long("simple-incremental")
        )
        .arg(
            Arg::new("rollback")
                .about("Allow rolling back the destination dataset. May cause data loss if there was divergence in the destination dataset.")
                .short('F')
                .long("--rollback")
        )
        .arg(
            Arg::new("dry-run")
                .about("Do not actually receive the replication stream into <destination>.")
                .short('n')
                .long("dry-run")
        )
        .arg(
            Arg::new("take-snap-now")
                .about("Take a snapshot of the source dataset prior to sending. Optionally specify a name with --snap-name; if not, a random name will be generated.")
                .short('t')
                .long("take-snap-now")
        )
        .arg(
            Arg::new("take-snap-now-name")
                .about("The user-supplied name to use for the snapshot created by --take-snap-now.")
                .takes_value(true)
                .long("snap-name")
                .short('T')
        );

    let apply_retention = App::new("apply-retention")
        .about("Apply a retention policy to a dataset.")
        .arg(
            Arg::new("dataset")
                .about("Dataset on which to operate.")
                .index(1)
                .required(true)
        )
        .arg(
            Arg::new("no-keep-unusual")
                .about("[Pangea specific] Also considers snapshots not named \"YYYY-MM-DD\" for deletion.")
                .long("no-keep-unusual")
        )
        .arg(
            Arg::new("run-directly")
                .about("Run the zfs-destroy command directly instead of printing it for manual review.")
                .long("run-directly")
        );

    let comm = App::new("comm")
        .about("Run a comm(1)-like utility on the snapshots of two copies of the same dataset.")
        .arg(
            Arg::new("dataset_1")
                .about("First dataset.")
                .index(1)
                .required(true)
        )
        .arg(
            Arg::new("dataset_2")
                .about("Second dataset.")
                .index(2)
                .required(true)
        )
        .arg(
            Arg::new("no-collapse")
                .about("Present output in natural form, without grouping consecutive runs.")
                .long("no-collapse")
        );

    let main_parser = App::new("zfs-rs")
        .about("Toolkit for common ZFS administrative tasks.")
        .arg(Arg::new("app-verbose").about("Increases verbosity of zfs-rs itself.").short('v'))
        .subcommand(replicate)
        .subcommand(apply_retention)
        .subcommand(comm);


    let main_matches = main_parser.get_matches();
    let app_verbose = main_matches.is_present("app-verbose");

    let result = match main_matches.subcommand() {

        Some(("replicate", sub_matches)) => {
            let (mut src_machine, mut src_ds) = parse_spec(sub_matches.value_of("source").expect("No such arg.")).unwrap();
            let (mut dst_machine, mut dst_ds) = parse_spec(sub_matches.value_of("destination").expect("No such arg.")).unwrap();
            let take_snap_now: Option<String> =
                if sub_matches.is_present("take-snap-now") {
                    if sub_matches.is_present("take-snap-now-name") {
                        Some(sub_matches.value_of("take-snap-now-name").unwrap().to_owned())
                    } else {
                        Some(format!("zfs-rs-{}", get_n_random_chars(7)))
                    }
                } else {
                    None
                };
            let opts = ReplicateDatasetOpts {
                dryrun_recv: sub_matches.is_present("dry-run"),
                verbose_recv: sub_matches.is_present("verbose"),
                verbose_send: sub_matches.is_present("verbose"),
                simple_incremental: sub_matches.is_present("simple-incremental"),
                do_rollback: sub_matches.is_present("rollback"),
                take_snap_now,
                app_verbose,
            };
            match replicate_dataset(&mut src_machine, &mut src_ds, &mut dst_machine, &mut dst_ds, opts) {
                Ok(res) => res,
                Err(e) => format!("{}\n{:?}",e.to_string(), e.chain().collect::<Vec::<_>>()),
            }
        }

        Some(("apply_retention", sub_matches)) => {
            let (mut machine, mut ds) = parse_spec(sub_matches.value_of("dataset").unwrap()).unwrap();
            let opts = RetentionOpts {
                keep_unusual: !sub_matches.is_present("no-keep-unusual"),
                run_directly: sub_matches.is_present("run-directly")
            };
            match retention::apply_retention(&mut machine, &mut ds, opts) {
                Ok(res) => res,
                Err(e) => e.to_string(),
            }
        }

        Some(("comm", sub_matches)) => {
            unimplemented!()
        }

        _ => unreachable!()
    };

    println!("{}", result);
}