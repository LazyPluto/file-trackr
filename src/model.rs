use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

fn chop_path_root<'a>(path: &'a Path) -> (&'a Path, &'a Path) {
	let mut components = path.components();

	// TODO: REMOVE UNSAFE.
	let root = components
		.next()
		.map(|root: std::path::Component<'a>| unsafe {
			std::mem::transmute::<&Path, &'a Path>(root.as_ref())
		})
		.unwrap_or(Path::new(""));

	(root, components.as_path())
}

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

	pub fn find_by_path(&self, path: &Path) -> Option<&FSEntrySnapshot> {
		let (root, rest) = chop_path_root(path);
		let root = format!("{}", root.display());
		let file = self
			.files
			.iter()
			.find_map(|(name, file)| (*name == root).then_some(file))?;
		if rest.components().count() == 0 {
			Some(file)
		} else {
			file.find_by_path(rest)
		}
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
	pub fn find_by_path(&self, path: &Path) -> Option<&Self> {
		if path.components().count() == 0 {
			Some(self)
		} else {
			match self {
				Self::File(_) => None,
				Self::Directory(dir) => dir.find_by_path(path),
			}
		}
	}

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
	pub time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

impl FromStr for PathPattern {
	type Err = ();

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"*" => Ok(Self::Wildcard),
			s => Ok(Self::Exact(PathBuf::from(s))),
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SnapshotRule {
	RuleSet(SnapshotRuleSet),
	Single(u32),
}

impl SnapshotRule {
	pub fn single(self) -> Option<u32> {
		match self {
			Self::Single(depth) => Some(depth),
			_ => None,
		}
	}

	pub fn ruleset(self) -> Option<SnapshotRuleSet> {
		match self {
			Self::RuleSet(set) => Some(set),
			_ => None,
		}
	}

	pub fn query(&self, path: &Path) -> Option<u32> {
		match self {
			Self::RuleSet(ruleset) => ruleset.query(path),
			Self::Single(depth) => {
				let path_depth = path.components().count();
				depth.checked_sub(path_depth as u32)
			}
		}
	}

	// Returns true if the rule can contribute to a snapshot.
	// For example, a ruleset containing a single empty ruleset
	// should not be visited as it will not contribute to the
	// snapshot.
	pub fn should_visit(&self) -> bool {
		match self {
			Self::Single(_) => true,
			Self::RuleSet(ruleset) => ruleset.should_visit(),
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
		if let Some(existing_rule) = self.find_rule_mut(&pattern) {
			*existing_rule = rule;
		} else {
			self.rules.push((pattern, rule));
		}
	}

	pub fn remove_rule(&mut self, pattern: &PathPattern) -> Result<(), ()> {
		let start_len = self.rules.len();
		self.rules.retain(|(p, _)| p != pattern);
		if start_len != self.rules.len() {
			Ok(())
		} else {
			Err(())
		}
	}

	pub fn add_rule_path(&mut self, patterns: &[PathPattern], rule: SnapshotRule) {
		// TODO: Optimize
		let other = patterns.iter().rev().fold(rule, |rule, pattern| {
			let mut ruleset = SnapshotRuleSet::new();
			ruleset.add_rule(pattern.clone(), rule);
			SnapshotRule::RuleSet(ruleset)
		});
		self.update(other.ruleset().unwrap());
	}

	pub fn remove_rule_path(&mut self, patterns: &[PathPattern]) -> Result<(), ()> {
		if patterns.len() == 1 {
			self.remove_rule(&patterns[0])
		} else {
			let rule = self.find_rule_mut(&patterns[0]).ok_or(())?;
			match rule {
				SnapshotRule::Single(_) => Err(()),
				SnapshotRule::RuleSet(ruleset) => ruleset.remove_rule_path(&patterns[1..]),
			}
		}
	}

	pub fn update(&mut self, other: Self) {
		for (pattern, rule) in other.rules {
			match rule {
				SnapshotRule::Single(_) => self.add_rule(pattern, rule),
				SnapshotRule::RuleSet(ruleset) => {
					if let Some(rule) = self.find_rule_mut(&pattern) {
						match rule {
							SnapshotRule::Single(depth) => {
								let mut set = SnapshotRuleSet::new();
								set.update(ruleset);
								set.add_rule(PathPattern::Wildcard, SnapshotRule::Single(*depth));
								*rule = SnapshotRule::RuleSet(set);
							}
							SnapshotRule::RuleSet(set) => {
								set.update(ruleset);
							}
						}
					} else {
						self.add_rule(pattern, SnapshotRule::RuleSet(ruleset));
					}
				}
			}
		}
	}

	pub fn simplify(&mut self) {
		for (_, rule) in &mut self.rules {
			Self::simplify_rule(rule);
		}
	}

	pub fn simplify_path(&mut self, patterns: &[PathPattern]) -> Result<(), ()> {
		assert!(!patterns.is_empty());
		let rule = self.find_rule_mut(&patterns[0]).ok_or(())?;
		if patterns.len() == 1 {
			Self::simplify_rule(rule);
			Ok(())
		} else {
			match rule {
				SnapshotRule::Single(_) => Err(()),
				SnapshotRule::RuleSet(set) => set.simplify_path(&patterns[1..]),
			}
		}
	}

	fn simplify_rule(rule: &mut SnapshotRule) {
		match rule {
			SnapshotRule::Single(_) => {}
			SnapshotRule::RuleSet(ruleset) => {
				ruleset.simplify();
				match ruleset.rules.as_slice() {
					&[(PathPattern::Wildcard, SnapshotRule::Single(depth))] => {
						*rule = SnapshotRule::Single(depth + 1);
					}
					_ => {}
				}
			}
		}
	}

	pub fn find_rule(&self, pattern: &PathPattern) -> Option<&SnapshotRule> {
		self.rules
			.iter()
			.find_map(|(path_pattern, rule)| (path_pattern == pattern).then_some(rule))
	}

	pub fn find_rule_mut(&mut self, pattern: &PathPattern) -> Option<&mut SnapshotRule> {
		self.rules
			.iter_mut()
			.find_map(|(path_pattern, rule)| (path_pattern == pattern).then_some(rule))
	}

	pub fn query(&self, path: &Path) -> Option<u32> {
		let (root, new_path) = chop_path_root(path);
		self.rules
			.iter()
			.filter(|(_, rule)| rule.should_visit())
			.find_map(|(pattern, rule)| {
				pattern
					.matches(root)
					.then(|| rule.query(new_path))
					.flatten()
			})
	}

	pub fn should_visit(&self) -> bool {
		self.rules.iter().any(|(_, rule)| rule.should_visit())
	}

	pub fn iter_rules(&self) -> impl Iterator<Item = &(PathPattern, SnapshotRule)> {
		self.rules.iter()
	}

	pub fn iter_mut_rules(&mut self) -> impl Iterator<Item = &mut (PathPattern, SnapshotRule)> {
		self.rules.iter_mut()
	}
}

impl Default for SnapshotRuleSet {
	fn default() -> Self {
		Self::new()
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FSTrack {
	root: PathBuf,
	snapshot_ruleset: SnapshotRuleSet,
	snapshots: Vec<FSSnapshot>,
}

impl FSTrack {
	pub fn new(root: PathBuf, snapshot_ruleset: SnapshotRuleSet) -> Self {
		Self {
			root,
			snapshot_ruleset,
			snapshots: Vec::new(),
		}
	}

	pub fn root_path(&self) -> &Path {
		&self.root
	}

	pub fn snapshot_ruleset(&self) -> &SnapshotRuleSet {
		&self.snapshot_ruleset
	}

	pub fn snapshot_ruleset_mut(&mut self) -> &mut SnapshotRuleSet {
		&mut self.snapshot_ruleset
	}

	pub fn snapshots(&self) -> &[FSSnapshot] {
		&self.snapshots
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

	pub fn count_snapshots(&self) -> usize {
		self.snapshots.len()
	}

	pub fn latest_snapshot(&self) -> Option<&FSSnapshot> {
		self.snapshots.last()
	}

	pub fn get_snapshot(&self, id: u32) -> Option<&FSSnapshot> {
		self.snapshots.get(id as usize)
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
			|| !self.new_files.is_empty()
			|| !self.deleted_files.is_empty()
			|| !self.file_diffs.is_empty()
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
		matches!(self, Self::File(_))
	}

	#[allow(dead_code)]
	pub fn is_dir(&self) -> bool {
		matches!(self, Self::Directory(_))
	}
}
