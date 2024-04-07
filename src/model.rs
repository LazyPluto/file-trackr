use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirSnapshot {
	pub size: u64,
	pub files: Vec<(String, FSEntrySnapshot)>,
}

impl DirSnapshot {
	pub fn get_file_snapshot(&self, filename: &String) -> Option<&FSEntrySnapshot> {
		self.files
			.iter()
			.find_map(|(name, snapshot)| Some(snapshot).filter(|_| *name == *filename))
	}

	pub fn depth(&self) -> u32 {
		self.files
			.iter()
			.map(|(_, snapshot)| 1 + snapshot.depth())
			.max()
			.unwrap_or(0)
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
	pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FSEntrySnapshot {
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
pub struct FSSnapshot {
	pub root: FSEntrySnapshot,
	pub path: String,
	pub depth: u32,
	pub time: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PathPattern {
	Exact(PathBuf),
	Wildcard,
}

impl PathPattern {
	pub fn matches(&self, path: &Path) -> bool {
		match self {
			Self::Exact(p) => p == path,
			Self::Wildcard => true,
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SnapshotRule {
	RuleSet(SnapshotRuleSet),
	Single(u32),
}

impl SnapshotRule {
	pub fn query(&self, path: &Path) -> Option<u32> {
		match self {
			Self::RuleSet(ruleset) => ruleset.query(path),
			Self::Single(depth) => {
				let path_depth = path.components().count();
				depth.checked_sub(path_depth as u32)
			}
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRuleSet {
	rules: Vec<(PathPattern, SnapshotRule)>,
}

impl SnapshotRuleSet {
	pub fn new() -> Self {
		Self { rules: Vec::new() }
	}

	pub fn add_rule(&mut self, pattern: PathPattern, rule: SnapshotRule) {
		self.rules.push((pattern, rule));
	}

	pub fn query(&self, path: &Path) -> Option<u32> {
		self.rules.iter().find_map(|(pattern, rule)| {
			// Do a small dance with the borrow checker to get the root.
			let root = path.components().next();
			let root = root
				.as_ref()
				.map(|root| root.as_ref())
				.unwrap_or(Path::new(""));

			let new_path = path.strip_prefix(root).unwrap();
			pattern
				.matches(root)
				.then(|| rule.query(new_path))
				.flatten()
		})
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FSTrack {
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

	pub fn snapshots(&self) -> &[FSSnapshot] {
		&self.snapshots
	}

	pub fn depth(&self) -> u32 {
		self.depth
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

	pub fn get_snapshot(&self, id: u32) -> Option<&FSSnapshot> {
		self.snapshots.get(id as usize)
	}

	pub fn count_snapshots(&self) -> usize {
		self.snapshots.len()
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FSTrackDB {
	tracked_paths: HashMap<PathBuf, FSTrack>,
	snapshot_depth: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackPathErr {
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

	pub fn tracked_paths(&self) -> &HashMap<PathBuf, FSTrack> {
		&self.tracked_paths
	}

	pub fn snapshot_depth(&self) -> u32 {
		self.snapshot_depth
	}

	pub fn get_track(&self, path: &Path) -> Option<&FSTrack> {
		self.tracked_paths.get(path)
	}

	pub fn get_track_mut(&mut self, path: &Path) -> Option<&mut FSTrack> {
		self.tracked_paths.get_mut(path)
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeDiff {
	pub old_size: u64,
	pub new_size: u64,
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

pub struct FileSnapshotDiff {
	pub size_diff: Option<SizeDiff>,
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

pub struct DirSnapshotDiff {
	pub size_diff: Option<SizeDiff>,
	pub file_diffs: Vec<(String, FSEntrySnapshotDiff)>,
	pub new_files: Vec<(String, FSEntrySnapshot)>,
	pub deleted_files: Vec<(String, FSEntrySnapshot)>,
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

pub enum FSEntrySnapshotDiff {
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
