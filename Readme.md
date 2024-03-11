# zfs-rs

zfs-rs is a small collection of administrative tools meant to automate certain workflows with ZFS storage pools.

Primarily, it automates the process of transferring snapshots between multiple instances of the same dataset. It was developed to automate the transfer of nightly backups on production infrastructure.

## Usage

See the [manpage](https://danielarmengod.github.io/zfs-rs/), or run with `--help`.

## Build

A Rust toolchain is required. Build is standard via `cargo`.

Manpage can be built with [ronn](https://github.com/rtomayko/ronn/tree/master).