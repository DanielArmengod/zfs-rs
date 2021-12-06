use std::cmp::{max, Ordering};
use std::str::FromStr;
use chrono::{Datelike, DateTime, Duration};
use chrono::offset::Utc;
use itertools::Itertools;
use regex::Regex;
use thiserror::Error;
use anyhow::anyhow;
use crate::machine::Machine;

#[cfg(test)]
use crate::machine::parse_zfs;

#[derive(Debug)]
pub struct Dataset {
    pub name: String,
    pub pool: String,
    pub snaps: Vec<Snap>,
}

#[derive(Debug)]
pub enum CommonOrDivergence<'a> {
    Common(&'a Snap),
    Divergence(&'a Snap),
    NoneInCommon,
}

#[derive(Error, Debug)]
#[error("{0}")]
pub struct ZfsParseError(String);

impl Dataset {
    pub fn fullname(&self) -> String {
        format!("{}/{}", self.pool, self.name)
    }
    pub fn name(&self) -> String { self.name.clone() }
    pub fn pool(&self) -> String { self.pool.clone() }

    pub fn comm<'a, 'b, 'c>(&'a self, other: &'b Self) -> Vec<(u8, &'c Snap)>
        where
            'a: 'c,
            'b: 'c,
    {
        let max_cap = max(self.snaps.len(), other.snaps.len());
        let mut retval = Vec::with_capacity(max_cap);
        let mut snaps_self = self.snaps.iter().peekable();
        let mut snaps_other = other.snaps.iter().peekable();
        let (i, snaps_left) = loop {
            if snaps_self.peek().is_none()  { break (2, &mut snaps_other) }
            if snaps_other.peek().is_none() { break (0, &mut snaps_self) }
            let snap_self = *snaps_self.peek().unwrap();
            let snap_other = *snaps_other.peek().unwrap();
            match snap_self.creation.cmp(&snap_other.creation) {
                Ordering::Less => {
                    retval.push((0, snap_self));
                    snaps_self.next();
                }
                Ordering::Equal => {
                    retval.push((1, snap_self));
                    snaps_self.next();
                    snaps_other.next();
                }
                Ordering::Greater => {
                    retval.push((2, snap_other));
                    snaps_other.next();
                }
            }
        };
        for remaining in snaps_left {
            retval.push((i, remaining))
        }
        retval
    }

    pub fn last_common_or_divergence<'a, 'b, 'c>(&'a self, other: &'b Self) -> CommonOrDivergence<'c>
        where
            'a: 'c,
            'b: 'c,
    // It is convention to have `self` be the "replication source" and `other` the "replication target".
    // Therefore, it is allowed for `self` to have additional snapshots after the last one in common
    // (it is assumed those will be "sent" to other).
    // `other`, however, must have no more snapshots after the last in common; otherwise, Divergence.
    {
        let v = self.comm(other);
        // Get the (index of the) last snapshot present in both datasets.
        let last_common = v.iter()    // Iterator over [&(1,&Snap), &(1,&Snap), &(0,&Snap), ...]
            .enumerate()                            // [(0,&(1,&Snap)), (1,&(1,&Snap)), (2,&(0,&Snap)), ...]
            .rev().find(|&(_,tup)| tup.0 == 1);
        match last_common {
            None => return CommonOrDivergence::NoneInCommon,  //TODO implicit return OK?
            Some((idx, tup)) => {
                let mut rem = (&v[idx..]).iter();
                // There can be no Snapshot only in OTHER within this slice.
                match rem.find(|&tup| tup.0 == 2) {
                    None => CommonOrDivergence::Common(tup.1),
                    Some(_) => CommonOrDivergence::Divergence(tup.1),
                }
            }
        }
    }

    fn tag_snaps_for_deletion<F>(&self, f: F) -> Vec<(bool, &Snap)>
        where
            F: Fn(&Snap) -> bool,
    // A Snap tagged with "true" is interpreted as being TO KEEP
    // TODO: Return type is probably sub-optimal because  (bool, &Snap) should fit in 9 bytes (1 for bool,
    //  8 for x64 ptr) but due to alignment constraints, Vec<(bool, &Snap)> will likely take 16bytes per elem.
    {
        // TODO: Maybe rewrite in terms of .map and .collect?
        //  But will .collect reserve capacity correctly like we do (with_capacity())?
        //  https://www.reddit.com/r/rust/comments/3spfh1/does_collect_allocate_more_than_once_while/
        let mut retval = Vec::with_capacity(self.snaps.len());
        for snap in &self.snaps {
            retval.push((f(snap), snap));
        }
        retval
    }

    pub fn oldest_snap(&self) -> &Snap {
        &self.snaps[0]
    }
    pub fn newest_snap(&self) -> &Snap {
        &self.snaps.last().unwrap()
    }
}


pub fn parse_spec(value: &str) -> Result<(Machine, Dataset), anyhow::Error> {
    let comps: Vec<&str> = value.split(':').collect();
    let (remote, rest) = match comps.len() {
        1 => (Machine::Local, comps[0]),
        2 => (Machine::Remote {host: comps[0].to_owned()}, comps[1]),
        _ => return Err(anyhow!("Too many ':' in dataset spec \"{}\".", value)),
    };
    let ds = Dataset::from_str(rest)?;

    Ok((remote, ds))
}

fn render_tagged_snaps_for_deletion(tagged_snaps: Vec<(bool, &Snap)>) -> String {
    // Returns a string of the form "2021-07-12%2021-07-17,2021-07-19%..." suitable for feeding
    // into "zfs destroy pool/dataset@<output>".
    let mut groups : Vec<Vec<&Snap>> = Vec::new();
    for (key, grouped_snap_iter) in &tagged_snaps.into_iter().group_by(|tup| tup.0) {
        if !key { groups.push(grouped_snap_iter.map(|tup| tup.1).collect()); }
    }
    groups
        .into_iter()
        .map(|group| {
            if group.len() == 1 {
                group[0].name.clone()
            }
            else {
                format!("{}%{}", group[0].name, group.last().unwrap().name)
            }})
        .join(",\\\n")
}

impl TryFrom<&str> for Dataset {
    type Error = ZfsParseError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Dataset::from_str(value)
    }
}

impl FromStr for Dataset {
    type Err = ZfsParseError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let comps: Vec<&str> = value.splitn(2, '/').collect();
        let (pool, name) = match comps.len() {
            1 => (comps[0], ""),
            2 => (comps[0], comps[1]),
            _ => unreachable!(),
        };
        // if comps.len() != 2 {
        //     return Err(ZfsParseError("There must be at least one '/' separator: {}.".to_owned()));
        // }
        let pool = pool.to_owned();
        let name = name.to_owned();

        Ok(Dataset {name, pool, snaps: Vec::new()})
    }
}

impl std::fmt::Display for Dataset {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.fullname())
    }
}


#[cfg(test)]
fn build_fake_dataset(pool: &'static str, name: &'static str, snaps_output_literal: &'static str) -> Dataset {
    let snaps = parse_zfs(snaps_output_literal);
    Dataset { pool: pool.to_string(), name: name.to_string(), snaps: snaps }
}

#[test]
fn test_render_tagged_snaps_for_deletion() {
    let zelda_webdata = build_fake_dataset(
        "zelda",
        "webdata",
        include_str!("dataset/tests/zelda_webdata-holds-and-weird-name.list")
    );
    let tagged_snaps = zelda_webdata.tag_snaps_for_deletion(basic_snap_retention_criteria);

    println!("{}", render_tagged_snaps_for_deletion(tagged_snaps));
}


#[test]
fn test_comm() -> Result<(), anyhow::Error> {
    let zelda_snaps = parse_zfs(include_str!("dataset/tests/zelda_webdata.list"));
    let zelda_webdata = Dataset { pool: "zelda".to_string(), name: "webdata".to_string(), snaps: zelda_snaps };
    let tank_snaps = parse_zfs(include_str!("dataset/tests/tank_webdata.list"));
    let tank_webdata = Dataset { pool: "tank".to_string(), name: "webdata".to_string(), snaps: tank_snaps };

    let res = zelda_webdata.comm(&tank_webdata);

    Ok(())
}

#[test]
fn test_last_common_or_divergence() {
    let zelda_snaps = parse_zfs(include_str!("dataset/tests/zelda_webdata-nocommon.list"));
    let zelda_webdata = Dataset { pool: "zelda".to_string(), name: "webdata".to_string(), snaps: zelda_snaps };
    let tank_snaps = parse_zfs(include_str!("dataset/tests/tank_webdata.list"));
    let tank_webdata = Dataset { pool: "tank".to_string(), name: "webdata".to_string(), snaps: tank_snaps };

    let res = tank_webdata.last_common_or_divergence(&zelda_webdata);

    println!("{:#?}", res);
}

fn basic_snap_retention_criteria(s: &Snap) -> bool {
    // A "true" veredict is interpreted as TO KEEP

    // Keep if taken less than 6 days ago, or taken on a Sunday and less than 6 months ago.
    let chrono_decision = matches!(s.creation.weekday(), chrono::Weekday::Sun) && (Utc::now() - s.creation) < Duration::days(180);
    // Keep if name ISN'T normal.
    let normal_name = Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap();
    let name_decision = !normal_name.is_match(&s.name);
    // Keep if there are any holds.
    let holds_decision = s.holds != 0;

    chrono_decision || name_decision || holds_decision
}

#[test]
fn test_tag_snaps_for_deletion() {
    let tank_snaps = parse_zfs(include_str!("dataset/tests/tank_webdata.list"));
    let tank_webdata = Dataset { pool: "tank".to_string(), name: "webdata".to_string(), snaps: tank_snaps };
    let zelda_snaps = parse_zfs(include_str!("dataset/tests/zelda_webdata-holds-and-weird-name.list"));
    let zelda_webdata = Dataset { pool: "zelda".to_string(), name: "webdata".to_string(), snaps: zelda_snaps };

    let res1 = tank_webdata.tag_snaps_for_deletion(basic_snap_retention_criteria);
    let res2 = zelda_webdata.tag_snaps_for_deletion(basic_snap_retention_criteria);
    println!("{:#?}", res2);
}



#[derive(Debug)]
pub struct Snap {
    pub guid: u64,
    pub name: String,  // Only the snapshot name; i.e. to the right of '@'.
    pub creation: DateTime<Utc>,
    pub holds: u32,
}

impl Default for Snap {
    fn default() -> Self {
        Snap {guid: u64::default(), name: String::default(), creation: Utc::now(), holds: u32::default() }
    }
}

impl PartialEq for Snap {
    fn eq(&self, other: &Self) -> bool {
        self.guid == other.guid
    }
}

impl Eq for Snap { }

impl std::fmt::Display for Snap {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[test]
fn snap_eq() {
    let mut s1 = Snap::default();
    let mut s2 = Snap::default();
    s1.guid = 1234;
    s2.guid = 5678;
    assert_ne!(s1, s2);
    s2.guid = 1234;
    assert_eq!(s1, s2);
    s2.name = "different".to_string();
    assert_eq!(s1, s2);
}
