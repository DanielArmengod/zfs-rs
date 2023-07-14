use anyhow::Context;
use itertools::Itertools;
use crate::dataset::{Dataset, Comm::{*}};
use crate::machine::Machine;

#[derive(Default)]
pub struct CommOpts {
    pub collapse: bool,
    pub collapse_keep_both_ends: bool,
    pub order_asc: bool
}

const INDENT_WIDTH : usize = 12;

pub fn comm_cli(
    src_machine : Machine,
    mut src_ds : Dataset,
    dst_machine : Machine,
    mut dst_ds: Dataset,
    opts: CommOpts
) -> Result<String, anyhow::Error> {
    dst_ds.append_relative(&src_ds);
    src_machine.get_snaps(&mut src_ds).context(format!(r#"Unable to get snapshots for "{}""#, src_ds))?;
    dst_machine.get_snaps(&mut dst_ds).context(format!(r#"Unable to get snapshots for "{}""#, dst_ds))?;
    return do_comm(src_ds, dst_ds, opts);
}

// This function doesn't interact with its environment, so it can be called from a test harness.
// It assumes the input datasets have been populated with snapshots already.
fn do_comm(src_ds: Dataset, dst_ds: Dataset, opts: CommOpts) -> Result<String, anyhow::Error> {
    let (mut tagged, _) = src_ds.comm(&dst_ds);
    if !opts.order_asc {
        tagged.reverse();
    }
    match (opts.collapse, opts.collapse_keep_both_ends) {
        (false, false) => {
            for t in tagged {
                let (side, snap) = t;
                let indent = match side {
                    LEFT => 0,
                    BOTH => 1,
                    RIGHT => 2,
                };
                let line = format!("{space:n$}{snapname}\n", space = "", n = INDENT_WIDTH * indent, snapname = snap.name);
                print!("{}", line);
            }
        }
        (true, false) => {
            for (side, mut group) in &tagged.into_iter().group_by(|(side, _)| *side) {
                let (_, group_leader) = group.next().unwrap();
                let rest_of_group_len = group.count();
                let indent = match side {
                    LEFT => 0,
                    BOTH => 1,
                    RIGHT => 2,
                };
                println!("{space:n$}{group_leader_name}", space = "", n = indent * INDENT_WIDTH, group_leader_name = group_leader.name);
                println!("{space:n$}  (+{rest_of_group_len})", space = "", n = indent * INDENT_WIDTH);
            }
        }
        (false, true) => {
            for (side, mut group) in &tagged.into_iter().group_by(|(side, _)| *side) {
                let (_, group_leader) = group.next().unwrap();
                let last = group.enumerate().last();
                let indent = match side {
                    LEFT => 0,
                    BOTH => 1,
                    RIGHT => 2,
                };
                println!("{space:n$}{group_leader_name}", space = "", n = indent * INDENT_WIDTH, group_leader_name = group_leader.name);
                if let Some((middle_elt_cnt, (_, last_snap))) = last {
                    println!("{space:n$}  (+{group_len})", space = "", n = indent * INDENT_WIDTH, group_len = middle_elt_cnt);
                    println!("{space:n$}{group_trailer_name}", space = "", n = indent * INDENT_WIDTH, group_trailer_name = last_snap.name)
                }
            }
        }
        (true, true) => panic!()
    }
    Ok("".to_string())
}

#[test]
fn test_do_comm() {
    use crate::dataset::build_fake_dataset;
    let tank_webdata = build_fake_dataset(
        "tank/webdata",
        include_str!("dataset/tests/tank_webdata.list")
    );
    let zelda_webdata = build_fake_dataset(
        "zelda/webdata",
        include_str!("dataset/tests/zelda_webdata.list")
    );
    let mut opts = CommOpts::default();
    opts.collapse = true;
    opts.collapse_keep_both_ends = true;
    opts.order_asc = false;
    do_comm(tank_webdata, zelda_webdata, opts).unwrap();
}