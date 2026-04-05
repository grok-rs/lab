use serde::Deserialize;

use super::job::{AllowFailure, When};
use super::variables::Variables;

/// A single rule entry in a job's `rules:` list.
/// Ref: <https://docs.gitlab.com/ci/yaml/#rules>
#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    /// Conditional expression using CI/CD variable syntax.
    /// Ref: <https://docs.gitlab.com/ci/jobs/job_rules/#rules-if>
    #[serde(rename = "if")]
    pub if_expr: Option<String>,

    /// Monitor file changes.
    /// Ref: <https://docs.gitlab.com/ci/jobs/job_rules/#rules-changes>
    #[serde(default)]
    pub changes: Option<ChangesConfig>,

    /// Check file existence.
    /// Ref: <https://docs.gitlab.com/ci/jobs/job_rules/#rules-exists>
    #[serde(default)]
    pub exists: Option<Vec<String>>,

    /// Override the job's `when` if this rule matches.
    #[serde(default)]
    pub when: Option<When>,

    /// Override allow_failure if this rule matches.
    #[serde(default)]
    pub allow_failure: Option<AllowFailure>,

    /// Override variables if this rule matches.
    #[serde(default)]
    pub variables: Option<Variables>,
}

/// File change monitoring configuration.
/// Ref: <https://docs.gitlab.com/ci/jobs/job_rules/#rules-changes>
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ChangesConfig {
    Simple(Vec<String>),
    Detailed {
        paths: Vec<String>,
        #[serde(default)]
        compare_to: Option<String>,
    },
}

/// Result of evaluating a rules list.
#[derive(Debug, Clone)]
pub enum RuleResult {
    /// A rule matched — use these settings.
    Matched {
        when: When,
        allow_failure: AllowFailure,
        variables: Option<Variables>,
    },
    /// No rule matched — job should not run.
    NotMatched,
}

/// Evaluate a list of rules for a job.
/// Rules are OR'd — first matching rule wins.
/// Ref: <https://docs.gitlab.com/ci/jobs/job_rules/>
pub fn evaluate_rules(rules: &[Rule], variables: &Variables, default_when: When) -> RuleResult {
    for rule in rules {
        if rule_matches(rule, variables) {
            return RuleResult::Matched {
                when: rule.when.unwrap_or(default_when),
                allow_failure: rule.allow_failure.clone().unwrap_or_default(),
                variables: rule.variables.clone(),
            };
        }
    }
    RuleResult::NotMatched
}

/// Check if a single rule matches.
/// Multiple conditions within a rule are AND'd.
fn rule_matches(rule: &Rule, variables: &Variables) -> bool {
    // If no conditions are specified, the rule always matches
    let has_conditions = rule.if_expr.is_some() || rule.changes.is_some() || rule.exists.is_some();
    if !has_conditions {
        return true;
    }

    // Check if: condition
    if let Some(expr) = &rule.if_expr {
        if !evaluate_if_expression(expr, variables) {
            return false;
        }
    }

    // Check changes: condition
    // Ref: <https://docs.gitlab.com/ci/jobs/job_rules/#rules-changes>
    if let Some(changes) = &rule.changes {
        let (patterns, compare_to) = match changes {
            ChangesConfig::Simple(paths) => (paths.clone(), None),
            ChangesConfig::Detailed { paths, compare_to } => (paths.clone(), compare_to.as_deref()),
        };
        if !check_git_changes(&patterns, compare_to) {
            return false;
        }
    }

    // Check exists: condition
    // Ref: <https://docs.gitlab.com/ci/jobs/job_rules/#rules-exists>
    if let Some(exists_patterns) = &rule.exists {
        if !check_files_exist(exists_patterns) {
            return false;
        }
    }

    true
}

/// Check if any files matching the given glob patterns have been changed in git.
/// Ref: <https://docs.gitlab.com/ci/jobs/job_rules/#rules-changes>
///
/// If `compare_to` is provided, diffs against that ref (e.g., "refs/heads/main").
/// Otherwise defaults to HEAD~1, falling back to staged changes.
fn check_git_changes(patterns: &[String], compare_to: Option<&str>) -> bool {
    let diff_ref = compare_to.unwrap_or("HEAD~1");

    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", diff_ref])
        .output();

    let changed_files: Vec<String> = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(String::from)
            .collect(),
        _ => {
            // Fallback: try staged changes
            let output = std::process::Command::new("git")
                .args(["diff", "--name-only", "--cached"])
                .output();
            match output {
                Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(String::from)
                    .collect(),
                _ => return true, // Can't determine changes, assume true
            }
        }
    };

    if changed_files.is_empty() {
        return false;
    }

    // Check if any changed file matches any pattern
    for pattern in patterns {
        let glob = match globset::Glob::new(pattern) {
            Ok(g) => g.compile_matcher(),
            Err(_) => continue,
        };
        for file in &changed_files {
            if glob.is_match(file) {
                return true;
            }
        }
    }

    false
}

/// Check if any files matching the given glob patterns exist.
/// Ref: <https://docs.gitlab.com/ci/jobs/job_rules/#rules-exists>
fn check_files_exist(patterns: &[String]) -> bool {
    for pattern in patterns {
        let _glob = match globset::GlobBuilder::new(pattern)
            .literal_separator(false)
            .build()
        {
            Ok(g) => g.compile_matcher(),
            Err(_) => continue,
        };

        // Walk current directory looking for matches
        if let Ok(entries) = glob::glob(pattern) {
            for entry in entries.flatten() {
                if entry.exists() {
                    return true;
                }
            }
        }

        // Fallback: check literal path
        if std::path::Path::new(pattern).exists() {
            return true;
        }
    }
    false
}

/// Evaluate a GitLab CI `rules:if` expression.
/// Ref: <https://docs.gitlab.com/ci/jobs/job_rules/#rules-if>
///
/// Supported operators:
/// - `==` — equality
/// - `!=` — inequality
/// - `=~` — regex match
/// - `!~` — regex non-match
/// - `&&` — logical AND
/// - `||` — logical OR
/// - Parentheses for grouping
/// - `$VAR` / `${VAR}` — variable reference (null if undefined)
/// - Quoted strings: `"value"` or `'value'`
///
/// A bare `$VAR` (no comparison) is truthy if the variable exists and is non-empty.
pub fn evaluate_if_expression(expr: &str, variables: &Variables) -> bool {
    let mut parser = ExprParser::new(expr, variables);
    parser.parse_or()
}

// ---------------------------------------------------------------------------
// Recursive descent parser for GitLab CI rule expressions
// ---------------------------------------------------------------------------

struct ExprParser<'a> {
    input: &'a str,
    pos: usize,
    variables: &'a Variables,
}

impl<'a> ExprParser<'a> {
    fn new(input: &'a str, variables: &'a Variables) -> Self {
        Self {
            input,
            pos: 0,
            variables,
        }
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.input.len() && self.input.as_bytes()[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn starts_with(&self, s: &str) -> bool {
        self.input[self.pos..].starts_with(s)
    }

    fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    /// Parse: expr_or = expr_and ('||' expr_and)*
    fn parse_or(&mut self) -> bool {
        let mut result = self.parse_and();
        loop {
            self.skip_whitespace();
            if self.starts_with("||") {
                self.advance(2);
                let rhs = self.parse_and();
                result = result || rhs;
            } else {
                break;
            }
        }
        result
    }

    /// Parse: expr_and = expr_not ('&&' expr_not)*
    fn parse_and(&mut self) -> bool {
        let mut result = self.parse_not();
        loop {
            self.skip_whitespace();
            if self.starts_with("&&") {
                self.advance(2);
                let rhs = self.parse_not();
                result = result && rhs;
            } else {
                break;
            }
        }
        result
    }

    /// Parse: expr_not = '!' expr_primary | expr_primary
    fn parse_not(&mut self) -> bool {
        self.skip_whitespace();
        if self.peek() == Some('!') && !self.starts_with("!=") && !self.starts_with("!~") {
            self.advance(1);
            !self.parse_primary()
        } else {
            self.parse_primary()
        }
    }

    /// Parse: expr_primary = '(' expr_or ')' | comparison | bare_var
    fn parse_primary(&mut self) -> bool {
        self.skip_whitespace();

        // Parenthesized expression
        if self.peek() == Some('(') {
            self.advance(1);
            let result = self.parse_or();
            self.skip_whitespace();
            if self.peek() == Some(')') {
                self.advance(1);
            }
            return result;
        }

        // Parse left-hand value
        let lhs = self.parse_value();

        self.skip_whitespace();

        // Check for comparison operator
        if self.starts_with("==") {
            self.advance(2);
            let rhs = self.parse_value();
            return match (&lhs, &rhs) {
                (Some(l), Some(r)) => l == r,
                (None, None) => true,
                _ => false,
            };
        }
        if self.starts_with("!=") {
            self.advance(2);
            let rhs = self.parse_value();
            return match (&lhs, &rhs) {
                (Some(l), Some(r)) => l != r,
                (None, None) => false,
                _ => true,
            };
        }
        if self.starts_with("=~") {
            self.advance(2);
            let pattern = self.parse_regex();
            return match (&lhs, &pattern) {
                (Some(val), Some(pat)) => regex::Regex::new(pat)
                    .map(|re| re.is_match(val))
                    .unwrap_or(false),
                _ => false,
            };
        }
        if self.starts_with("!~") {
            self.advance(2);
            let pattern = self.parse_regex();
            return match (&lhs, &pattern) {
                (Some(val), Some(pat)) => regex::Regex::new(pat)
                    .map(|re| !re.is_match(val))
                    .unwrap_or(true),
                _ => true,
            };
        }

        // Bare variable — truthy if exists and non-empty
        lhs.is_some_and(|v| !v.is_empty())
    }

    /// Parse a value: $VAR, ${VAR}, "string", 'string', or null.
    /// Returns None if the variable is undefined.
    fn parse_value(&mut self) -> Option<String> {
        self.skip_whitespace();

        match self.peek()? {
            '$' => self.parse_variable_ref(),
            '"' => Some(self.parse_quoted_string('"')),
            '\'' => Some(self.parse_quoted_string('\'')),
            'n' if self.starts_with("null") => {
                self.advance(4);
                None
            }
            _ => {
                // Bare word (shouldn't happen in well-formed expressions, but handle gracefully)
                let start = self.pos;
                while self.pos < self.input.len() {
                    let c = self.input.as_bytes()[self.pos] as char;
                    if c.is_ascii_whitespace()
                        || c == ')'
                        || c == '&'
                        || c == '|'
                        || c == '='
                        || c == '!'
                    {
                        break;
                    }
                    self.pos += 1;
                }
                Some(self.input[start..self.pos].to_string())
            }
        }
    }

    fn parse_variable_ref(&mut self) -> Option<String> {
        self.advance(1); // skip '$'

        let var_name = if self.peek() == Some('{') {
            self.advance(1); // skip '{'
            let start = self.pos;
            while self.pos < self.input.len() && self.input.as_bytes()[self.pos] != b'}' {
                self.pos += 1;
            }
            let name = &self.input[start..self.pos];
            if self.peek() == Some('}') {
                self.advance(1);
            }
            name
        } else {
            let start = self.pos;
            while self.pos < self.input.len() {
                let c = self.input.as_bytes()[self.pos];
                if c.is_ascii_alphanumeric() || c == b'_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            &self.input[start..self.pos]
        };

        self.variables.get(var_name).map(|v| v.value().to_string())
    }

    fn parse_quoted_string(&mut self, quote: char) -> String {
        self.advance(1); // skip opening quote
        let start = self.pos;
        while self.pos < self.input.len() && self.input.as_bytes()[self.pos] as char != quote {
            self.pos += 1;
        }
        let s = self.input[start..self.pos].to_string();
        if self.peek() == Some(quote) {
            self.advance(1); // skip closing quote
        }
        s
    }

    fn parse_regex(&mut self) -> Option<String> {
        self.skip_whitespace();
        if self.peek() != Some('/') {
            return None;
        }
        self.advance(1); // skip opening /
        let mut pattern = String::new();
        while self.pos < self.input.len() {
            let ch = self.input.as_bytes()[self.pos] as char;
            if ch == '\\' && self.pos + 1 < self.input.len() {
                // Escaped character — include both backslash and next char
                pattern.push('\\');
                self.pos += 1;
                pattern.push(self.input.as_bytes()[self.pos] as char);
                self.pos += 1;
            } else if ch == '/' {
                break;
            } else {
                pattern.push(ch);
                self.pos += 1;
            }
        }
        if self.peek() == Some('/') {
            self.advance(1); // skip closing /
        }
        Some(pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::variables::VariableValue;

    fn vars(pairs: &[(&str, &str)]) -> Variables {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), VariableValue::Simple(v.to_string())))
            .collect()
    }

    #[test]
    fn test_equality() {
        let v = vars(&[("CI_BRANCH", "main")]);
        assert!(evaluate_if_expression("$CI_BRANCH == \"main\"", &v));
        assert!(!evaluate_if_expression("$CI_BRANCH == \"dev\"", &v));
    }

    #[test]
    fn test_inequality() {
        let v = vars(&[("CI_BRANCH", "main")]);
        assert!(evaluate_if_expression("$CI_BRANCH != \"dev\"", &v));
        assert!(!evaluate_if_expression("$CI_BRANCH != \"main\"", &v));
    }

    #[test]
    fn test_bare_variable_truthy() {
        let v = vars(&[("CI_BRANCH", "main")]);
        assert!(evaluate_if_expression("$CI_BRANCH", &v));

        let v = vars(&[("CI_BRANCH", "")]);
        assert!(!evaluate_if_expression("$CI_BRANCH", &v));

        let v = vars(&[]);
        assert!(!evaluate_if_expression("$CI_BRANCH", &v));
    }

    #[test]
    fn test_logical_and_or() {
        let v = vars(&[("A", "1"), ("B", "2")]);
        assert!(evaluate_if_expression("$A && $B", &v));
        assert!(evaluate_if_expression("$A || $MISSING", &v));
        assert!(!evaluate_if_expression("$MISSING && $A", &v));
    }

    #[test]
    fn test_null_comparison() {
        let v = vars(&[("A", "1")]);
        assert!(evaluate_if_expression("$A != null", &v));
        assert!(!evaluate_if_expression("$A == null", &v));
        assert!(evaluate_if_expression("$MISSING == null", &v));
    }

    #[test]
    fn test_regex_match() {
        let v = vars(&[("CI_BRANCH", "feature/login")]);
        assert!(evaluate_if_expression("$CI_BRANCH =~ /^feature\\//", &v));
        assert!(!evaluate_if_expression("$CI_BRANCH =~ /^main$/", &v));
    }

    #[test]
    fn test_regex_not_match() {
        let v = vars(&[("CI_BRANCH", "main")]);
        assert!(evaluate_if_expression("$CI_BRANCH !~ /^feature/", &v));
    }

    #[test]
    fn test_parentheses() {
        let v = vars(&[("A", "1")]);
        assert!(evaluate_if_expression("($A == \"1\") && ($A != \"2\")", &v));
    }

    #[test]
    fn test_complex_expression() {
        let v = vars(&[
            ("CI_PIPELINE_SOURCE", "merge_request_event"),
            ("CI_MERGE_REQUEST_TARGET_BRANCH_NAME", "main"),
        ]);
        assert!(evaluate_if_expression(
            "$CI_PIPELINE_SOURCE == \"merge_request_event\" && $CI_MERGE_REQUEST_TARGET_BRANCH_NAME == \"main\"",
            &v,
        ));
    }
}
