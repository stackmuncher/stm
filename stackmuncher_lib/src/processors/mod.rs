use super::muncher::Muncher;
use crate::git::get_blob_contents;
use crate::report::Tech;
use encoding_rs as _;
use encoding_rs::WINDOWS_1252;
use encoding_rs_io::{DecodeReaderBytes, DecodeReaderBytesBuilder};
use regex::Regex;
use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use tracing::{debug, trace, warn};

/// Extract the file as git blob contents from the repository and perform the analysis.
/// * **all_tree_files***: needed to remove local imports that match the local file name, e.g. as in Python or Rust
pub(crate) async fn process_file(
    file_name: &String,
    blob_sha1: &String,
    rules: &Muncher,
    project_dir: &Path,
    commit_sha1: &String,
    commit_date_epoch: i64,
    commit_date_iso: &String,
    all_tree_files: Option<&HashSet<String>>,
) -> Result<Tech, String> {
    debug!("Muncher: {}", rules.muncher_name);

    // prepare the blank structure
    let mut tech = Tech {
        language: rules.language.clone(),
        muncher_name: rules.muncher_name.clone(),
        file_name: Some(file_name.clone()),
        commit_sha1: Some(commit_sha1.clone()),
        commit_date_epoch: Some(commit_date_epoch),
        commit_date_iso: Some(commit_date_iso.clone()),
        files: 1,
        total_lines: 0,
        code_lines: 0,
        line_comments: 0,
        block_comments: 0,
        docs_comments: 0,
        inline_comments: 0,
        blank_lines: 0,
        bracket_only_lines: 0,
        keywords: HashSet::new(), // this is wasteful
        refs: HashSet::new(),     // they should be Option<>
        refs_kw: None,
        pkgs: HashSet::new(), // they should be Option<>
        pkgs_kw: None,
        muncher_hash: rules.muncher_hash,
        history: None,
    };

    // get file contents as UTF
    let lines = match get_file_lines(file_name, blob_sha1, project_dir, false).await {
        Ok(v) => v,
        Err(_) => {
            // try ANSI if that fails
            match get_file_lines(file_name, blob_sha1, project_dir, true).await {
                Err(_) => {
                    // exit now if the file is either empty or binary
                    trace!("Empty or binary file - not processing.");
                    return Ok(tech);
                }
                Ok(v) => v,
            }
        }
    };
    if lines.len() == 0 {
        // no point processing an empty file further
        trace!("The file is empty - not processing.");
        return Ok(tech);
    }

    // get total lines
    tech.total_lines = lines.len();

    // set to true when the line is inside a block comment
    let mut inside_block_comment = false;

    // evaluate every line
    for line in lines {
        trace!("{}", line);
        // check for non-code parts

        // check if it's inside a block comment
        if inside_block_comment {
            tech.block_comments += 1;
            trace!("block_comments");
            // is it a closing block?
            if match_line(&rules.block_comments_end_regex, &line) {
                inside_block_comment = false;
            }
            continue;
        }

        if match_line(&rules.block_comments_start_regex, &line) {
            tech.block_comments += 1;
            trace!("block_comments");

            // mark it as the start of the block if there is no closing part on the same line
            if !match_line(&rules.block_comments_end_regex, &line) {
                inside_block_comment = true;
            }

            continue;

            // It is possible that some code may have multiple opening / closing comments on the same page.
            // That would probably be just messy code that can be ignored.
            // Those comments may also be inside string literals, e.g. "some text like this /*".
            // The same applies to other types of comments - they can be inside " ... "
        }

        if match_line(&rules.doc_comments_regex, &line) {
            tech.docs_comments += 1;
            trace!("doc_comments");
            continue;
        }

        if match_line(&rules.line_comments_regex, &line) {
            tech.line_comments += 1;
            trace!("line_comments");
            continue;
        }

        if match_line(&rules.inline_comments_regex, &line) {
            tech.inline_comments += 1;
            trace!("inline_comments");
            continue;
        }

        if match_line(&rules.bracket_only_regex, &line) {
            tech.bracket_only_lines += 1;
            trace!("bracket_only_lines");
            continue;
        }

        if match_line(&rules.blank_line_regex, &line) {
            tech.blank_lines += 1;
            trace!("blank_lines");
            continue;
        }

        // this is a code line of sorts
        tech.code_lines += 1;
        trace!("code_lines");

        // count keywords and package references
        tech.count_refs(&rules.refs_regex, &line);
        tech.count_pkgs(&rules.packages_regex, &line);
        tech.count_keywords(&rules.keywords_regex, &line);
    }

    // remove refs names that match local file names
    tech = tech.remove_local_imports(all_tree_files);

    Ok(tech)
}

/// Returns multiple lines from a text file, if the encoding is UTF-something.
/// Returns an error if the file cannot be read or cannot be decoded.
/// ANSI files may be incompatible with UTF, so use it with try_ansi=false first
/// and then try_ansi=true to read it as WINDOWS_1252
async fn get_file_lines(
    file_name: &String,
    blob_sha1: &String,
    project_dir: &Path,
    try_ansi: bool,
) -> Result<Vec<String>, ()> {
    // read the file
    let file = get_blob_contents(project_dir, &blob_sha1).await?;
    // this decoder is required to read non-UTF-8 files
    let mut decoder = if try_ansi {
        DecodeReaderBytesBuilder::new()
            .encoding(Some(WINDOWS_1252))
            .build(&file[..])
    } else {
        DecodeReaderBytes::new(&file[..])
    };

    // output collector
    let mut lines: Vec<String> = Vec::new();

    // try to read the file
    let mut utf8_string = String::new();
    if let Err(e) = decoder.read_to_string(&mut utf8_string) {
        // log an error only on the 2nd run of this function when ANSI is ON
        if try_ansi {
            warn!("Cannot decode {} as UTF due to {} with ANSI={}", file_name, e, try_ansi);
        }

        return Err(());
    };

    // convert the file into a collection of lines
    for line in utf8_string.as_str().lines() {
        lines.push(line.into());
    }

    Ok(lines)
}

/// Returns true if there is a regex and it matches the line.
#[inline(always)]
fn match_line(regex: &Option<Vec<Regex>>, line: &String) -> bool {
    if let Some(v) = regex {
        for r in v {
            if r.is_match(&line) {
                trace!("{}", r);
                return true;
            }
        }
    }

    // no match found
    false
}
