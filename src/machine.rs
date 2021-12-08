use std::str::FromStr;
use anyhow::Context;
use crate::dataset::{Dataset, Snap};
use subprocess::{Exec, Redirection};
use chrono::offset::Utc;
use chrono::TimeZone;
use thiserror::Error;
use crate::S;


#[derive(Error,Debug)]
pub enum MachineError {
    #[error("No such dataset.")]
    NoDataset,
    #[error("Invalid character in snapshot name.")]
    InvalidCharacter,
    #[error("The name is already in use.")]
    NameAlreadyInUse,
    #[error("{0}")]
    Other(String),
    #[error(transparent)]
    Boxed(#[from] anyhow::Error),
}

#[derive(Debug)]
pub enum Machine {
    Local,
    Remote {
        host: String,
        // Maybe add <user> field here, for credentials?
    }
}

impl Machine {
    pub fn _get_datasets(&self) -> Vec<()> {todo!()}

    pub fn get_snaps(&self, dataset: &mut Dataset) -> Result<(), MachineError> {
        // Populates the &mut Dataset.snaps with data fetched from the Machine.
        // Returns () if successful (even though .snaps might be an empty vector).
        // May return NoDataset as an Error if something went wrong
        let mut cmd = format!("zfs list -Hp -o name,creation,guid,userrefs -t snapshot -d1 {}", dataset.fullname());
        if let Machine::Remote{ host} = self {
                cmd = format!("ssh {} -- '{}'", host, cmd);
        }
        // TODO: Use .communicate() instead of .capture() to support timeout settings.
        let subproc = Exec::shell(cmd)
            .stdout(Redirection::Pipe)
            .stderr(Redirection::Pipe)
            .capture().context("Failed to spawn the command.")?;
        if !subproc.exit_status.success() {
            return if subproc.stderr_str().ends_with("dataset does not exist\n") {
                Err(MachineError::NoDataset)
            } else {
                Err(MachineError::Other(subproc.stderr_str()))
            }
        }
        let stdout = subproc.stdout_str();
        dataset.snaps = parse_zfs(&stdout);

        Ok(())
    }

    pub fn send_from_s_till_last(&self, ds: &Dataset, s: &Snap, simple_incremental: bool, verbose: bool) -> Exec {
        assert_ne!(ds.newest_snap(), s);  // It is an error to do zfs send -i @today tank/foobar@today.
        let i = if simple_incremental {"i"} else {"I"};
        let verbose = if verbose {"v"} else {""};
        let src_snap = &s.name;
        let ds_name = ds.fullname();
        let dst_snap = &ds.snaps.last().unwrap().name;
        let mut cmd = format!("zfs send -cp{v}Le{i} @{src_snap} {ds_name}@{dst_snap}", i=i, v=verbose, src_snap=src_snap, ds_name=ds_name, dst_snap=dst_snap);
        if let Machine::Remote {host} = self {
            cmd = format!("ssh {} -- '{}'", host, cmd);
        }

        Exec::shell(cmd)
    }

    pub fn fullsend_s(&self, ds: &Dataset, s: &Snap, verbose: bool) -> Exec {
        let verbose = if verbose {"v"} else {""};
        let snap = &s.name;
        let ds_name = ds.fullname();
        let mut cmd = format!("zfs send -cp{v}Le {ds_name}@{snap}", v=verbose, snap=snap, ds_name=ds_name);
        if let Machine::Remote {host} = self {
            cmd = format!("ssh {} -- '{}'", host, cmd);
        }

        Exec::shell(cmd)
    }

    pub fn recv_into_pool(&self, ds: &Dataset, rollback: bool, dryrun: bool, verbose: bool) -> Exec {
        let dryrun = if dryrun {"-n"} else {""};
        let rollback = if rollback {"-F"} else {""};
        let verbose = if verbose {"-v"} else {""};
        let dst_pool = ds.pool();
        let mut cmd = format!("zfs recv {rollback} {dryrun} {verbose} -d {dst_pool}", dryrun=dryrun, rollback=rollback, verbose=verbose, dst_pool=dst_pool);
        if let Machine::Remote {host} = self {
            cmd = format!("ssh {} -- '{}'", host, cmd);
        }

        Exec::shell(cmd)
    }

    pub fn create_snap(&self, ds: &Dataset, name: &str) -> Result<(), MachineError> {
        let mut cmd = format!("zfs snapshot {}@{}", ds.fullname(), name);
        if let Machine::Remote {host} = self {
            cmd = format!("ssh {} -- '{}'", host, cmd);
        }

        // TODO: Use .communicate() instead of .capture() to support timeout settings.
        let subproc = Exec::shell(cmd)
            .stdout(Redirection::Pipe)
            .stderr(Redirection::Pipe)
            .capture().context("Failed to spawn the command.")?;
        if !subproc.exit_status.success() {
            return if subproc.stderr_str().contains("invalid character") {
                Err(MachineError::InvalidCharacter)
            } else if subproc.stderr_str().contains("dataset does not exist") {
                Err(MachineError::NoDataset)
            } else if subproc.stderr_str().contains("dataset already exists") {
                Err(MachineError::NameAlreadyInUse)
            } else {
                Err(MachineError::Other(subproc.stderr_str()))
            }
        }

        Ok(())
    }
}

impl std::fmt::Display for Machine {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Machine::Local => write!(f, "localhost"),
            Machine::Remote {host} => write!(f, "{}", host),
        }
    }
}


pub fn parse_zfs(output: &str) -> Vec<Snap> {
    // Parses "zfs list -Hp -o name,creation,guid,userrefs -t snapshot -d1 <dataset>" output.

    // Preallocate a Vec. We'll need to hold exactly as many elements as lines are present in the file.
    let numlines = output.matches('\n').count();
    let mut retval = Vec::with_capacity(numlines);

    for line in output.lines() {
        let mut splitted = line.split('\t');
        let name = splitted.next().unwrap().split('@').nth(1).unwrap().to_string();
        let creation = Utc.timestamp(splitted.next().unwrap().parse().unwrap(), 0);
        let guid : u64 = splitted.next().unwrap().parse().unwrap();
        let holds : u32 = splitted.next().unwrap().parse().unwrap();
        retval.push(Snap {name, creation, guid, holds});
    }

    assert_eq!(numlines, retval.capacity());
    assert_eq!(numlines, retval.len());

    retval
}

#[test]
fn test_parse_zfs() {
    let output = include_str!("dataset/tests/baal_tank_phone.list");
    println!("{:?}", parse_zfs(output));
}

#[test]
fn test_remotes() -> Result<(), anyhow::Error>{
    // let ml = Machine::Local;
    let mr = Machine::Remote { host: "baal".to_string() };
    let mut d = Dataset::from_str("tank/deluge")?;
    // ml.get_snaps(&mut d);
    mr.get_snaps(&mut d)?;
    println!("{:#?}", d);
    Ok(())
}
