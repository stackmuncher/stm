use super::kwc::{KeywordCounter, KeywordCounterSet};
use super::tech::Tech;
use chrono;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashSet;
use std::fs::File;
use std::io::prelude::*;
use tokio::process::Command;
use tracing::{error, info, debug, trace, warn};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename = "tech")]
pub struct Report {
    pub tech: HashSet<Tech>,
    pub timestamp: String,
    pub unprocessed_file_names: HashSet<String>,
    pub unknown_file_types: HashSet<KeywordCounter>,
    pub user_name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub repo_name: String,
    /// A UUID of the report
    #[serde(skip_serializing_if = "String::is_empty")]
    pub report_id: String,
    /// A unique name containing user name and project name
    #[serde(skip_serializing_if = "String::is_empty")]
    pub report_name: String,
    /// S3 keys of the reports merged into this one
    pub reports_included: HashSet<String>,
    /// List of names and emails of other committers
    pub collaborators: Option<HashSet<(String, String)>>,
    /// The date of the first commit
    pub date_init: Option<String>,
    /// The date of the current HEAD
    pub date_head: Option<String>,
    /// The current list of files in the GIT tree
    pub tree_files: Option<Vec<String>>,
}

impl Report {
    /// .report
    pub const REPORT_FILE_NAME_SUFFIX: &'static str = ".report";

    /// Adds up `tech` totals from `other_report` into `self`, clears unprocessed files and unknown extensions.
    pub fn merge(merge_into: Option<Self>, other_report: Self) -> Option<Self> {
        let mut merge_into = merge_into;
        let mut other_report = other_report;

        // update keyword summaries and muncher name in all tech records
        let mut new_rep_tech = Report::new(String::new(), String::new());
        for mut tech in other_report.tech.drain() {
            tech.refs_kw = Tech::new_kw_summary(&tech.refs);
            tech.pkgs_kw = Tech::new_kw_summary(&tech.pkgs);
            // reset the muncher names on other_report to merge per-language
            // tech1==tech2 if munchers and languages are the same
            // we want to combine multiple munchers for the same language
            tech.muncher_name = String::new();
            new_rep_tech.add_tech_record(tech);
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
                merge_into_inner.add_tech_record(tech);
            }

            // merge unknown_file_types
            for uft in other_report.unknown_file_types {
                merge_into_inner.unknown_file_types.increment_counters(uft);
            }

            // collect names of sub-reports in an array for easy retrieval
            merge_into_inner.reports_included.insert(other_report.report_name);

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

            // merge collaborators
            if other_report.collaborators.is_some() {
                // this should not happen, but check just in case if there is a hashset
                if merge_into_inner.collaborators.is_none() {
                    warn!("Missing collaborators in the master report");
                    merge_into_inner.collaborators = Some(HashSet::new());
                }

                let colabs = merge_into_inner.collaborators.as_mut().unwrap();
                for x in other_report.collaborators.unwrap() {
                    colabs.insert(x);
                }
            } else {
                warn!("Missing collaborators in the other report");
            }
        }

        merge_into
    }

    /// Add a new Tech record merging with the existing records.
    pub(crate) fn add_tech_record(&mut self, tech: Tech) {
        trace!("lang: {}, files: {}", tech.language, tech.files);
        // add totals to the existing record, if any
        if let Some(mut master) = self.tech.take(&tech) {
            trace!("match in master, lang: {}, files: {}", master.language, master.files);
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
            self.tech.insert(tech);
        }
    }

    /// Generates a new report name in a consistent way.
    pub fn generate_report_name(user_name: &String, repo_name: &String) -> String {
        [user_name, "/", repo_name, Report::REPORT_FILE_NAME_SUFFIX].concat()
    }

    /// Create a blank report with the current timestamp.
    pub(crate) fn new(user_name: String, repo_name: String) -> Self {
        let report_name = Report::generate_report_name(&user_name, &repo_name);
        let mut reports_included: HashSet<String> = HashSet::new();
        reports_included.insert(report_name.clone());

        Report {
            tech: HashSet::new(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            unprocessed_file_names: HashSet::new(),
            unknown_file_types: HashSet::new(),
            user_name: user_name.clone(),
            repo_name: repo_name.clone(),
            report_name,
            report_id: uuid::Uuid::new_v4().to_string(),
            reports_included,
            collaborators: None,
            date_head: None,
            date_init: None,
            tree_files: None,
        }
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
        error!("Failed to get the prev report from S3");
        None
    }

    /// Add a file that won't be processed because it is of unknown type.
    pub(crate) fn add_unprocessed_file(&mut self, file_name: &String) {
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
                trace!("No extension on {}", file_name);
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

    /// Executes a git command in the specified dir. Returns stdout or Err.
    pub async fn execute_git_command(args: Vec<String>, repo_dir: &String) -> Result<Vec<u8>, ()> {
        // build `git ...` command
        let mut cmd = Command::new("git");
        cmd.args(args);
        cmd.current_dir(&repo_dir);

        // run git reset
        let git_output = match cmd.output().await {
            Err(_e) => {
                error!("Git command failed");
                return Err(());
            }
            Ok(v) => v,
        };

        // check the status of the cloning
        let status = git_output.status.to_string();
        trace!("Status: {}, stdout len: {}", status, git_output.stdout.len());

        // the exit code must be 0 or there was a problem
        if git_output.status.code().is_none() || git_output.status.code() != Some(0) {
            let std_err = String::from_utf8(git_output.stderr).unwrap_or("Faulty stderr".into());
            error!("Git command failed. Status: {}. Stderr: {}", status, std_err);
            return Err(());
        }

        // stdout is Vec<u8>
        Ok(git_output.stdout)
    }

    /// Adds details about the commit history to the report.
    /// Exits early if the rev-list cannot be extracted.
    pub(crate) async fn extract_commit_info(&mut self, repo_dir: &String) {
        debug!("Extracting git rev-list");
        let git_output = match Report::execute_git_command(
            vec!["log".into(), "--no-decorate".into(), "--encoding=utf-8".into()],
            repo_dir,
        )
        .await
        {
            Err(_) => {
                return;
            }
            Ok(v) => v,
        };

        // try to convert the commits into a list of lines
        let git_output = String::from_utf8_lossy(&git_output);
        if git_output.len() == 0 {
            warn!("Zero-length rev-list");
            return;
        }

        // loop through all the lines to get Authors
        for line in git_output.lines() {
            if line.starts_with("Author: ") {
                trace!("{}", line);
                // the author line looks something like this
                //Lorenzo Baboollie <lorenzo@xamsie.be>
                let (_, author) = line.split_at(7);
                {
                    trace!("Extracted: {}", author);
                    // go to the next line if there is no author
                    let author = author.trim();
                    if author.is_empty() {
                        continue;
                    }

                    // there is some colab data - prepare the container
                    if self.collaborators.is_none() {
                        self.collaborators = Some(HashSet::new());
                    }

                    // try to split the author details into name and email
                    if author.ends_with(">") {
                        if let Some(idx) = author.rfind(" <") {
                            let (author_n, author_e) = author.split_at(idx);
                            trace!("Split: {}|{}", author_n, author_e);
                            let author_e = author_e.trim_end_matches(">").trim_start_matches(" <");
                            self.collaborators
                                .as_mut()
                                .unwrap()
                                .insert((author_n.to_owned(), author_e.to_owned()));
                            continue;
                        };
                    }
                    // split failed - add the entire line
                    trace!("Split failed");
                    self.collaborators
                        .as_mut()
                        .unwrap()
                        .insert((author.to_owned(), "".to_owned()));
                }
            }
            // there is also the commit message, but that is unimplemented
        }

        // loop through the top few lines to find the date of the last commit
        trace!("Looking for HEAD commit date");
        for line in git_output.lines() {
            trace!("{}", line);
            if line.starts_with("Date:   ") {
                let (_, date) = line.split_at(7);
                trace!("Extracted: {}", date);
                // go to the next line if there is no date (impossible?)
                let date = date.trim();
                if date.is_empty() {
                    error!("Encountered a commit with no date: {}", line);
                    break;
                }

                // Formatter: https://docs.rs/chrono/0.4.15/chrono/format/strftime/index.html
                // Example: Mon Aug 10 22:47:56 2020 +0200
                if let Ok(d) = chrono::DateTime::parse_from_str(date, "%a %b %d %H:%M:%S %Y %z") {
                    trace!("Parsed as: {}", d.to_rfc3339());
                    self.date_head = Some(d.to_rfc3339());
                    break;
                } else {
                    error! {"Invalid commit date format: {}", date};
                };
            }
        }

        // loop through the bottom few lines to find the date of the first commit
        trace!("Looking for INIT commit date");
        for line in git_output.lines().rev() {
            trace!("{}", line);
            if line.starts_with("Date:   ") {
                let (_, date) = line.split_at(7);
                trace!("Extracted: {}", date);
                // go to the next line if there is no date (impossible?)
                let date = date.trim();
                if date.is_empty() {
                    error!("Encountered a commit with no date: {}", line);
                    break;
                }

                // Formatter: https://docs.rs/chrono/0.4.15/chrono/format/strftime/index.html
                if let Ok(d) = chrono::DateTime::parse_from_str(date, "%a %b %d %H:%M:%S %Y %z") {
                    trace!("Parsed as: {}", d.to_rfc3339());
                    self.date_init = Some(d.to_rfc3339());
                    break;
                } else {
                    error! {"Invalid commit date format: {}", date};
                };
            }
        }
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
