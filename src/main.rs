use std::fmt::Display;
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
	DirSnapshot, FSEntrySnapshot, FSEntrySnapshotDiff, FSSnapshot, FSTrack, FileSnapshot,
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
	pub fn new(path: &Path, ruleset: &SnapshotRuleSet) -> Self {
		let (mut tx, rx) = mpsc::channel();
		let path = path.to_owned();
		let ruleset = ruleset.clone();
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
	humansize::format_size(size, humansize::BINARY)
}

fn to_human_readable_time(time: i64) -> impl Display {
	let date_time = chrono::Local.timestamp_opt(time, 0).unwrap();
	date_time.format("%a %d %b %Y %H:%M:%S")
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
						name,
						file_snapshot,
						indent + 1,
						max_depth - 1,
						colorize,
					);
				}
			}
		}
	}
}

fn print_fs_entry_snapshot_diff(
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
			format!("{name}: {old_size_str} -> {new_size_str}: {diff_sign}{diff_str}").color(color);
		println!("{colored_str}");
	} else {
		let colored_str = name.yellow();
		println!("{colored_str}");
	}

	if max_depth == 0 {
		return false;
	}

	if let FSEntrySnapshotDiff::Directory(dir_diff) = diff {
		let mut add_newline = false;
		if !dir_diff.file_diffs.is_empty() {
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

				print_fs_entry_snapshot_diff(name, snapshot_diff, indent + 1, max_depth - 1);
			}
			add_newline = true;
		}

		if !dir_diff.new_files.is_empty() {
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

		if !dir_diff.deleted_files.is_empty() {
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

fn print_ruleset(ruleset: &SnapshotRuleSet, indent: u32) {
	for (pattern, rule) in ruleset.iter_rules() {
		let pattern_str = match pattern {
			PathPattern::Exact(name) => &format!("{}", name.display()),
			PathPattern::Wildcard => "*",
		};

		print_indent(indent);

		match rule {
			SnapshotRule::Single(depth) => println!("{pattern_str}: {depth}"),
			SnapshotRule::RuleSet(ruleset) => {
				println!("{pattern_str}: ");
				print_ruleset(ruleset, indent + 1);
			}
		}
	}
}

#[derive(Debug)]
struct Context {
	track: FSTrack,
	track_path: PathBuf,
	should_stop: bool,
	commands: Vec<(clap::Command, fn(clap::ArgMatches, &mut Self))>,
}

fn handle_list(args: clap::ArgMatches, context: &mut Context) {
	match args.subcommand().unwrap() {
		("snapshots", _) => {
			let track = &context.track;
			if track.count_snapshots() == 0 {
				println!("No snapshots taken");
				return;
			}

			for (i, snapshot) in track.snapshots().iter().enumerate() {
				println!("{i}: {}", to_human_readable_time(snapshot.time as i64));
			}
		}
		_ => unreachable!(),
	}
}

fn handle_show(args: clap::ArgMatches, context: &mut Context) {
	let max_depth = args.get_one::<u32>("depth").copied().unwrap_or(u32::MAX);
	let snapshot_id = args.get_one::<u32>("id").copied();
	let path = args.get_one::<String>("path").map(PathBuf::from);

	let track = &context.track;
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

	let (root_path, root_snapshot) = if let Some(path) = path {
		let Some(root_snapshot) = snapshot.root.find_by_path(&path) else {
			println!("Failed to find snapshot of `{}`", path.display());
			return;
		};
		(track.root_path().join(path), root_snapshot)
	} else {
		(track.root_path().to_owned(), &snapshot.root)
	};
	let path = format!("{}", root_path.display());
	print_fs_entry_snapshot(&path, root_snapshot, 0, max_depth, &|x| x.normal());
}

fn handle_snap(_args: clap::ArgMatches, context: &mut Context) {
	let track = &mut context.track;
	let time = SystemTime::now()
		.duration_since(SystemTime::UNIX_EPOCH)
		.unwrap()
		.as_secs();

	let snapshot_taker = SnapshotTaker::new(track.root_path(), track.snapshot_ruleset());
	let snapshot = match snapshot_taker.wait() {
		Ok(snapshot) => snapshot,
		Err(err) => {
			println!("{}", format!("Failed to take snapshot: {err}").red());
			return;
		}
	};

	track.add_snapshot(FSSnapshot {
		root: snapshot,
		time,
	});
}

fn handle_compare(args: clap::ArgMatches, context: &mut Context) {
	let depth = args.get_one::<u32>("depth").copied().unwrap_or(u32::MAX);
	let snapshot1_id = args.get_one::<u32>("a").copied();
	let snapshot2_id = args.get_one::<u32>("b").copied();

	if snapshot1_id.is_some() != snapshot2_id.is_some() {
		println!("Both snapshot IDs are required for comparison");
		return;
	}

	let path = args.get_one::<String>("path").map(PathBuf::from);

	let track = &context.track;
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

	let (old_root, new_root) = if let Some(path) = path {
		let Some(old_root) = old.root.find_by_path(&path) else {
			println!(
				"Failed to find snapshot of `{}` in previous snapshot",
				path.display()
			);
			return;
		};
		let Some(new_root) = new.root.find_by_path(&path) else {
			println!(
				"Failed to find snapshot of `{}` in current snapshot",
				path.display()
			);
			return;
		};
		(old_root, new_root)
	} else {
		(&old.root, &new.root)
	};

	let diff = FSEntrySnapshotDiff::compute(old_root, new_root, depth);
	if diff.has_changes() {
		let old_time = to_human_readable_time(old.time as i64);
		let new_time = to_human_readable_time(new.time as i64);
		println!("{old_time} -> {new_time}");
		let path = format!("{}", track.root_path().display());
		print_fs_entry_snapshot_diff(&path, &diff, 0, depth.min(diff.depth()));
	} else {
		println!("No changes");
	}
}

fn handle_rule(args: clap::ArgMatches, context: &mut Context) {
	let track = &mut context.track;
	let ruleset = track.snapshot_ruleset_mut();

	if let Some((subcommand, args)) = args.subcommand() {
		// TODO: Cleanup
		let path = args
			.try_get_one::<String>("path")
			.map_err(|_| ())
			.and_then(|path| path.ok_or(()))
			.and_then(|path| {
				let path = PathBuf::from(path);
				path.components()
					.map(|component| {
						component
							.as_os_str()
							.to_str()
							.ok_or(())
							.and_then(|s| s.parse())
					})
					.collect::<Result<Vec<PathPattern>, ()>>()
			});

		match subcommand {
			"add" => {
				let Ok(path_patterns) = path else {
					println!("Failed to parse path pattern");
					return;
				};

				let depth = args
					.get_one::<u32>("depth")
					.copied()
					.expect("Must be present");

				ruleset.add_rule_path(&path_patterns, SnapshotRule::Single(depth));
			}
			"remove" => {
				let Ok(path_patterns) = path else {
					println!("Failed to parse path pattern");
					return;
				};

				match ruleset.remove_rule_path(&path_patterns) {
					Ok(()) => println!("Rule removed successfully"),
					Err(err) => println!("Failed to remove rule: {err:?}"),
				}
			}
			"simplify" => {
				if let Ok(path_patterns) = path {
					match ruleset.simplify_path(&path_patterns) {
						Ok(()) => println!("Rule simplified successfully"),
						Err(err) => println!("Failed to simplify rule: {err:?}"),
					}
				} else {
					ruleset.simplify();
					println!("Ruleset simplified successfully")
				}
			}
			_ => unreachable!("Unimplemented subcommand: {subcommand}"),
		}
	} else {
		print_ruleset(track.snapshot_ruleset(), 0);
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
	let track_string = serde_json::to_string(&context.track).unwrap();
	fs::write(&context.track_path, track_string).unwrap();
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
	if parts.is_empty() {
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
	let track_path = args.path;
	let track = if track_path.exists() {
		let track_string = String::from_utf8(fs::read(&track_path).unwrap()).unwrap();
		serde_json::from_str(&track_string).unwrap()
	} else {
		let path = loop {
			print!("Path to track: ");
			io::stdout().flush().unwrap();

			let mut input = String::new();
			io::stdin().read_line(&mut input).unwrap();

			if let Ok(path) = dunce::canonicalize(PathBuf::from(input.trim())) {
				break path;
			} else {
				println!("Path does not exist!");
			}
		};

		let mut ruleset = SnapshotRuleSet::new();
		ruleset.add_rule(PathPattern::Wildcard, SnapshotRule::Single(2));

		FSTrack::new(path, ruleset)
	};

	let mut commands: Vec<(clap::Command, fn(clap::ArgMatches, &mut Context))> = Vec::new();

	let list_command = clap::Command::new("list")
		.subcommand(clap::Command::new("snapshots").about("Lists snapshots of <path>"))
		.subcommand_required(true)
		.about("Lists tracks/snapshots");
	commands.push((list_command, handle_list));

	let show_command = clap::Command::new("show")
		.arg(clap::arg!([path] "Path from where to show the snapshot"))
		.arg(
			clap::arg!(-i --id <id> "ID of the snapshot which must be shown")
				.value_parser(clap::value_parser!(u32)),
		)
		.arg(
			clap::arg!(-d --depth <depth> "Maximum depth for displaying the snapshot")
				.value_parser(clap::value_parser!(u32)),
		)
		.about("Shows the newest snapshot of <path>");
	commands.push((show_command, handle_show));

	let snap_command = clap::Command::new("snap")
		.arg(
			clap::arg!(-d --depth <depth> "Maximum depth of snapshot")
				.value_parser(clap::value_parser!(u32)),
		)
		.about("Takes a snapshot of <path>");
	commands.push((snap_command, handle_snap));

	let compare_command = clap::Command::new("compare")
		.arg(clap::arg!([path] "Path from where to compare snapshots"))
		.arg(clap::arg!([a] "ID of first snapshot").value_parser(clap::value_parser!(u32)))
		.arg(clap::arg!([b] "ID of second snapshot").value_parser(clap::value_parser!(u32)))
		.arg(
			clap::arg!(-d --depth <depth> "Maximum depth for displaying the diff")
				.value_parser(clap::value_parser!(u32)),
		)
		.about("Compares the 2 newest snapshots of <path>");
	commands.push((compare_command, handle_compare));

	// TODO: Make simplify accept an optional path.
	let rule_command = clap::Command::new("rule")
		.subcommand(
			clap::Command::new("add")
				.about("Adds or modifies a rule")
				.arg(clap::arg!(<path> "Path whose rule must be added or modified"))
				.arg(clap::arg!(<depth> "Depth").value_parser(clap::value_parser!(u32))),
		)
		.subcommand(
			clap::Command::new("remove")
				.about("Removes a rule")
				.arg(clap::arg!(<path> "Path whose rule must be removed")),
		)
		.subcommand(
			clap::Command::new("simplify")
				.about("Simplifies the rule tree")
				.arg(clap::arg!([path] "Path whose rule must be simplified")),
		)
		.about("Displays/modifies the ruleset used for taking snapshots");
	commands.push((rule_command, handle_rule));

	let help_command = clap::Command::new("help")
		.arg(clap::arg!([command] "Command whose help must be displayed"))
		.about("Displays the help menu");
	commands.push((help_command, handle_help));

	let exit_command = clap::Command::new("exit")
		.visible_alias("quit")
		.about("Exits the program");
	commands.push((exit_command, handle_exit));

	let mut context = Context {
		track,
		track_path,
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
