use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::SystemTime;

use chrono::TimeZone;
use clap::Parser;
use colored::{Color, ColoredString, Colorize};

mod model;

use model::{
	DirSnapshot, FSEntrySnapshot, FSEntrySnapshotDiff, FSSnapshot, FSTrackDB, FileSnapshot,
	PathPattern, SnapshotRule, SnapshotRuleSet,
};

enum SnapshotWorkerMessage {
	Trace(PathBuf),
	Error(String),
}

pub struct SnapshotTaker {
	worker_thread: JoinHandle<io::Result<FSEntrySnapshot>>,
	worker_rx: mpsc::Receiver<SnapshotWorkerMessage>,
}

impl SnapshotTaker {
	pub fn new(path: &Path, ruleset: SnapshotRuleSet) -> Self {
		let (mut tx, rx) = mpsc::channel();
		let path = path.to_owned();
		let worker_thread = thread::spawn(move || Self::take_snapshot(&path, &ruleset, &mut tx));
		Self {
			worker_thread,
			worker_rx: rx,
		}
	}

	fn wait(self) -> io::Result<FSEntrySnapshot> {
		while let Ok(message) = self.worker_rx.recv() {
			match message {
				SnapshotWorkerMessage::Trace(path) => println!("{}", path.display()),
				SnapshotWorkerMessage::Error(error) => {
					println!("{}", format!("ERROR: {error}").red())
				}
			}
		}

		self.worker_thread.join().expect("Worker thread crashed!")
	}

	fn take_snapshot(
		path: &Path,
		ruleset: &SnapshotRuleSet,
		tx: &mut mpsc::Sender<SnapshotWorkerMessage>,
	) -> io::Result<FSEntrySnapshot> {
		fn helper(
			root: &Path,
			path: &Path,
			ruleset: &SnapshotRuleSet,
			tx: &mut mpsc::Sender<SnapshotWorkerMessage>,
		) -> io::Result<FSEntrySnapshot> {
			let mut snapshot = DirSnapshot {
				size: 0,
				files: Vec::new(),
			};
			for entry in fs::read_dir(path)? {
				let file = entry?;
				let path = file.path();
				let metadata = file.metadata()?;

				let sub_path = path.strip_prefix(root).unwrap();
				let Some(depth) = ruleset.query(sub_path) else {
					if metadata.is_dir() {
						snapshot.size += dir_size(path)?;
					} else if metadata.is_file() {
						snapshot.size += metadata.len();
					}
					continue;
				};

				if depth == 1 {
					tx.send(SnapshotWorkerMessage::Trace(path.to_owned()))
						.expect("Receiver was destroyed!");
				}

				let file_snapshot = match metadata {
					data if data.is_dir() => match helper(root, &path, ruleset, tx) {
						Ok(snapshot) => Some(snapshot),
						Err(error) => {
							let msg = format!("Failed to snap {}: {}", path.display(), error);
							tx.send(SnapshotWorkerMessage::Error(msg))
								.expect("Receiver was destroyed!");
							None
						}
					},
					data if data.is_file() => {
						let snapshot = FileSnapshot { size: data.len() };
						Some(FSEntrySnapshot::File(snapshot))
					}
					_ => None,
				};

				if let Some(file_snapshot) = file_snapshot {
					snapshot.size += file_snapshot.size();
					let file_name = file.file_name();
					let file_name: &Path = file_name.as_ref();
					let file_name = format!("{}", file_name.display());
					snapshot.files.push((file_name, file_snapshot));
				}
			}

			Ok(FSEntrySnapshot::Directory(snapshot))
		}

		helper(path, path, ruleset, tx)
	}
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
				let mut sorted_files: Vec<_> = snapshot.files.iter().by_ref().collect();
				sorted_files.sort_by_key(|(_, file)| file.size());
				sorted_files.reverse();

				for (name, file_snapshot) in sorted_files {
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
			if track_db.tracked_paths().len() == 0 {
				println!("No tracked paths");
			} else {
				println!("Tracked paths: ");
				for (path, track) in track_db.tracked_paths() {
					println!(
						"    {}: {}",
						path.as_os_str().to_str().unwrap(),
						track.depth()
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

			if track.count_snapshots() == 0 {
				println!("No snapshots taken");
				return;
			}

			for (i, snapshot) in track.snapshots().iter().enumerate() {
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
		let Some(snapshot) = track.get_snapshot(id) else {
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

	print_fs_entry_snapshot(&snapshot.path, &snapshot.root, 0, max_depth, &|x| {
		x.normal()
	});
}

fn handle_snap(args: clap::ArgMatches, context: &mut Context) {
	let path = args.get_one::<String>("path").expect("Must be present");
	let depth = args.get_one::<u32>("depth").copied();

	let mut paths_to_snap = Vec::new();
	if path == "*" {
		paths_to_snap = context
			.track_db
			.tracked_paths()
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

		let depth = depth.unwrap_or(track.depth());
		let mut ruleset = SnapshotRuleSet::new();
		// The root is captured to `depth`, so its children are captured to `depth - 1`
		ruleset.add_rule(PathPattern::Wildcard, SnapshotRule::Single(depth - 1));
		let snapshot_taker = SnapshotTaker::new(&path, ruleset);
		let Ok(snapshot) = snapshot_taker.wait() else {
			println!("Failed to take snapshot of {}", path.to_string_lossy());
			return;
		};

		track.add_snapshot(FSSnapshot {
			root: snapshot,
			path: format!("{}", path.display()),
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
		let Some(old) = track.get_snapshot(id1) else {
			println!("Snapshot {id1} does not exist");
			return;
		};
		let Some(new) = track.get_snapshot(id2) else {
			println!("Snapshot {id2} does not exist");
			return;
		};
		(old, new)
	} else {
		if track.count_snapshots() < 2 {
			println!("Atleast 2 snapshots are required for comparison");
			return;
		}
		let snapshots = track.snapshots();
		(&snapshots[snapshots.len() - 2], snapshots.last().unwrap())
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
				let mut sorted_diffs: Vec<_> = dir_diff.file_diffs.iter().by_ref().collect();
				sorted_diffs.sort_by_key(|&(_, diff)| {
					diff.size_diff()
						.map(|size_diff| size_diff.new_size as i64 - size_diff.old_size as i64)
						.unwrap_or(0)
				});
				sorted_diffs.reverse();

				let mut first = true;
				let mut previous_was_dir = false;
				for (name, snapshot_diff) in sorted_diffs {
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

				let mut sorted_diffs: Vec<_> = dir_diff.new_files.iter().by_ref().collect();
				sorted_diffs.sort_by_key(|&(_, diff)| diff.size());
				sorted_diffs.reverse();

				print_indent(indent + 1);
				println!("{}", "New files:".bright_red().bold());
				for (name, snapshot) in sorted_diffs {
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

				let mut sorted_diffs: Vec<_> = dir_diff.deleted_files.iter().by_ref().collect();
				sorted_diffs.sort_by_key(|&(_, diff)| diff.size());
				sorted_diffs.reverse();

				print_indent(indent + 1);
				println!("{}", "Deleted files:".bright_green().bold());
				for (name, snapshot) in sorted_diffs {
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
	let parts = match shell_words::split(command) {
		Ok(parts) => parts,
		Err(err) => {
			println!("{}", format!("Failed to parse command: {err}").red());
			return;
		}
	};
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
