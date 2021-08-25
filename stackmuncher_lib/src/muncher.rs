use regex::Regex;
use serde::Deserialize;
use std::hash::{Hash, Hasher};
use tracing::{error, trace};

// ===================================================================
// IMPORTANT: update the hashing function after adding any new members
// ===================================================================
#[derive(Deserialize, Clone, Debug)]
pub struct Muncher {
    #[serde(default)]
    pub muncher_name: String,
    pub language: String,
    pub keywords: Option<Vec<String>>,
    pub bracket_only: Option<Vec<String>>,
    pub line_comments: Option<Vec<String>>,
    pub inline_comments: Option<Vec<String>>,
    pub doc_comments: Option<Vec<String>>,
    pub block_comments_start: Option<Vec<String>>,
    pub block_comments_end: Option<Vec<String>>,
    pub refs: Option<Vec<String>>,
    pub packages: Option<Vec<String>>,
    // REMEMBER TO ADD ANY NEW MEMBERS TO HASH TRAIT!!!

    // Regex section is compiled once from the above properties
    #[serde(skip)]
    pub bracket_only_regex: Option<Vec<Regex>>,
    #[serde(skip)]
    pub line_comments_regex: Option<Vec<Regex>>,
    #[serde(skip)]
    pub inline_comments_regex: Option<Vec<Regex>>,
    #[serde(skip)]
    pub doc_comments_regex: Option<Vec<Regex>>,
    #[serde(skip)]
    pub block_comments_start_regex: Option<Vec<Regex>>,
    #[serde(skip)]
    pub block_comments_end_regex: Option<Vec<Regex>>,
    #[serde(skip)]
    pub refs_regex: Option<Vec<Regex>>,
    #[serde(skip)]
    pub packages_regex: Option<Vec<Regex>>,
    #[serde(skip)]
    pub blank_line_regex: Option<Vec<Regex>>,
    #[serde(skip)]
    pub keywords_regex: Option<Vec<Regex>>,
    /// Set to true for newly added munchers to help upstream code
    /// identify them and share with other threads
    #[serde(skip)]
    pub brand_new: bool,
    /// A short hash of the muncher rules to detect a change for reprocessing
    #[serde(skip)]
    pub muncher_hash: u64,
}

impl Muncher {
    /// Create a new instance from the muncher file contents.
    /// Returns None if there was a problem loading it
    pub fn new(muncher_contents: &str, muncher_name: &String) -> Option<Self> {
        trace!("Loading {}", muncher_name);

        // convert into a struct
        let mut conf = match serde_json::from_str::<Self>(muncher_contents) {
            Err(e) => {
                error!("Cannot parse muncher {} due to {}", muncher_name, e);
                return None;
            }
            Ok(v) => v,
        };

        conf.muncher_name = muncher_name.clone();
        conf.brand_new = true;

        // hash the muncher to ID the rules and avoid reprocessing
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        conf.hash(&mut hasher);
        conf.muncher_hash = hasher.finish();

        // compile all regex strings
        if conf.compile_all_regex().is_err() {
            return None;
        }

        Some(conf)
    }

    /// Compiles regex strings.
    fn compile_all_regex(&mut self) -> Result<(), ()> {
        trace!("Compiling regex for {}", self.muncher_name);

        // resets to `false` if any of the regex statements failed to compile
        // this is done to loop through all regex strings in the file and give
        // a combined view of any failed ones
        let mut compilation_success = true;

        if let Some(v) = self.bracket_only.as_ref() {
            for s in v {
                compilation_success &= Muncher::add_regex_to_list(&mut self.bracket_only_regex, s);
            }
        }

        if let Some(v) = self.line_comments.as_ref() {
            for s in v {
                compilation_success &= Muncher::add_regex_to_list(&mut self.line_comments_regex, s);
            }
        }

        if let Some(v) = self.inline_comments.as_ref() {
            for s in v {
                compilation_success &= Muncher::add_regex_to_list(&mut self.inline_comments_regex, s);
            }
        }

        if let Some(v) = self.doc_comments.as_ref() {
            for s in v {
                compilation_success &= Muncher::add_regex_to_list(&mut self.doc_comments_regex, s);
            }
        }

        if let Some(v) = self.block_comments_start.as_ref() {
            for s in v {
                compilation_success &= Muncher::add_regex_to_list(&mut self.block_comments_start_regex, s);
            }
        }

        if let Some(v) = self.block_comments_end.as_ref() {
            for s in v {
                compilation_success &= Muncher::add_regex_to_list(&mut self.block_comments_end_regex, s);
            }
        }

        if let Some(v) = self.refs.as_ref() {
            for s in v {
                compilation_success &= Muncher::add_regex_to_list(&mut self.refs_regex, s);
            }
        }

        if let Some(v) = self.packages.as_ref() {
            for s in v {
                compilation_success &= Muncher::add_regex_to_list(&mut self.packages_regex, s);
            }
        }

        if let Some(v) = self.keywords.as_ref() {
            for s in v {
                Muncher::add_regex_to_list(&mut self.keywords_regex, s);
            }
        }

        // empty strings should have the same regex, but this may change - odd one out
        compilation_success &= Muncher::add_regex_to_list(&mut self.blank_line_regex, &r"^\s*$".to_string());

        // panic if there were compilation errors
        if compilation_success {
            return Ok(());
        } else {
            error!("Compilation for {} failed.", self.muncher_name);
            return Err(());
        }
    }

    /// Adds the `regex` to the supplied `list`. Creates an instance of Vec<Regex> on the first insert.
    /// Always returns Some(). Returns FALSE on regex compilation error.
    pub fn add_regex_to_list(list: &mut Option<Vec<Regex>>, regex: &String) -> bool {
        // try to compile the regex
        let compiled_regex = match Regex::new(regex) {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to compile regex {} with {}", regex, e);
                return false;
            }
        };

        // get the existing vector or create a new one
        if list.is_none() {
            list.replace(Vec::new());
        }

        // add the new regex to the list
        list.as_mut().unwrap().push(compiled_regex);
        true
    }
}

impl Hash for Muncher {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.muncher_name.hash(state);
        self.language.hash(state);
        self.keywords.hash(state);
        self.bracket_only.hash(state);
        self.line_comments.hash(state);
        self.inline_comments.hash(state);
        self.doc_comments.hash(state);
        self.block_comments_start.hash(state);
        self.block_comments_end.hash(state);
        self.refs.hash(state);
        self.packages.hash(state);
    }
}
