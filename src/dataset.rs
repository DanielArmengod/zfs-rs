use std::cmp::{max, Ordering};
use std::str::FromStr;
use chrono::{Datelike, DateTime, Duration};
use chrono::offset::Utc;
use itertools::Itertools;
use regex::Regex;
use thiserror::Error;
use crate::machine::Machine;

#[cfg(test)]
use crate::machine::parse_zfs;
use crate::S;

#[derive(Debug)]
pub struct Dataset {
    fullname: String,
    pool_idx: usize,  // fullname[pool_idx] == last char of the pool
    relative_idx: Option<usize>, // 1st '/' pool/dataset separator
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
    pub fn fullname(&self) -> &str { &self.fullname }
    pub fn pool(&self) -> &str { &self.fullname[0..self.pool_idx] }
    pub fn relative(&self) -> &str {
        if let Some(idx) = self.relative_idx {
            &self.fullname[idx+1..]
        } else { "" }
    }

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

    pub fn last_common_or_divergence<'a, 'b>(&'a self, other: &'b Self) -> CommonOrDivergence<'a>
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
                unsafe {
                    match rem.find(|&tup| tup.0 == 2) {
                        None => CommonOrDivergence::Common(std::mem::transmute::<&'_ Snap, &'a Snap>(tup.1)),
                        Some(_) => CommonOrDivergence::Divergence(std::mem::transmute::<&'_ Snap, &'a Snap>(tup.1)),
                    }
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

    pub fn append_relative(&mut self, other: &Self) {
        if other.relative() != "" {
            self.fullname.push('/');
            self.fullname.push_str(other.relative());
        }
    }
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

// impl TryFrom<&str> for Dataset<'_> {
//     type Error = ZfsParseError;
//     fn try_from(value: &str) -> Result<Self, Self::Error> {
//         Dataset::from_str(value)
//     }
// }

pub fn parse_spec(value: &str) -> Result<(Machine, Dataset), ZfsParseError> {
    if !value.is_ascii() {
        return Err(ZfsParseError(format!("{}: this string is not valid ASCII and thus cannot be parsed as a ZFS name.", value)));
    }
    let value_u8 = value.as_bytes();
    if value_u8[0] == b'/' || value_u8[value_u8.len()-1] == b'/' {
        return Err(ZfsParseError(format!("{}: a ZFS name cannot begin or end with a slash.", value)));
    }

    let mut colon = value.find(':');
    let slash = value.find('/');

    //  Commented out because the lines below are a neater replacement.
    // colon = match (colon, slash) {
    //     (Some(cidx), _) if cidx == 0 => colon,
    //     (Some(cidx), None) => colon,
    //     (Some(cidx), Some(sidx)) => {
    //         if cidx < sidx { colon } else { None }
    //     },
    //     (None, _) => None,
    // };

    if let (Some(cidx), Some(sidx)) = (colon, slash) {
        if cidx > sidx {
            colon = None;
        }
    }

    let machine_spec = match colon {
        None => &value[0..0],
        Some(colon_idx) => &value[0..colon_idx],
    };
    let dataset_spec = match colon {
        None => &value[..],
        Some(colon_idx) => &value[colon_idx+1..]
    };

    Ok((Machine::from_str(machine_spec)?, Dataset::from_str(dataset_spec)?))
}

#[test]
fn test_parse_spec() {
    let (m, d) = parse_spec("tank").unwrap();
    assert_eq!(m, Machine::Local);
    assert_eq!(d.fullname, "tank");
    assert_eq!(d.relative(), "");
    assert_eq!(d.pool(), "tank");

    let (m, d) = parse_spec("baal:tank").unwrap();
    match m {
        Machine::Remote {ref host } if host == "baal" => (),
        _ => panic!("Machine wasn't constructed properly!"),
    }
    assert_eq!(d.fullname, "tank");
    assert_eq!(d.relative(), "");
    assert_eq!(d.pool(), "tank");

    let (m, d) = parse_spec(":tank").unwrap();
    assert_eq!(m, Machine::Local);
    assert_eq!(d.fullname, "tank");
    assert_eq!(d.relative(), "");
    assert_eq!(d.pool(), "tank");

    let (m, d) = parse_spec(":tank:lareputa").unwrap();
    assert_eq!(m, Machine::Local);
    assert_eq!(d.fullname, "tank:lareputa");
    assert_eq!(d.relative(), "");
    assert_eq!(d.pool(), "tank:lareputa");

    let (m, d) = parse_spec(":tank:lareputa/a/path/to/dataset").unwrap();
    assert_eq!(m, Machine::Local);
    assert_eq!(d.fullname, "tank:lareputa/a/path/to/dataset");
    assert_eq!(d.relative(), "");
    assert_eq!(d.pool(), "tank:lareputa");

    let (m, d) = parse_spec(":tank:lareputa/a/path//to/a/relative/dataset").unwrap();
    assert_eq!(m, Machine::Local);
    assert_eq!(d.fullname, "tank:lareputa/a/path/to/a/relative/dataset");
    assert_eq!(d.relative(), "to/a/relative/dataset");
    assert_eq!(d.pool(), "tank:lareputa");

    let (m, d) = parse_spec("server.company.tld:tank:lareputa/a/path//to/a/relative/dataset").unwrap();
    match m {
        Machine::Remote {ref host } if host == "server.company.tld" => (),
        _ => panic!("Machine wasn't constructed properly!"),
    }    assert_eq!(d.fullname, "tank:lareputa/a/path/to/a/relative/dataset");
    assert_eq!(d.relative(), "to/a/relative/dataset");
    assert_eq!(d.pool(), "tank:lareputa");

    let r = parse_spec("somehost:an_invâlid_pòól/somedataset");
    assert!(r.is_err());

    let r = parse_spec("somehost:but/trailing/slash/");
    assert!(r.is_err());
}

impl FromStr for Dataset {
    type Err = ZfsParseError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let doubleslash = value.find("//");
        let fullname = match doubleslash {
            Some(_) => value.replace("//", "/"),
            None => value.to_string(),
        };
        let pool_idx : usize = fullname.find('/').unwrap_or(fullname.len());
        let relative_idx = doubleslash;

        Ok(Dataset { fullname, snaps: Vec::new(), pool_idx, relative_idx })
    }
}


impl std::fmt::Display for Dataset {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.fullname())
    }
}


#[cfg(test)]
fn build_fake_dataset(spec: &str, snaps_output_literal: &str) -> Dataset {
    let mut ds = Dataset::from_str(spec).unwrap();
    let snaps = parse_zfs(snaps_output_literal);
    ds.snaps = snaps;

    ds
}

#[test]
fn test_render_tagged_snaps_for_deletion() {
    fn retention_criteria(s: &Snap) -> bool {
        let when = "2021-12-08T10:01:58Z".parse::<DateTime<Utc>>().unwrap();
        __basic_snap_retention_criteria(s, when)
    }
    let zelda_webdata = build_fake_dataset(
        "zelda/webdata",
        include_str!("dataset/tests/zelda_webdata-holds-and-weird-name.list")
    );
    let tagged_snaps = zelda_webdata.tag_snaps_for_deletion(retention_criteria);

    let res = render_tagged_snaps_for_deletion(tagged_snaps);
    println!("zfs destroy -v zelda/webdata@\\\n{}", res);
    assert_eq!(res, include_str!("dataset/tests/test_render_tagged_snaps_for_deletion.result"));
}


#[test]
fn test_comm() {
    let zelda_webdata = build_fake_dataset(
        "zelda/webdata",
        include_str!("dataset/tests/zelda_webdata.list")
    );
    let tank_webdata = build_fake_dataset(
        "tank/webdata",
        include_str!("dataset/tests/tank_webdata.list")
    );
    let comm = zelda_webdata.comm(&tank_webdata);
    let res = format!("{:#?}\n", comm);
    assert_eq!(res, include_str!("dataset/tests/test_comm.result"));
}

#[test]
fn test_last_common_or_divergence() {
    let tank_webdata = build_fake_dataset(
        "tank/webdata",
        include_str!("dataset/tests/tank_webdata.list")
    );
    let zelda_webdata = build_fake_dataset(
        "zelda/webdata",
        include_str!("dataset/tests/zelda_webdata.list")
    );
    let zelda_webdata_divergence = build_fake_dataset(
        "zelda/webdata",
        include_str!("dataset/tests/zelda_webdata-divergence.list")
    );
    let baal_phone = build_fake_dataset(
        "zelda/webdata",
        include_str!("dataset/tests/baal_tank_phone.list")
    );
    let none = tank_webdata.last_common_or_divergence(&baal_phone);
    let common = tank_webdata.last_common_or_divergence(&zelda_webdata);
    let divergence = tank_webdata.last_common_or_divergence(&zelda_webdata_divergence);

    let res = format!("{:#?}\n{:#?}\n{:#?}\n", none, common, divergence);

    assert_eq!(res, include_str!("dataset/tests/test_last_common_or_divergence.result"));
}

fn __basic_snap_retention_criteria(s: &Snap, when: DateTime<Utc>) -> bool {
    // A "true" veredict is interpreted as TO KEEP

    // Keep if taken less than 6 days ago, or taken on a Sunday and less than 6 months ago.
    let chrono_decision = matches!(s.creation.weekday(), chrono::Weekday::Sun) && (when - s.creation) < Duration::days(180);
    // Keep if name ISN'T normal.
    let normal_name = Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap();
    let name_decision = !normal_name.is_match(&s.name);
    // Keep if there are any holds.
    let holds_decision = s.holds != 0;

    chrono_decision || name_decision || holds_decision
}

fn basic_snap_retention_criteria(s: &Snap) -> bool {
    let when = Utc::now();
    __basic_snap_retention_criteria(s, when)
}

#[test]
fn test_tag_snaps_for_deletion() {
    fn retention_criteria(s: &Snap) -> bool {
        let when = "2021-12-08T10:01:58Z".parse::<DateTime<Utc>>().unwrap();
        __basic_snap_retention_criteria(s, when)
    }
    let zelda_webdata = build_fake_dataset(
        "zelda/webdata",
        include_str!("dataset/tests/zelda_webdata-holds-and-weird-name.list")
    );
    let tagged = zelda_webdata.tag_snaps_for_deletion(retention_criteria);
    let res = format!("{:#?}\n", tagged);

    assert_eq!(res, include_str!("dataset/tests/test_tag_snaps_for_deletion.result"));
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

