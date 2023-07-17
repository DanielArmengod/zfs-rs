use std::cmp::{Ordering};
use std::cmp::Ordering::{Less, Equal, Greater};
use std::str::FromStr;
use self::Comm::*;
use self::MRCUD::*;
use chrono::{Datelike, DateTime, Duration};
use chrono::offset::Utc;
use itertools::Itertools;
use regex::Regex;
use thiserror::Error;
use crate::machine::Machine;

#[cfg(test)]
use crate::machine::parse_zfs;

/// Represents a ZFS dataset
#[derive(Debug)]
pub struct Dataset {
    /// Contains the full name of a dataset, pool included, with path separators (slashes) normalized.
    /// Example: "tank/webdata".
    /// Example: "tank" (the pool root is a dataset too).
    /// Not example: "nas1:tank/webdata" (includes machine specification).
    /// Not example: "tank//lxc/webserv" (includes double-slash, which is zfs-backup-tools -specific).
    fullname: String,
    /// Points to the first slash.
    /// ```text
    /// Example: "tank/webdata"
    ///               ^pool_idx = 4
    /// ```
    pool_idx: usize,  // fullname[pool_idx] == last char of the pool
    /// Points to where the double-slash separator was, before normalization.
    /// ```text
    /// Example: "tank//lxc/webserv" - normalized to - "tank/lxc/webserv"
    ///                                                     ^relative_idx = Some(4)
    /// ```
    relative_idx: Option<usize>, // 1st '/' pool/dataset separator
    /// Snapshots must always be ordered by creation time, oldest first.
    pub snaps: Vec<Snap>,
}

/// Describes the relationship of two sets of snapshots belonging to the same datset.
/// Short for "Most Recent Common, Up-to-date, or Divergence"
#[derive(Debug)]
pub enum MRCUD<'a> {
    /// No snapshots in common.
    NoneInCommon,
    /// The most recent snapshot is also the last snapshot in both sides.
    UpToDate(&'a Snap),
    /// There is at least one snapshot in common.
    /// Both sides diverge (have further snapshots) after the most recent common snapshot.
    Divergence(&'a Snap),
    /// There is at least one snapshot in common.
    /// The destination side has more snapshots after that one.
    DestinationHasMore(&'a Snap),
    /// There is at least one snapshot in common.
    /// The source side has more snapshots after that one.
    SourceHasMore(&'a Snap),
}

/// Take two copies of the same datset, each with its own set of snapshots.
/// Find which case they fall into according to the [MRCUD] enum.
pub fn find_mrcud<'a>(source: &'a Dataset, destination: &'_ Dataset) -> MRCUD<'a> {
    use Comm::*;
    let (comm_vector, most_recent_common_idx) = source.comm(destination);
    let Some(most_recent_common_idx) = most_recent_common_idx else {
        return NoneInCommon;
    };
    let most_recent_common_snap = comm_vector[most_recent_common_idx].1;
    let most_recent_common_snap = unsafe {
        // SAFETY: Dataset::comm(&self, &other) guarantees that, for any snapshot that belongs
        // in either LEFT or BOTH, its reference will be taken from the self.snaps side.
        std::mem::transmute::<&Snap, &'a Snap>(most_recent_common_snap)
    };
    let remaining = &comm_vector[most_recent_common_idx+1..];
    let mut source_has_more = false;
    let mut destination_has_more = false;
    for (side, _) in remaining {
        match side {
            LEFT => source_has_more = true,
            RIGHT => destination_has_more = true,
            BOTH => unreachable!("There is a logic bug somewhere; we shouldn't be able to see snapshots present in both sides at this point in the code."),
        }
        if source_has_more && destination_has_more {
            // No need to keep checking, we already know that there is divergence.
            break;
        }
    }
    match (source_has_more, destination_has_more) {
        (false, false) => UpToDate(most_recent_common_snap),
        (true, false) => SourceHasMore(most_recent_common_snap),
        (false, true) => DestinationHasMore(most_recent_common_snap),
        (true, true) => Divergence(most_recent_common_snap)
    }
}


#[derive(Error, Debug)]
pub enum SpecParseError {
    #[error("{0}: a colon is only allowed at the beginning of a spec, before any slash. It is used to indicate the presence of a remote host, and is not a valid character in ZFS names.")]
    ColonAfterSlash(String),
    #[error("{0}: no characters after the machine:dataset separating colon.")]
    ZeroLengthAfterColon(String),
    #[error("{0}: a dataset spec cannot begin or end with a slash.")]
    IllegalSlashes(String),
    #[error("{0}: no characters other than ASCII alphanumeric, dash, and underscore may appear in dataset names supported by this tool.")]
    IllegalCharacters(String),
    #[error("{0}: empty dataset components (think \"zfs create testpool/////dataset\") are not allowed.")]
    EmptyComponent(String),
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Comm {
    LEFT,
    BOTH,
    RIGHT,
}

impl Dataset {
    pub fn fullname(&self) -> &str { &self.fullname }
    pub fn pool(&self) -> &str { &self.fullname[0..self.pool_idx] }
    pub fn relative(&self) -> &str {
        if let Some(idx) = self.relative_idx {
            &self.fullname[idx+1..]
        } else { "" }
    }
    pub fn is_pool_root(&self) -> bool {
        self.fullname().find('/').is_none()
    }

    /// Walk two time-ordered vectors of snapshots.
    /// Return:
    ///   * A single vector containing a reference to each unique snapshot, tagged as it appears on the left side, the right side, or both sides.
    ///   * The index within the previous vector of the last snapshot encountered that is in both collections, if any.
    /// The return vector has the same sort order as the input vectors.
    /// If the input is sorted date ascending (oldest snapshot first), then the second return value indexes the "most recent common snapshot".
    /// The input vectors are not checked for proper sort order, and the results are undefined if they are not properly sorted.
    pub fn comm<'a, 'b, 'c>(&'a self, other: &'b Self) -> (Vec<(Comm, &'c Snap)>, Option<usize>)
        where
            'a: 'c,
            'b: 'c,
    {
        let mut retval = Vec::with_capacity(self.snaps.len() + other.snaps.len());
        let mut retval2 = None;
        let mut snaps_self = self.snaps.iter().peekable();
        let mut snaps_other = other.snaps.iter().peekable();
        let (last_side, snaps_left) = loop {
            if snaps_self.peek().is_none()  { break (RIGHT, &mut snaps_other) }
            if snaps_other.peek().is_none() { break (LEFT, &mut snaps_self) }
            let snap_self = *snaps_self.peek().unwrap();
            let snap_other = *snaps_other.peek().unwrap();
            match snap_self.partial_cmp(&snap_other) {
                Some(Less) => {
                    retval.push((LEFT, snap_self));
                    snaps_self.next();
                }
                Some(Equal) => {
                    retval2 = Some(retval.len());
                    retval.push((BOTH, snap_self));
                    snaps_self.next();
                    snaps_other.next();
                }
                Some(Greater) => {
                    retval.push((RIGHT, snap_other));
                    snaps_other.next();
                }
                None => panic!("Found two snapshots which aren't comparable (i.e. l.guid != r.guid && l.creation == r.creation")
            }
        };
        for remaining in snaps_left {
            retval.push((last_side, remaining))
        }
        (retval, retval2)
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
        self.snaps.first().expect("This dataset contains no snapshots.")
    }

    pub fn newest_snap(&self) -> &Snap {
        &self.snaps.last().expect("This dataset contains no snapshots.")
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


// parse_spec defined as a free function because it uses both Machine and Dataset.
pub fn parse_spec(value: &str) -> Result<(Machine, Dataset), SpecParseError> {
    let first_colon = value.find(':');
    let first_slash = value.find('/');

    // Refer to the error message description for ZfsParseError::ColonAfterSlash
    if let (Some(cidx), Some(sidx)) = (first_colon, first_slash) {
        if cidx > sidx {
            return Err(SpecParseError::ColonAfterSlash(value.into()));
        }
    }

    let machine_spec = match first_colon {
        None => &value[0..0],
        Some(colon_idx) => &value[0..colon_idx],
    };
    let dataset_spec = match first_colon {
        None => &value[..],
        Some(colon_idx) => &value[colon_idx+1..]
    };

    if dataset_spec.len() == 0 { return Err(SpecParseError::ZeroLengthAfterColon(value.into())); }
    Ok((Machine::from_str(machine_spec)?, Dataset::from_str(dataset_spec)?))
}

#[test]
fn test_parse_spec() {
    let (m, d) = parse_spec("tank").unwrap();
    assert_eq!(m, Machine::Local);
    assert_eq!(d.fullname(), "tank");
    assert_eq!(d.relative(), "");
    assert_eq!(d.pool(), "tank");

    let (m, d) = parse_spec("baal:tank").unwrap();
    match m {  // TODO What a weird (?) way to check for equality on Machine{host: "baal".into()}... ?
        Machine::Remote {ref host } if host == "baal" => (),
        _ => panic!("Machine wasn't constructed properly!"),
    }
    assert_eq!(d.fullname(), "tank");
    assert_eq!(d.relative(), "");
    assert_eq!(d.pool(), "tank");

    let (m, d) = parse_spec(":tank").unwrap();
    assert_eq!(m, Machine::Local);
    assert_eq!(d.fullname(), "tank");
    assert_eq!(d.relative(), "");
    assert_eq!(d.pool(), "tank");

    let err = parse_spec(":tank:lareputa");
    assert!(matches!(err, Err(SpecParseError::IllegalCharacters(_))));

    let err = parse_spec(":tank:lareputa/a/path//to/a/relative/dataset");
    assert!(matches!(err, Err(SpecParseError::IllegalCharacters(_))));

    let (m, d) = parse_spec("server.company.tld:tank/a/path//to/a/relative/dataset").unwrap();
    match m {  // TODO What a weird (?) way to check for equality on Machine{host: "baal".into()}... ?
        Machine::Remote {ref host } if host == "server.company.tld" => (),
        _ => panic!("Machine wasn't constructed properly!"),
    }
    assert_eq!(d.fullname(), "tank/a/path/to/a/relative/dataset");
    assert_eq!(d.relative(), "to/a/relative/dataset");
    assert_eq!(d.pool(), "tank");

    let err = parse_spec("somehost:an_invâlid_pòól/somedataset");
    assert!(matches!(err, Err(SpecParseError::IllegalCharacters(_))));

    let err = parse_spec("somehost:but/trailing/slash/");
    assert!(matches!(err, Err(SpecParseError::IllegalSlashes(_))));
}

#[test]
fn test_append_relative() {
    let (_, d1) = parse_spec("ganon//lxc/web-ng").unwrap();
    let (_, mut d2) = parse_spec("bk:zelda").unwrap();
    d2.append_relative(&d1);
    assert_eq!(d1.relative(), "lxc/web-ng");
    assert_eq!(d2.fullname(), "zelda/lxc/web-ng");
    assert_eq!(d2.pool(), "zelda");

    let (_, d1) = parse_spec("tank/deluge").unwrap();
    let (_, mut d2) = parse_spec("baccu/deluge").unwrap();
    d2.append_relative(&d1);
    assert_eq!(d1.relative(), "");
    assert_eq!(d2.fullname(), "baccu/deluge");
    assert_eq!(d2.pool(), "baccu");
}

impl std::str::FromStr for Dataset {
    type Err = SpecParseError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        assert!(value.len() > 0, "Passed a zero-length string to Dataset::from_str!");
        for char in value.chars() {
            if ! (char.is_ascii_alphanumeric() || char == '-' || char == '_' || char == '/') {
                return Err(SpecParseError::IllegalCharacters(value.into()));
            }
        }
        let value_u8 = value.as_bytes();
        if value_u8[0] == b'/' || value_u8[value_u8.len()-1] == b'/' {
            return Err(SpecParseError::IllegalSlashes(value.into()));
        }

        // Empty dataset components (think "zfs create testpool/////dataset") are not allowed.
        // Because we want to support doubleslash notation to signal relative-path copying, we must take care to remove only 1 instance of "//" and then check for additional instances of "//" which would indicate empty path components.
        let doubleslash = value.find("//");
        let fullname = match doubleslash {
            Some(_) => value.replacen("//", "/", 1),
            None => value.to_string(),
        };
        let exists_some_empty_path_component = fullname.find("//").is_some();
        if exists_some_empty_path_component { return Err(SpecParseError::EmptyComponent(value.into())); }

        let pool_idx = fullname.find('/').unwrap_or(fullname.len());
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
pub(crate) fn build_fake_dataset(spec: &str, snaps_output_literal: &str) -> Dataset {
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
    let (comm, idx) = zelda_webdata.comm(&tank_webdata);
    let res = format!("{:#?}\n", comm);
    assert_eq!(res, include_str!("dataset/tests/test_comm.result"));
    assert_eq!(idx, Some(177));
}

#[test]
fn test_mrcud() {
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
    let none = find_mrcud(&tank_webdata, &baal_phone);
    let source_has_more = find_mrcud(&tank_webdata, &zelda_webdata);
    let divergence = find_mrcud(&tank_webdata, &zelda_webdata_divergence);

    let res = format!("{:#?}\n{:#?}\n{:#?}\n", none, source_has_more, divergence);

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


/// See the documentation in [the PartialOrd implementation](Snap::PartialOrd)
#[derive(Debug, Clone)]
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

/// For simplicity's sake, the algorithms that work on snapshots assume that no two different snapshots are ever taken on the exact same instant.
/// This PartialOrd implementation reflects that: in the unlikely case that the snapshots are not the same (l.guid != r.guid) but the creation times are equal (l.creation == r.creation), the ordering is undefined.
impl PartialOrd for Snap {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self == other {
            return Some(Equal);
        }
        return match self.creation.cmp(&other.creation) {
            Less => Some(Less),
            Greater => Some(Greater),
            Equal => None,
        }
    }
}

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

