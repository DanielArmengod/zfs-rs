use std::str::FromStr;
use std::{io};
use std::process::{Command, Output, Stdio};
use crate::dataset::{Dataset, Snap, SpecParseError};
use chrono::offset::Utc;
use chrono::TimeZone;
use thiserror::Error;


#[derive(Error,Debug)]
pub enum MachineError {
    #[error("No such dataset.")]
    NoDataset,
    #[error("Invalid character in snapshot name.")]
    IllegalZFSName,
    #[error("The name is already in use.")]
    NameAlreadyInUse,
    #[error("ZFS administrative commands not in PATH. Hint: is ZFS installed in the target machine, and are you root there?")]
    NoZFSRuntime,
    #[error("Failed to spawn command: {0}")]
    SubprocessError(#[from] io::Error),
    #[error("Unknown ZFS command execution error: {0}")]
    ZFSCommandExecutionError(String),
}

#[derive(Debug, PartialEq)]
pub enum Machine {
    Local,
    Remote {
        host: String,
        // Maybe add <user> field here, for credentials?
    }
}

impl FromStr for Machine {
    type Err = SpecParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.len() {
            0 => Machine::Local,
            _ => Machine::Remote { host: s.to_string() }  // TODO: Check that the string slice `s` passed in is a valid host name
        })
    }
}

trait OutputExt {
    fn stdout_str(&self) -> String;
    fn stderr_str(&self) -> String;
}

impl OutputExt for Output {
    fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
    fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}


/// Previous versions of this program follow the pattern of building a shell command line to invoke ZFS commands.
/// I wanted to switch to building the exec(2) syscall itself, to protect against shell injection attacks and generally separate data from code.
/// Unfortunately sshd always invokes a shell on the remote side. See https://unix.stackexchange.com/q/205567/
/// So whatever; in a future version of this program I'll could go with environment variables and quoted shell expansion, for untrusted user input. Idk.
impl Machine {
    /// Prepends `ssh {machine.user}@{machine.host} -- ` to `command` if `self` is a remote host.
    /// Prepends `sh -c ` to `command` if `self` is the local host.
    fn prepare_cmd(&self, command: &str) -> Command {
        let mut cmd : Command;
        match self {
            Machine::Local => {
                cmd = Command::new("sh");
                cmd.arg("-c");
            }
            Machine::Remote { host } => {
                cmd = Command::new("ssh");
                cmd
                    //.arg(format!("{user}@{host}"))
                    .arg(format!("{host}"))
                    .arg("--");
            }
        };
        cmd.arg(command);
        return cmd;
    }

    /// Populates `dataset.snaps` with data fetched from the Machine.
    pub fn get_snaps(&self, dataset: &mut Dataset) -> Result<(), MachineError> {
        let mut cmd= self.prepare_cmd(&format!(
            "zfs list -Hp -o name,creation,guid,userrefs -t snapshot -d1 {}", dataset.fullname()
        ));
        let result = cmd.output()?;   // TODO <- timeout
        if !result.status.success() {
            return if result.stderr.ends_with(b"dataset does not exist\n") {
                Err(MachineError::NoDataset)
            } else if result.stderr.starts_with(b"sh: ") {
                Err(MachineError::NoZFSRuntime)
            }
            else {
                Err(MachineError::ZFSCommandExecutionError(result.stderr_str()))
            }
        }
        dataset.snaps = parse_zfs(&result.stdout_str());

        Ok(())
    }

    pub fn send_from_s_till_newest(&self, ds: &Dataset, s: &Snap, simple_incremental: bool) -> Command {
        assert_ne!(ds.newest_snap(), s);  // It is an error to do zfs send -i @today tank/foobar@today.
        let i = if simple_incremental {"i"} else {"I"};
        let src_snap = &s.name;
        let ds_name = ds.fullname();
        let dst_snap = &ds.snaps.last().unwrap().name;
        let mut cmd = self.prepare_cmd(&format!(
            "zfs send -vP -cpLe{i} @{src_snap} {ds_name}@{dst_snap}", i=i, src_snap=src_snap, ds_name=ds_name, dst_snap=dst_snap
        ));
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped());
        return cmd;
    }

    pub fn fullsend_s(&self, ds: &Dataset, s: &Snap) -> Command {
        let snap = &s.name;
        let ds_name = ds.fullname();
        let mut cmd = self.prepare_cmd(&format!(
            "zfs send -vP -cpLe {ds_name}@{snap}", snap=snap, ds_name=ds_name
        ));
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped());
        return cmd;
    }

    pub fn recv(&self, ds: &Dataset, rollback: bool) -> Command {
        let rollback = if rollback {"-F"} else {""};
        let dst = ds.fullname();
        let mut cmd = self.prepare_cmd(&format!(
            "zfs recv -s {rollback} {dst}", rollback=rollback, dst=dst
        ));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        return cmd;
    }

    pub fn create_snap_with_name(&self, ds: &mut Dataset, name: &str) -> Result<(), MachineError> {
        let mut cmd = self.prepare_cmd(&format!(
            "zfs snapshot {}@{}", ds.fullname(), name
        ));
        let result = cmd.output()?; // TODO: timeout

        if !result.status.success() {
            return if result.stderr_str().contains("invalid character") {
                Err(MachineError::IllegalZFSName)
            } else if result.stderr_str().contains("dataset does not exist") {
                Err(MachineError::NoDataset)
            } else if result.stderr_str().contains("dataset already exists") {
                Err(MachineError::NameAlreadyInUse)
            } else {
                Err(MachineError::ZFSCommandExecutionError(result.stderr_str()))
            }
        }
        self.get_snaps(ds)?;
        Ok(())
    }

    /// Panics if `ds.is_pool_root()` is true.
    pub fn create_ancestors(&self, ds: &Dataset) -> Result<(), MachineError> {
        let fullname = ds.fullname();
        let idx = fullname.rfind('/').unwrap();
        let dirname = &fullname[..idx];
        let mut cmd= self.prepare_cmd(&format!(
            "zfs create -p {}", dirname
        ));
        let result = cmd.output()?;   // TODO <- timeout
        if !result.status.success() {
           return Err(MachineError::ZFSCommandExecutionError(result.stderr_str()));
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
        let creation = Utc.timestamp_opt(splitted.next().unwrap().parse().unwrap(), 0).unwrap();
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
    let res = format!("{:#?}", parse_zfs(include_str!("dataset/tests/baal_tank_phone.list")));
    assert!(res == include_str!("dataset/tests/test_parse_zfs.result"));
}

#[test]
#[ignore]
fn test_remotes() -> Result<(), MachineError>{
    //TODO This functionality interacts with the environment and should probably not be tested here.

    let (m, mut d) = crate::parse_spec("baal:tank/deluge").unwrap();
    m.get_snaps(&mut d)?;
    println!("{:#?}", d);
    Ok(())
}

#[test]
#[ignore]
fn test_local() -> Result<(), MachineError>{
    //TODO This functionality interacts with the environment and should probably not be tested here.

    let (m, mut d) = crate::parse_spec("tank/deluge").unwrap();
    let res = m.get_snaps(&mut d);
    println!("{}", res.as_ref().unwrap_err());
    res?;
    Ok(())
}
