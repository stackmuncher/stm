use super::git::get_hashed_remote_urls;
use super::kwc::{KeywordCounter, KeywordCounterSet};
use super::tech::Tech;
use crate::{contributor::Contributor, git::GitLogEntry, utils};
use chrono;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashSet;
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
use tracing::{debug, error, info, warn};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename = "tech")]
pub struct Report {
    /// Combined summary per technology, e.g. Rust, C# or CSS
    /// This member can be shared publicly after some clean up
    pub tech: HashSet<Tech>,
    /// Per-file technology summary, e.g. Rust/main.rs.
    /// This member should not be shared publicly, unless it's a public project
    /// because file names are sensitive info that can be exploited.
    #[serde(skip_serializing_if = "HashSet::is_empty", default = "HashSet::new")]
    pub per_file_tech: HashSet<Tech>,
    pub timestamp: String,
    #[serde(skip_serializing_if = "HashSet::is_empty", default = "HashSet::new")]
    pub unprocessed_file_names: HashSet<String>,
    #[serde(skip_serializing_if = "HashSet::is_empty", default = "HashSet::new")]
    pub unknown_file_types: HashSet<KeywordCounter>,
    /// GitHub user name, if known
    #[serde(skip_serializing_if = "String::is_empty", default = "String::new")]
    pub github_user_name: String,
    /// A public name of the project, if known. GitHub project names do not include the user name.
    /// E.g. `https://github.com/awslabs/aws-lambda-rust-runtime.git` would be `aws-lambda-rust-runtime`.
    #[serde(skip_serializing_if = "String::is_empty", default = "String::new")]
    pub github_repo_name: String,
    /// A list of hashed remote URLs from the repo. They are used in place of the private project name
    /// and can be used to match a local project to publicly available projects. If that happens the project name
    /// is populated automatically by STM on the server side
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url_hashes: Option<HashSet<String>>,
    /// A UUID of the report
    #[serde(skip_serializing_if = "String::is_empty", default = "String::new")]
    pub report_id: String,
    /// A unique name containing user name and project name when stored in S3, e.g. `rimutaka/stackmuncher.report`
    #[serde(skip_serializing_if = "String::is_empty", default = "String::new")]
    pub report_s3_name: String,
    /// The commit used to generate the report
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_commit_sha1: Option<String>,
    /// A SHA1 hash of all commit SHA1s to determine changes by looking at the log
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_hash: Option<String>,
    /// S3 keys of the reports from `report_s3_name` merged into a combined user or org report
    #[serde(skip_serializing_if = "HashSet::is_empty", default = "HashSet::new")]
    pub reports_included: HashSet<String>,
    /// A list of GIT identities for the contributor included in the report.
    /// Used only in combined contributor reports
    #[serde(skip_serializing_if = "HashSet::is_empty", default = "HashSet::new")]
    pub git_ids_included: HashSet<String>,
    /// List of names and emails of all committers for this repo. Only applies to per-project reports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contributors: Option<Vec<Contributor>>,
    /// List of names or emails of contributors (authors and committers) from `contributors` section.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contributor_git_ids: Option<HashSet<String>>,
    /// The date of the first commit
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_init: Option<String>,
    /// The date of the current HEAD
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_head: Option<String>,
    /// The current list of files in the GIT tree
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_files: Option<HashSet<String>>,
    /// Is `true` if the report was generated by adding a single commit to a cached report
    #[serde(default = "default_as_false")]
    pub is_single_commit: bool,
    /// Git identity of the author of the last (HEAD) commit. Should only be present in the project report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_commit_author: Option<String>,
}

/// A plug for Serde default
fn default_as_false() -> bool {
    false
}

impl Report {
    /// .report
    pub const REPORT_FILE_NAME_SUFFIX: &'static str = ".report";

    /// Repos with this more files than this are ignored
    /// This is a temporary measure. The file count should be taken after some files were ignored,
    /// but since ignoring files like nodejs modules is not implemented we'll just ignore such repos.
    pub const MAX_FILES_PER_REPO: usize = 5000;

    /// Adds up `tech` totals from `other_report` into `self`, clears unprocessed files and unknown extensions.
    pub fn merge(merge_into: Option<Self>, other_report: Self) -> Option<Self> {
        let mut merge_into = merge_into;
        let mut other_report = other_report;

        // update keyword summaries and muncher name in all tech records
        let mut new_rep_tech = Report::new();
        for mut tech in other_report.tech.drain() {
            tech.refs_kw = Tech::new_kw_summary(&tech.refs);
            tech.pkgs_kw = Tech::new_kw_summary(&tech.pkgs);
            // reset the muncher names on other_report to merge per-language
            // tech1==tech2 if munchers and languages are the same
            // we want to combine multiple munchers for the same language
            tech.muncher_name = String::new();
            new_rep_tech.merge_tech_record(tech);
        }
        other_report.tech = new_rep_tech.tech;

        // the very first report is added with minimal changes
        if merge_into.is_none() {
            info!("Adding 1st report");
            other_report.unprocessed_file_names.clear();
            merge_into = Some(other_report);
        } else {
            // additional reports are merged
            info!("Merging reports");
            let merge_into_inner = merge_into.as_mut().unwrap();

            // merge all tech records
            for tech in other_report.tech {
                merge_into_inner.merge_tech_record(tech);
            }

            // merge unknown_file_types
            for uft in other_report.unknown_file_types {
                merge_into_inner.unknown_file_types.increment_counters(uft);
            }

            // collect names of sub-reports in an array for easy retrieval
            if !other_report.report_s3_name.is_empty() {
                merge_into_inner.reports_included.insert(other_report.report_s3_name);
            }

            // update the date of the last commit
            if merge_into_inner.date_head.is_none() {
                // this should not happen - all commits have dates, so should the reports
                warn!("Missing date_head in master");
                merge_into_inner.date_head = other_report.date_head;
            } else if other_report.date_head.is_some() {
                // update if the report has a newer date
                if merge_into_inner.date_head.as_ref().unwrap() < other_report.date_head.as_ref().unwrap() {
                    merge_into_inner.date_head = other_report.date_head;
                }
            }

            // repeat the same logic for the oldest commit
            if merge_into_inner.date_init.is_none() {
                // this should not happen - all commits have dates, so should the reports
                warn!("Missing date_init in master");
                merge_into_inner.date_init = other_report.date_init;
            } else if other_report.date_init.is_some() {
                // update if the report has a newer date
                if merge_into_inner.date_init.as_ref().unwrap() > other_report.date_init.as_ref().unwrap() {
                    merge_into_inner.date_init = other_report.date_init;
                }
            }

            // only contributor IDs are getting merged
            if let Some(contributor_git_ids) = other_report.contributor_git_ids {
                // this should not happen often, but check just in case if there is a hashset
                if merge_into_inner.contributor_git_ids.is_none() {
                    warn!("Missing contributor ids in the master report");
                    merge_into_inner.contributor_git_ids = Some(HashSet::new());
                }
                for contributor_git_id in contributor_git_ids {
                    merge_into_inner
                        .contributor_git_ids
                        .as_mut()
                        .unwrap()
                        .insert(contributor_git_id);
                }
            } else {
                warn!("Missing contributors in the other report");
            };
        }

        merge_into
    }

    /// Add a new Tech record merging with the existing records. It removes per-file and some other
    /// potentially sensitive info used for local caching.
    pub(crate) fn merge_tech_record(&mut self, tech: Tech) {
        debug!("Merging Tech, lang: {}, files: {}", tech.language, tech.files);
        // Tech is hashed with the file name for per-file Tech records, but here
        // they are summaries, so it has to be removed to match
        let tech = tech.reset_file_and_commit_info();
        // add totals to the existing record, if any
        if let Some(mut master) = self.tech.take(&tech) {
            debug!(
                "Tech match in master, lang: {}, files: {}",
                master.language, master.files
            );
            // add up numeric values
            master.docs_comments += tech.docs_comments;
            master.files += tech.files;
            master.inline_comments += tech.inline_comments;
            master.line_comments += tech.line_comments;
            master.total_lines += tech.total_lines;
            master.blank_lines += tech.blank_lines;
            master.block_comments += tech.block_comments;
            master.bracket_only_lines += tech.bracket_only_lines;
            master.code_lines += tech.code_lines;

            // add keyword counts
            for kw in tech.keywords {
                master.keywords.increment_counters(kw);
            }

            // add dependencies
            for kw in tech.refs {
                master.refs.increment_counters(kw);
            }
            for kw in tech.pkgs {
                master.pkgs.increment_counters(kw);
            }

            // add unique words from dependencies - references
            if tech.refs_kw.is_some() {
                // init the field if None
                if master.refs_kw.is_none() {
                    master.refs_kw = Some(HashSet::new());
                }

                let refs_kw = master.refs_kw.as_mut().unwrap();
                for kw in tech.refs_kw.unwrap() {
                    refs_kw.increment_counters(kw);
                }
            }

            // add unique words from dependencies - packages
            if tech.pkgs_kw.is_some() {
                // init the field if None
                if master.pkgs_kw.is_none() {
                    master.pkgs_kw = Some(HashSet::new());
                }

                let pkgs_kw = master.pkgs_kw.as_mut().unwrap();
                for kw in tech.pkgs_kw.unwrap() {
                    pkgs_kw.increment_counters(kw);
                }
            }

            // re-insert the master record
            self.tech.insert(master);
        } else {
            // there no matching tech record - add it to the hashmap for the 1st time
            // but reset file-specific data first
            debug!("No matching Tech exists - inserting as-is");
            self.tech.insert(tech.reset_file_and_commit_info());
        }
    }

    /// Combines per_file_tech records choosing the most recent record by comparing the commit dates if there is a conflict.
    /// It does not affect `tech` records. They need to be updated using a separate function.
    /// Adds the name of the other report to `reports_included`.
    pub fn merge_contributor_reports(&mut self, other_report: Self, contributor_git_id: String) {
        debug!("Merging contributor report for {}", contributor_git_id);
        'outer: for tech in other_report.per_file_tech {
            // check if tech should be added to the report at all or is it older than what we already have
            for existing_tech in &self.per_file_tech {
                if *existing_tech == tech && existing_tech.commit_date_epoch > tech.commit_date_epoch {
                    continue 'outer;
                }
            }

            // remove a matching record if it's older
            // this double handling is done because I could not find a way to remove the record inside a for-loop
            self.per_file_tech
                .retain(|t| *t != tech || t.commit_date_epoch > tech.commit_date_epoch);

            // insert the new one
            self.per_file_tech.insert(tech);
        }

        self.git_ids_included.insert(contributor_git_id);
    }

    /// Deletes existing `tech` records and re-creates them from scratch using `per_file_tech` records.
    pub fn recompute_tech_section(&mut self) {
        debug!("Recomputing tech section");
        self.tech.clear();

        for tech in self.per_file_tech.clone() {
            self.merge_tech_record(tech);
        }
    }

    /// Resets report timestamp, contributor and report IDs.
    pub fn reset_combined_contributor_report(&mut self, contributor_git_id: String) {
        debug!("Resetting combined contributor report for {}", contributor_git_id);
        self.report_id = uuid::Uuid::new_v4().to_string();
        self.timestamp = chrono::Utc::now().to_rfc3339();
        self.report_s3_name = String::new();
        self.git_ids_included.insert(contributor_git_id);
    }

    /// Removes some sections that make no sense in the combined report.
    pub fn reset_combined_dev_report(&mut self) {
        self.contributors = None;
        self.tree_files = None;
        self.remote_url_hashes = None;
        self.report_commit_sha1 = None;
        self.last_commit_author = None;
        self.log_hash = None;
        self.unprocessed_file_names.clear();
        self.per_file_tech.clear();

        self.github_repo_name = String::new();
        self.github_user_name = String::new();
        self.report_id = String::new();
        self.report_s3_name = String::new();
        self.timestamp = chrono::Utc::now().to_rfc3339();
    }

    /// Returns an abridge copy with some bulky sections removed for indexing in a DB:
    /// * per_file_tech
    /// * contributor.touched_files
    pub fn abridge(self) -> Self {
        let mut report = self;

        // this can be huge and is not really needed for search
        report.per_file_tech.clear();

        // the list of contributors is useful, but indexing every file in the db isn't needed
        if let Some(contributors) = report.contributors.as_mut() {
            for contributor in contributors {
                contributor.touched_files.clear();
            }
        };

        report
    }

    /// Generates a new report name in a consistent way if both github user and repo names are known.
    /// The contributor hash is optional and is only used for contributor reports, which are stored in a folder with the repo name.
    pub fn generate_report_s3_name(
        github_user_name: &String,
        github_repo_name: &String,
        contributor_sha1_hash: Option<String>,
    ) -> String {
        if github_user_name.is_empty() || github_repo_name.is_empty() {
            return String::new();
        }

        // contributor part is optional
        let contributor_part = match contributor_sha1_hash {
            None => String::new(),
            Some(v) => ["/".to_owned(), v].concat(),
        };

        [
            github_user_name,
            "/",
            github_repo_name,
            &contributor_part,
            Report::REPORT_FILE_NAME_SUFFIX,
        ]
        .concat()
    }

    /// Create a blank report with the current timestamp and a unique ID.
    pub(crate) fn new() -> Self {
        Report {
            tech: HashSet::new(),
            per_file_tech: HashSet::new(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            unprocessed_file_names: HashSet::new(),
            unknown_file_types: HashSet::new(),
            github_user_name: String::new(),
            github_repo_name: String::new(),
            remote_url_hashes: None,
            report_s3_name: String::new(),
            report_id: uuid::Uuid::new_v4().to_string(),
            reports_included: HashSet::new(),
            git_ids_included: HashSet::new(),
            contributor_git_ids: None,
            contributors: None,
            date_head: None,
            date_init: None,
            tree_files: None,
            report_commit_sha1: None,
            is_single_commit: false,
            log_hash: None,
            last_commit_author: None,
        }
    }

    /// Add github details to the report and generate an S3 file name. Missing details are ignored. It will try to add whatever it can.
    pub fn with_github(
        self,
        github_user_name: &String,
        github_repo_name: &String,
        contributor_sha1_hash: Option<String>,
    ) -> Self {
        // make self mutable
        let mut report = self;

        // check if any data is missing
        if github_user_name.is_empty() || github_repo_name.is_empty() {
            warn!(
                "Missing github details for user {}, repo: {}",
                github_user_name, github_repo_name
            );
        } else {
            // generate the S3 file name
            report.report_s3_name =
                Report::generate_report_s3_name(&github_user_name, &github_repo_name, contributor_sha1_hash);
            if !report.report_s3_name.is_empty() {
                report.reports_included.insert(report.report_s3_name.clone());
            }
        }

        report.github_user_name = github_user_name.clone();
        report.github_repo_name = github_repo_name.clone();

        report
    }

    /// A helper function to match the S3 output.
    /// Returns None if there are any problems converting the S3 data into
    /// the struct because it would be just regenerated downstream if None.
    /// It's a bit of a hack.
    pub fn from_s3_bytes(s3_bytes: Result<Vec<u8>, ()>) -> Option<Self> {
        if let Ok(rpt) = s3_bytes {
            if let Ok(rpt) = serde_json::from_slice::<Report>(rpt.as_slice()) {
                info!("Loaded prev report from S3");
                return Some(rpt);
            }
        };
        info!("Failed to get a cached report from S3");
        None
    }

    /// Load a report from the local storage, if one exists. Returns None and logs errors on failure.
    pub fn from_disk(path: &String) -> Option<Self> {
        // check if the file exists at all
        let existing_report_file = Path::new(path);
        if !existing_report_file.exists() {
            info!("No report found at {}. The repo will be processed in full.", path,);

            return None;
        }

        // try to load the file and read its contents
        let mut existing_report_file = match File::open(path) {
            Err(e) => {
                error!("Cannot read report at {} due to {}.", path, e);
                return None;
            }
            Ok(v) => v,
        };
        let mut report_contents = String::new();
        if let Err(e) = existing_report_file.read_to_string(&mut report_contents) {
            error!("Failed to read report contents from {} due to {}", path, e);
            return None;
        };

        // convert to a struct and return
        match serde_json::from_str::<Report>(&report_contents) {
            Err(e) => {
                error!("Failed to deser report contents from {} due to {}", path, e);
                return None;
            }
            Ok(v) => {
                info!("Loaded a report from {}", path);
                return Some(v);
            }
        }
    }

    /// Add a file that won't be processed because it is of unknown type and count the number of files
    /// with the same extension.
    fn add_unprocessed_file(&mut self, file_name: &String) {
        // add the file name to the list
        self.unprocessed_file_names.insert(file_name.clone());

        // check if this particular extension was encountered
        if let Some(position) = file_name.rfind(".") {
            let ext = file_name.split_at(position);
            // filter out files with no extension and files that sit in a folder
            // starting with a ., e.g. `.bin/license`
            if !ext.1.is_empty() && ext.1.find("/").is_none() && ext.1.find("\\").is_none() {
                let ext = KeywordCounter {
                    k: ext.1.trim_start_matches(".").to_string(),
                    t: None,
                    c: 1,
                };
                self.unknown_file_types.increment_counters(ext);
            } else {
                debug!("No extension on {}", file_name);
            }
        }
    }

    /// First it tries to save into the specified location. If that failed it saves into the local folder.
    pub fn save_as_local_file(&self, file_name: &String) {
        // try to create the file
        let mut file = match File::create(file_name) {
            Err(e) => {
                error!("Cannot save in {} due to {}", file_name, e);
                panic!();
            }
            Ok(f) => {
                info!("Saving into {}", file_name);
                f
            }
        };

        write!(file, "{}", self).expect("Failed to save in the specified location. ");
    }

    /// Adds details about the commit history to the report: head, init, contributors, collaborators, log hash, and remote URLs.
    /// Does not panic (exits early) if `git rev-list` command fails.
    pub(crate) async fn add_commits_history(
        self,
        repo_dir: &String,
        git_remote_url_regex: &Regex,
        git_log: Vec<GitLogEntry>,
    ) -> Self {
        let mut report = self;
        debug!("Adding commit history");

        // get the date of the last commit
        if let Some(commit) = git_log.iter().next() {
            if commit.date_epoch > 0 {
                report.date_head = Some(commit.date.clone());
                report.report_commit_sha1 = Some(commit.sha1.clone());
                report.last_commit_author = Some(Contributor::git_identity_from_name_email_pair(
                    &commit.author_name_email,
                ));
            }
        }

        // get the date of the first commit
        if let Some(commit) = git_log.iter().last() {
            if commit.date_epoch > 0 {
                report.date_init = Some(commit.date.clone());
            }
        }

        // hash the list of commits to determine if there were any history re-writes
        report.log_hash = Some(utils::hash_vec_sha1(
            git_log.iter().map(|entry| entry.sha1.clone()).collect::<Vec<String>>(),
        ));

        // this part consumes git_log because there is a lot of data in it
        // so should appear at the end
        report.contributors = Some(Contributor::from_commit_history(git_log));
        report.contributor_git_ids = Some(
            report
                .contributors
                .as_ref()
                .unwrap()
                .iter()
                .map(|contributor| contributor.git_id.clone())
                .collect::<HashSet<String>>(),
        );

        // get the list of remote hashes for matching projects without exposing their names
        report.remote_url_hashes = match get_hashed_remote_urls(repo_dir, git_remote_url_regex).await {
            Err(_) => {
                error!("Failed to hash remote URLs");
                None
            }
            Ok(v) => {
                debug!("Hashed {} remote URLs", v.len());
                Some(v)
            }
        };

        report
    }

    /// Copy the list of collaborators, init and head dates from the old report.
    pub async fn copy_commit_info(self, old_report: &Self) -> Self {
        let mut report = self;

        report.contributors = old_report.contributors.clone();
        report.date_head = old_report.date_head.clone();
        report.date_init = old_report.date_init.clone();
        report.remote_url_hashes = old_report.remote_url_hashes.clone();
        info!("Copied commit info from the old report");

        report
    }

    /// Adds the entire list of tree files or just the touched files to the report, extracts names of unprocessed files
    /// and counts their extensions.
    pub fn update_project_file_lists(self, all_tree_files: HashSet<String>) -> Self {
        // result collector
        let mut report = self;

        // subtract processed files from all files to get the list of unprocessed files
        let processed_files = report
            .per_file_tech
            .iter()
            .map(|tech| tech.file_name.as_ref().unwrap_or(&String::new()).clone())
            .collect::<HashSet<String>>();
        let unprocessed_files = all_tree_files
            .difference(&processed_files)
            .map(|f| f)
            .collect::<Vec<&String>>();

        // store the names of unprocessed files in the report
        debug!("Found {} un-processed files", unprocessed_files.len());
        for f in unprocessed_files {
            report.add_unprocessed_file(f);
        }

        // save the entire list of tree files in the report
        report.tree_files = Some(all_tree_files);

        report
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match serde_json::to_string(self) {
            Ok(v) => {
                write!(f, "{}", v).expect("Invalid JSON string in report.");
            }
            Err(e) => {
                write!(f, "Cannot serialize Report {:?}", e).expect("Invalid error msg in report.");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod test_report {
    use super::Report;
    use std::fs::File;
    use std::io::prelude::*;

    #[test]
    fn test_merge() {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .init();

        let r1 = File::open("test-files/report1.json").unwrap();
        let r1: Report = serde_json::from_reader(r1).unwrap();

        let r2 = File::open("test-files/report2.json").unwrap();
        let r2: Report = serde_json::from_reader(r2).unwrap();

        // calculate the expected sums of files
        let cs_files: usize = r1
            .tech
            .iter()
            .chain(r2.tech.iter())
            .map(|t| if t.language == "C#" { t.files } else { 0 })
            .sum();
        let md_files: usize = r1
            .tech
            .iter()
            .chain(r2.tech.iter())
            .map(|t| if t.language == "Markdown" { t.files } else { 0 })
            .sum();
        let ps1_files: usize = r1
            .tech
            .iter()
            .chain(r2.tech.iter())
            .map(|t| if t.language == "PowerShell" { t.files } else { 0 })
            .sum();

        // do the same for refs and pkgs in C#
        let cs_refs: usize = r1
            .tech
            .iter()
            .chain(r2.tech.iter())
            .map(|t| {
                if t.language == "C#" {
                    let rs: usize = t.refs.iter().map(|tr| tr.c).sum();
                    rs
                } else {
                    0
                }
            })
            .sum();
        let cs_pkgs: usize = r1
            .tech
            .iter()
            .chain(r2.tech.iter())
            .map(|t| {
                if t.language == "C#" {
                    let rs: usize = t.pkgs.iter().map(|tr| tr.c).sum();
                    rs
                } else {
                    0
                }
            })
            .sum();

        let rm = Report::merge(None, r1).unwrap();
        let rm = Report::merge(Some(rm), r2).unwrap();
        let rms = serde_json::to_string_pretty(&rm).unwrap();

        let mut rmf = File::create("test-files/report_merged.json").unwrap();
        let _ = rmf.write_all(&mut rms.as_bytes());

        // compare number of files
        for t in rm.tech.iter() {
            match t.language.as_str() {
                "C#" => {
                    assert_eq!(t.files, cs_files, "C# file count");
                }
                "Markdown" => {
                    assert_eq!(t.files, md_files, "Markdown file count");
                }
                "PowerShell" => {
                    assert_eq!(t.files, ps1_files, "PowerShell file count");
                }
                _ => assert!(false, "Unexpected language {}", t.language),
            }
        }

        // compare number of refs and pkgs for C#
        let cs_refs_rm: usize = rm
            .tech
            .iter()
            .map(|t| {
                if t.language == "C#" {
                    let rs: usize = t.refs.iter().map(|tr| tr.c).sum();
                    rs
                } else {
                    0
                }
            })
            .sum();
        println!("Refs counts, merged: {}, expected {}", cs_refs_rm, cs_refs);
        assert_eq!(cs_refs_rm, cs_refs, "C# refs count");

        let cs_pkgs_rm: usize = rm
            .tech
            .iter()
            .map(|t| {
                if t.language == "C#" {
                    let rs: usize = t.pkgs.iter().map(|tr| tr.c).sum();
                    rs
                } else {
                    0
                }
            })
            .sum();
        println!("Pkgs counts, merged: {}, expected {}", cs_pkgs_rm, cs_pkgs);
        assert_eq!(cs_pkgs_rm, cs_pkgs, "C# pkgs count");
    }
}
