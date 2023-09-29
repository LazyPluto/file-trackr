use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::TimeZone;
use clap::Parser;
use colored::{Color, ColoredString, Colorize};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DirSnapshot {
	size: u64,
	files: Vec<(String, FSEntrySnapshot)>,
}

impl DirSnapshot {
	fn get_file_snapshot(&self, filename: &String) -> Option<&FSEntrySnapshot> {
		self.files
			.iter()
			.find_map(|(name, snapshot)| Some(snapshot).filter(|_| *name == *filename))
	}

	fn depth(&self) -> u32 {
		self.files
			.iter()
			.map(|(_, snapshot)| 1 + snapshot.depth())
			.max()
			.unwrap_or(0)
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileSnapshot {
	size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum FSEntrySnapshot {
	File(FileSnapshot),
	Directory(DirSnapshot),
}

impl FSEntrySnapshot {
	pub fn size(&self) -> u64 {
		match self {
			Self::File(snapshot) => snapshot.size,
			Self::Directory(snapshot) => snapshot.size,
		}
	}

	pub fn depth(&self) -> u32 {
		match self {
			Self::File(_) => 0,
			Self::Directory(snapshot) => snapshot.depth(),
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FSSnapshot {
	root: FSEntrySnapshot,
	path: String,
	depth: u32,
	time: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FSTrack {
	snapshots: Vec<FSSnapshot>,
	depth: u32,
}

impl FSTrack {
	pub fn new(depth: u32) -> Self {
		Self {
			snapshots: Vec::new(),
			depth,
		}
	}

	pub fn add_snapshot(&mut self, snapshot: FSSnapshot) {
		// Ensure that the snapshot that we're adding is newer than snapshots we have.
		assert!(self
			.snapshots
			.last()
			.map(|previous| previous.time <= snapshot.time)
			.unwrap_or(true));
		self.snapshots.push(snapshot);
	}

	pub fn latest_snapshot(&self) -> Option<&FSSnapshot> {
		self.snapshots.last()
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FSTrackDB {
	tracked_paths: HashMap<PathBuf, FSTrack>,
	snapshot_depth: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackPathErr {
	PathAlreadyTracked,
	PathDoesntExist,
}

impl FSTrackDB {
	pub fn new() -> Self {
		Self {
			tracked_paths: HashMap::new(),
			snapshot_depth: 3,
		}
	}

	pub fn track_path(&mut self, path: &Path, depth: Option<u32>) -> Result<(), TrackPathErr> {
		if self.tracked_paths.contains_key(path) {
			return Err(TrackPathErr::PathAlreadyTracked);
		}

		if !path.exists() {
			return Err(TrackPathErr::PathDoesntExist);
		}

		self.tracked_paths.insert(
			path.to_owned(),
			FSTrack::new(depth.unwrap_or(self.snapshot_depth)),
		);

		Ok(())
	}

	pub fn get_track(&self, path: &Path) -> Option<&FSTrack> {
		self.tracked_paths.get(path)
	}

	pub fn get_track_mut(&mut self, path: &Path) -> Option<&mut FSTrack> {
		self.tracked_paths.get_mut(path)
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SizeDiff {
	old_size: u64,
	new_size: u64,
}

impl SizeDiff {
	pub fn compute(old: u64, new: u64) -> Option<Self> {
		if old != new {
			Some(Self {
				old_size: old,
				new_size: new,
			})
		} else {
			None
		}
	}
}

struct FileSnapshotDiff {
	size_diff: Option<SizeDiff>,
}

impl FileSnapshotDiff {
	pub fn compute(old: &FileSnapshot, new: &FileSnapshot) -> Self {
		Self {
			size_diff: SizeDiff::compute(old.size, new.size),
		}
	}

	pub fn has_changes(&self) -> bool {
		self.size_diff.is_some()
	}
}

struct DirSnapshotDiff {
	size_diff: Option<SizeDiff>,
	file_diffs: Vec<(String, FSEntrySnapshotDiff)>,
	new_files: Vec<(String, FSEntrySnapshot)>,
	deleted_files: Vec<(String, FSEntrySnapshot)>,
}

impl DirSnapshotDiff {
	pub fn compute(old: &DirSnapshot, new: &DirSnapshot) -> Self {
		let mut result = DirSnapshotDiff {
			size_diff: SizeDiff::compute(old.size, new.size),
			file_diffs: Vec::new(),
			new_files: Vec::new(),
			deleted_files: Vec::new(),
		};

		for (name_a, snapshot_a) in &old.files {
			if let Some(snapshot_b) = new.get_file_snapshot(name_a) {
				let snapshot_diff = FSEntrySnapshotDiff::compute(snapshot_a, snapshot_b);
				if snapshot_diff.has_changes() {
					result.file_diffs.push((name_a.clone(), snapshot_diff));
				}
			} else {
				result
					.deleted_files
					.push((name_a.clone(), snapshot_a.clone()));
			}
		}

		for (name, snapshot) in &new.files {
			if old.get_file_snapshot(name).is_none() {
				result.new_files.push((name.clone(), snapshot.clone()));
			}
		}

		result
	}

	pub fn has_changes(&self) -> bool {
		self.size_diff.is_some()
			|| self.new_files.len() != 0
			|| self.deleted_files.len() != 0
			|| self.file_diffs.len() != 0
	}

	pub fn depth(&self) -> u32 {
		let file_diffs_depth = self
			.file_diffs
			.iter()
			.map(|(_, diff)| 1 + diff.depth())
			.max()
			.unwrap_or(0);

		let new_files_depth = self
			.new_files
			.iter()
			.map(|(_, snapshot)| 1 + snapshot.depth())
			.max()
			.unwrap_or(0);

		let deleted_files_depth = self
			.deleted_files
			.iter()
			.map(|(_, snapshot)| 1 + snapshot.depth())
			.max()
			.unwrap_or(0);

		file_diffs_depth
			.max(new_files_depth)
			.max(deleted_files_depth)
	}
}

enum FSEntrySnapshotDiff {
	File(FileSnapshotDiff),
	Directory(DirSnapshotDiff),
}

impl FSEntrySnapshotDiff {
	pub fn compute(old: &FSEntrySnapshot, new: &FSEntrySnapshot) -> Self {
		match (&old, &new) {
			(FSEntrySnapshot::File(old), FSEntrySnapshot::File(new)) => {
				Self::File(FileSnapshotDiff::compute(old, new))
			}
			(FSEntrySnapshot::Directory(old), FSEntrySnapshot::Directory(new)) => {
				Self::Directory(DirSnapshotDiff::compute(old, new))
			}
			_ => panic!(),
		}
	}

	pub fn size_diff(&self) -> Option<SizeDiff> {
		match self {
			Self::File(file_diff) => file_diff.size_diff,
			Self::Directory(dir_diff) => dir_diff.size_diff,
		}
	}

	pub fn has_changes(&self) -> bool {
		match self {
			Self::File(file_diff) => file_diff.has_changes(),
			Self::Directory(dir_diff) => dir_diff.has_changes(),
		}
	}

	pub fn depth(&self) -> u32 {
		match self {
			Self::File(_) => 0,
			Self::Directory(dir_diff) => dir_diff.depth(),
		}
	}

	#[allow(dead_code)]
	pub fn is_file(&self) -> bool {
		match self {
			Self::File(_) => true,
			_ => false,
		}
	}

	#[allow(dead_code)]
	pub fn is_dir(&self) -> bool {
		match self {
			Self::Directory(_) => true,
			_ => false,
		}
	}
}

fn take_snapshot(path: &Path, depth: u32) -> io::Result<FSEntrySnapshot> {
	fn helper(path: &Path, depth: u32) -> io::Result<FSEntrySnapshot> {
		if depth == 0 {
			let size = dir_size(path)?;
			let snapshot = DirSnapshot {
				size,
				files: Vec::new(),
			};
			return Ok(FSEntrySnapshot::Directory(snapshot));
		}

		if depth == 1 {
			println!("{:?}", path);
		}

		let mut snapshot = DirSnapshot {
			size: 0,
			files: Vec::new(),
		};
		for entry in fs::read_dir(path)? {
			let file = entry?;
			let file_snapshot = match file.metadata()? {
				data if data.is_dir() => helper(&file.path(), depth - 1).ok(),
				data if data.is_file() => {
					let snapshot = FileSnapshot { size: data.len() };
					Some(FSEntrySnapshot::File(snapshot))
				}
				_ => None,
			};

			if let Some(file_snapshot) = file_snapshot {
				snapshot.size += file_snapshot.size();
				snapshot.files.push((
					file.file_name().as_os_str().to_str().unwrap().to_owned(),
					file_snapshot,
				));
			}
		}

		Ok(FSEntrySnapshot::Directory(snapshot))
	}

	helper(path, depth)
}

fn to_human_readable_size(size: u64) -> String {
	const SIZE_TYPES: [&str; 4] = ["B", "KiB", "MiB", "GiB"];

	if size < 1024 {
		return format!("{} B", size);
	}

	let mut size = size as f64;
	let mut idx = 0;
	while size > 1024.0 && idx < SIZE_TYPES.len() {
		size /= 1024.0;
		idx += 1;
	}

	format!("{:.2} {}", size, SIZE_TYPES[idx])
}

fn dir_size(path: impl Into<PathBuf>) -> io::Result<u64> {
	fn dir_size(mut dir: fs::ReadDir) -> io::Result<u64> {
		dir.try_fold(0, |acc, file| {
			let file = file?;
			let size = match file.metadata()? {
				data if data.is_dir() => dir_size(std::fs::read_dir(file.path())?)?,
				data if data.is_file() => data.len(),
				_ => 0,
			};
			Ok(acc + size)
		})
	}

	dir_size(std::fs::read_dir(path.into())?)
}

fn scan_file_sizes(path: impl Into<PathBuf>) {
	let path = path.into();
	let mut total_size = 0;
	for file in std::fs::read_dir(path).unwrap() {
		match file {
			Ok(file) => {
				let file_type = file.file_type().unwrap();
				let size = if file_type.is_dir() {
					dir_size(file.path())
				} else {
					file.metadata().map(|metadata| metadata.len())
				};
				total_size += size.as_ref().unwrap_or(&0);
				let file_name = file.file_name();
				let file_name_str = file_name
					.as_os_str()
					.to_str()
					.unwrap_or("Failed to convert path to string");
				let file_size_str = size
					.map(to_human_readable_size)
					.unwrap_or_else(|err| format!("{:?}", err));
				println!("{}: {}", file_name_str, file_size_str);
			}
			Err(error) => {
				println!("Failed to fetch file: {}", error);
			}
		}
	}

	println!("Total file size: {}", to_human_readable_size(total_size));
}

fn is_recent_file(metadata: &fs::Metadata) -> bool {
	let time = metadata.created().unwrap();
	time.elapsed().unwrap().as_secs() < 60 * 60 * 4
}

fn collect_recent_files(path: impl Into<PathBuf>, filepaths: &mut Vec<PathBuf>) -> io::Result<()> {
	fn collect_recent_files(dir: fs::ReadDir, filepaths: &mut Vec<PathBuf>) -> io::Result<()> {
		for file in dir {
			let file = file?;
			let metadata = file.metadata()?;
			match metadata {
				data if data.is_dir() => {
					collect_recent_files(std::fs::read_dir(file.path())?, filepaths)?
				}
				data if data.is_file() => {
					if is_recent_file(&data) {
						filepaths.push(file.path())
					}
				}
				_ => {}
			}
		}
		Ok(())
	}

	collect_recent_files(std::fs::read_dir(path.into())?, filepaths)
}

fn scan_file_dates(path: impl Into<PathBuf>) {
	let mut recent_files = Vec::new();
	collect_recent_files(path, &mut recent_files).unwrap();
	let mut total_recent_size = 0;
	for path in recent_files {
		let file_size = fs::metadata(&path).unwrap().len();
		let file_size_str = to_human_readable_size(file_size);
		total_recent_size += file_size;
		println!(
			"{}: {}",
			path.as_os_str()
				.to_str()
				.unwrap_or("Failed to convert path to string"),
			file_size_str
		);
	}
	println!(
		"Total file size: {}",
		to_human_readable_size(total_recent_size)
	);
}

fn main2() {
	let args = std::env::args().collect::<Vec<String>>();

	if args.len() < 3 {
		println!("Usage: {} <path> (size|date)", args[0]);
		return;
	}

	match args[2].to_lowercase().as_str() {
		"size" => scan_file_sizes(args[1].clone()),
		"date" => scan_file_dates(args[1].clone()),
		_ => println!("Usage: <path> {} (size|date)", args[0]),
	}
}

fn print_indent(indent: u32) {
	for _ in 0..indent {
		print!("    ");
	}
}

fn print_fs_entry_snapshot(
	name: &str,
	snapshot: &FSEntrySnapshot,
	indent: u32,
	max_depth: u32,
	colorize: &impl Fn(&str) -> ColoredString,
) {
	print_indent(indent);
	match snapshot {
		FSEntrySnapshot::File(snapshot) => {
			let colored_str = colorize(&format!(
				"{}: {}",
				name,
				to_human_readable_size(snapshot.size)
			));
			println!("{colored_str}");
		}
		FSEntrySnapshot::Directory(snapshot) => {
			let colored_str = colorize(&format!(
				"{}: {}",
				name,
				to_human_readable_size(snapshot.size)
			));
			println!("{colored_str}");
			if max_depth != 0 {
				for (name, file_snapshot) in &snapshot.files {
					print_fs_entry_snapshot(
						&name,
						&file_snapshot,
						indent + 1,
						max_depth - 1,
						colorize,
					);
				}
			}
		}
	}
}

#[derive(Debug)]
struct Context {
	track_db: FSTrackDB,
	track_db_path: PathBuf,
	should_stop: bool,
	commands: Vec<(clap::Command, fn(clap::ArgMatches, &mut Self))>,
}

fn handle_add(args: clap::ArgMatches, context: &mut Context) {
	let path = args.get_one::<String>("path").expect("Must be present");
	let depth = args.get_one("depth").copied();

	let Ok(path) = dunce::canonicalize(PathBuf::from(path)) else {
		println!("Invalid path: '{path}'");
		return;
	};

	println!(
		"Adding {} to the database",
		path.as_os_str().to_str().unwrap()
	);
	context.track_db.track_path(&path, depth).unwrap();
}

fn handle_list(args: clap::ArgMatches, context: &mut Context) {
	match args.subcommand().unwrap() {
		("tracks", _) => {
			let track_db = &mut context.track_db;
			if track_db.tracked_paths.len() == 0 {
				println!("No tracked paths");
			} else {
				println!("Tracked paths: ");
				for (path, track) in &track_db.tracked_paths {
					println!(
						"    {}: {}",
						path.as_os_str().to_str().unwrap(),
						track.depth
					);
				}
			}
		}
		("snapshots", matches) => {
			let path = matches.get_one::<String>("path").unwrap();
			let Ok(path) = dunce::canonicalize(PathBuf::from(path)) else {
				println!("Invalid path");
				return;
			};

			let Some(track) = context.track_db.get_track(&path) else {
				println!("{} not tracked", path.to_string_lossy());
				return;
			};

			if track.snapshots.len() == 0 {
				println!("No snapshots taken");
				return;
			}

			for (i, snapshot) in track.snapshots.iter().enumerate() {
				let date_time = chrono::Local
					.timestamp_opt(snapshot.time as i64, 0)
					.unwrap();
				let date_time_str = date_time.format("%a %d %b %Y %H:%M:%S");
				println!("{i}: {date_time_str}");
			}
		}
		_ => unreachable!(),
	}
}

fn handle_show(args: clap::ArgMatches, context: &mut Context) {
	let path = args.get_one::<String>("path").expect("Must be present");
	let max_depth = args.get_one::<u32>("depth").copied().unwrap_or(u32::MAX);
	let snapshot_id = args.get_one::<u32>("id").copied();

	let Ok(path) = dunce::canonicalize(PathBuf::from(path)) else {
		println!("Invalid path");
		return;
	};

	let Some(track) = context.track_db.get_track(&path) else {
		println!("Path not tracked");
		return;
	};

	let snapshot = if let Some(id) = snapshot_id {
		let Some(snapshot) = track.snapshots.get(id as usize) else {
			println!("Snapshot {id} does not exist");
			return;
		};
		snapshot
	} else {
		let Some(snapshot) = track.latest_snapshot() else {
			println!("No snapshots taken");
			return;
		};
		snapshot
	};

	print_fs_entry_snapshot(&snapshot.path, &snapshot.root, 0, max_depth, &|x| x.normal());
}

fn handle_snap(args: clap::ArgMatches, context: &mut Context) {
	let path = args.get_one::<String>("path").expect("Must be present");
	let depth = args.get_one::<u32>("depth").copied();

	let mut paths_to_snap = Vec::new();
	if path == "*" {
		paths_to_snap = context
			.track_db
			.tracked_paths
			.iter()
			.map(|(path, _)| path)
			.cloned()
			.collect();
	} else {
		if let Ok(path) = dunce::canonicalize(PathBuf::from(path)) {
			paths_to_snap.push(path);
		} else {
			println!("Invalid path");
			return;
		}
	}

	if paths_to_snap.len() == 0 {
		println!("No paths are tracked");
		return;
	}

	let time = SystemTime::now()
		.duration_since(SystemTime::UNIX_EPOCH)
		.unwrap()
		.as_secs();
	for path in paths_to_snap {
		let Some(track) = context.track_db.get_track_mut(&path) else {
			println!("{} not tracked", path.to_string_lossy());
			return;
		};

		println!("Snapping {}", path.to_string_lossy());

		let depth = depth.unwrap_or(track.depth);
		let Ok(snapshot) = take_snapshot(&path, depth) else {
			println!("Failed to take snapshot of {}", path.to_string_lossy());
			return;
		};

		track.add_snapshot(FSSnapshot {
			root: snapshot,
			path: path.to_str().unwrap().to_owned(),
			depth,
			time,
		});
	}
}

fn handle_compare(args: clap::ArgMatches, context: &mut Context) {
	let path = args.get_one::<String>("path").expect("Must be present");
	let depth = args.get_one::<u32>("depth").copied().unwrap_or(u32::MAX);
	let snapshot1_id = args.get_one::<u32>("a").copied();
	let snapshot2_id = args.get_one::<u32>("b").copied();

	if snapshot1_id.is_some() != snapshot2_id.is_some() {
		println!("Both snapshot IDs are required for comparison");
		return;
	}

	let Ok(path) = dunce::canonicalize(PathBuf::from(path)) else {
		println!("Invalid path");
		return;
	};

	let Some(track) = context.track_db.get_track(&path) else {
		println!("Path not tracked");
		return;
	};

	let (old, new) = if let Some((id1, id2)) = snapshot1_id.zip(snapshot2_id) {
		let Some(old) = track.snapshots.get(id1 as usize) else {
			println!("Snapshot {id1} does not exist");
			return;
		};
		let Some(new) = track.snapshots.get(id2 as usize) else {
			println!("Snapshot {id2} does not exist");
			return;
		};
		(old, new)
	} else {
		if track.snapshots.len() < 2 {
			println!("Atleast 2 snapshots are required for comparison");
			return;
		}
		(
			&track.snapshots[track.snapshots.len() - 2],
			track.snapshots.last().unwrap(),
		)
	};

	fn show_diff_helper(
		name: &str,
		diff: &FSEntrySnapshotDiff,
		indent: u32,
		max_depth: u32,
	) -> bool {
		print_indent(indent);
		if let Some(size_diff) = diff.size_diff() {
			let diff = size_diff.new_size.abs_diff(size_diff.old_size);
			let diff_str = to_human_readable_size(diff);
			let diff_sign = if size_diff.old_size < size_diff.new_size {
				'+'
			} else {
				'-'
			};
			let old_size_str = to_human_readable_size(size_diff.old_size);
			let new_size_str = to_human_readable_size(size_diff.new_size);
			let color = if size_diff.new_size < size_diff.old_size {
				Color::Green
			} else {
				Color::Red
			};
			let colored_str =
				format!("{name}: {old_size_str} -> {new_size_str}: {diff_sign}{diff_str}")
					.color(color);
			println!("{colored_str}");
		} else {
			let colored_str = format!("{name}").yellow();
			println!("{colored_str}");
		}

		if max_depth == 0 {
			return false;
		}

		if let FSEntrySnapshotDiff::Directory(dir_diff) = diff {
			let mut add_newline = false;
			if dir_diff.file_diffs.len() != 0 {
				let mut first = true;
				let mut previous_was_dir = false;
				for (name, snapshot_diff) in &dir_diff.file_diffs {
					// We want empty dirs to look like files in the output (i.e. no surrounding newlines).
					// So, we use SnapshotDiff::depth() instead of is_dir() to see if it actually has contents.
					let dir_like = snapshot_diff.depth() != 0;

					if (previous_was_dir || dir_like) && !first && max_depth != 1 {
						println!();
					}
					first = false;
					previous_was_dir = dir_like;

					show_diff_helper(name, snapshot_diff, indent + 1, max_depth - 1);
				}
				add_newline = true;
			}

			if dir_diff.new_files.len() != 0 {
				if add_newline {
					println!();
				}

				print_indent(indent + 1);
				println!("{}", "New files:".bright_red().bold());
				for (name, snapshot) in &dir_diff.new_files {
					print_fs_entry_snapshot(name, snapshot, indent + 2, max_depth - 1, &|x| {
						x.bright_red().bold()
					});
				}
				add_newline = true;
			}

			if dir_diff.deleted_files.len() != 0 {
				if add_newline {
					println!();
				}

				print_indent(indent + 1);
				println!("{}", "Deleted files:".bright_green().bold());
				for (name, snapshot) in &dir_diff.deleted_files {
					print_fs_entry_snapshot(name, snapshot, indent + 2, max_depth - 1, &|x| {
						x.bright_green().bold()
					});
				}
				add_newline = true;
			}

			add_newline
		} else {
			false
		}
	}

	let diff = FSEntrySnapshotDiff::compute(&old.root, &new.root);
	if diff.has_changes() {
		show_diff_helper(&old.path, &diff, 0, depth.min(diff.depth()));
	} else {
		println!("No changes");
	}
}

fn handle_help(args: clap::ArgMatches, context: &mut Context) {
	let command = args.get_one::<String>("command").and_then(|command_name| {
		context
			.commands
			.iter()
			.map(|(command, _)| command)
			.find(|command| command.get_name() == command_name)
			.cloned()
	});

	if let Some(mut command) = command {
		command
			.print_long_help()
			.expect("Failed to print help due to IO error");
	} else {
		// HACK: Add all commands as subcommands of a tmp command.
		//       Use help_template() to render only the subcommands.
		//       This gives us a well formatted and colorized help message.
		let mut tmp_command = clap::Command::new("tmp")
			// Disable the auto generated help subcommand as we have our own help.
			.disable_help_subcommand(true)
			.subcommands(context.commands.iter().map(|(command, _)| command).cloned())
			.help_template("Commands:\n{subcommands}\n\nRun 'help <command>' for help about a specific command");

		tmp_command
			.print_long_help()
			.expect("Failed to print help due to IO error");
	}
}

fn handle_exit(_args: clap::ArgMatches, context: &mut Context) {
	let track_db_string = serde_json::to_string(&context.track_db).unwrap();
	fs::write(&context.track_db_path, track_db_string).unwrap();
	context.should_stop = true;
}

fn handle_command(command: &str, context: &mut Context) {
	let parts = command.split_ascii_whitespace().collect::<Vec<_>>();
	if parts.len() < 1 {
		return;
	}

	let command = context
		.commands
		.iter()
		.find(|(command, _)| {
			command.get_name() == parts[0] || command.get_all_aliases().any(|name| name == parts[0])
		})
		.cloned();
	if let Some((command, command_fn)) = command {
		match command.try_get_matches_from(parts) {
			Ok(matches) => command_fn(matches, context),
			Err(err) => err.print().expect("Failed to print error due to IO error"),
		}
	} else {
		println!("Command '{}' not found", parts[0]);
	}
}

#[derive(Debug, Parser)]
struct Args {
	#[arg(name = "path", help = "Path to TrackDB")]
	path: PathBuf,
}

fn main() {
	let args = Args::parse();
	let track_db_path = args.path;
	let track_db = if track_db_path.exists() {
		let track_db_string = String::from_utf8(fs::read(&track_db_path).unwrap()).unwrap();
		serde_json::from_str(&track_db_string).unwrap()
	} else {
		FSTrackDB::new()
	};

	let mut commands: Vec<(clap::Command, fn(clap::ArgMatches, &mut Context))> = Vec::new();

	let add_command = clap::Command::new("add")
		.arg(clap::arg!(<path> "Path to track"))
		.arg(
			clap::arg!(-d --depth <depth> "Default depth of snapshots of this path")
				.value_parser(clap::value_parser!(u32)),
		)
		.about("Adds <path> to the database of tracked paths");
	commands.push((add_command, handle_add));

	let list_command = clap::Command::new("list")
		.subcommand(clap::Command::new("tracks").about("Lists tracked paths"))
		.subcommand(
			clap::Command::new("snapshots")
				.arg(clap::arg!(<path> "Path whose snapshots must be listed"))
				.about("Lists snapshots of <path>"),
		)
		.subcommand_required(true)
		.about("Lists tracks/snapshots");
	commands.push((list_command, handle_list));

	let show_command = clap::Command::new("show")
		.arg(clap::arg!(<path> "Path whose snapshot must be shown"))
		.arg(
			clap::arg!([id] "ID of the snapshot which must be shown")
				.value_parser(clap::value_parser!(u32)),
		)
		.arg(
			clap::arg!(-d --depth <depth> "Maximum depth for displaying the snapshot")
				.value_parser(clap::value_parser!(u32)),
		)
		.about("Shows the newest snapshot of <path>");
	commands.push((show_command, handle_show));

	let snap_command = clap::Command::new("snap")
		.arg(clap::arg!(<path> "Path whose snapshot must be taken"))
		.arg(
			clap::arg!(-d --depth <depth> "Maximum depth of snapshot")
				.value_parser(clap::value_parser!(u32)),
		)
		.about("Takes a snapshot of <path>");
	commands.push((snap_command, handle_snap));

	let compare_command = clap::Command::new("compare")
		.arg(clap::arg!(<path> "Path whose snapshots must be compared"))
		.arg(clap::arg!([a] "ID of first snapshot").value_parser(clap::value_parser!(u32)))
		.arg(clap::arg!([b] "ID of second snapshot").value_parser(clap::value_parser!(u32)))
		.arg(
			clap::arg!(-d --depth <depth> "Maximum depth for displaying the diff")
				.value_parser(clap::value_parser!(u32)),
		)
		.about("Compares the 2 newest snapshots of <path>");
	commands.push((compare_command, handle_compare));

	let help_command = clap::Command::new("help")
		.arg(clap::arg!([command] "Command whose help must be displayed"))
		.about("Displays the help menu");
	commands.push((help_command, handle_help));

	let exit_command = clap::Command::new("exit")
		.visible_alias("quit")
		.about("Exits the program");
	commands.push((exit_command, handle_exit));

	let mut context = Context {
		track_db,
		track_db_path,
		should_stop: false,
		commands,
	};
	while !context.should_stop {
		print!(">> ");
		io::stdout().flush().unwrap();

		let mut input = String::new();
		io::stdin().read_line(&mut input).unwrap();

		handle_command(input.trim(), &mut context);
		println!();
	}
}
