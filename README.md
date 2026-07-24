# Firmware-27A-H5-Projects
STM32H563/ THUMBV8 Rust Embassy projects 

This is the location of all of the board-specific and high level operating code.

Each folder is a project corresponding to ONE binary.

**template-h5** is a project to start from when making a new project.  It contains all stubs needed.

## Setup

1. [get rustup](https://www.rust-lang.org/learn/get-started).
2. clone and open this root folder.
3. [get probe-rs](https://probe.rs/docs/getting-started/installation/).

## Commands

- To enter a project: `cd ./project-name`
- To build: `cargo build`
- To deploy onto an embedded chip locally connected, run `cargo run --release`.
- To format, run `cargo format`. 
- To lint and check stuff, run `cargo clippy`.
- To run a RTT terminal dedicated: `cargo embed --release rtt`
- To run a GDB terminal dedicated: `cargo embed --release gdb`
- To flash and leave code: `cargo embed --release`

**At this time, many commands only work consistently inside the project (via `cd`)**
The workspace's only purpose is to organize dependencies and build artificats, especially for rust-analyzer.

### Coding tips and tricks

- Use defmt macros to print stuff
- 

### IDE Stuff

There are currently custom rust-analyzer settings for VSCode and zed.  Feel free to adapt them to your own liking.


## Repository structure

This is a mono-repo configured as a normal embassy-styled project with multiple sub-projects.

Various files:

Top level `Cargo.toml` and `rust-toolchain.toml` define the various parts of the embassy project.  See comments inside for how these were structure, but most follow embassy specification.


The projects folders defines individual board-specific compilation units which inherit explicity defined `Cargo.toml` dependencies and `Embed.toml` settings, and more.  They can also depend on a crate in the firmware-rs repository via `git = "https://github.com/Northeastern-Electric-Racing/firmware-rs"` cargo dependencies.  To update them after making a change, run `cargo update`.  Use `branch = ` when developing and testing your changes.

This structure has multiple benefits, including:
- Static versioning of all embassy and other dependencies, eliminating version conflicts for in-tree code
- Inherited build settings so like-microcontroller projects share all of that boilerplate
- Shared `target` folder meaning a shared build cache for quicker and space-saving builds
- Other quirks, such as vscode `settings.json` and `config.toml`, are shared between projects


### Upgrades of Embassy/Dep versions

Updating versions is as follows

1. Update the rust-toolchain to the version found in embassy repo
2. Update all embassy versions to the versions found in the embassy repo, use x.y.z, in main Cargo.toml AND in firmware-rs Cargo.toml
3. Update all features, especially ones that say the version in them (ex. "defmt-03")
3. Update major package versions of other projects, use x.y, in main Cargo.toml
4. Update package versions of other projects, in individual Cargo.toml
4. Fix any build issues
