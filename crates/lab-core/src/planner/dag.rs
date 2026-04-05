use std::collections::{HashMap, HashSet, VecDeque};

use indexmap::IndexMap;

use crate::error::{LabError, Result};
use crate::model::job::{Job, When};
use crate::model::pipeline::{Plan, PlannedJob, Stage};
use crate::model::rules::{RuleResult, evaluate_rules};
use crate::model::variables::Variables;

/// Build an execution plan from a pipeline's jobs and stages.
///
/// The plan respects both stage ordering and `needs:` DAG dependencies.
/// Jobs within the same stage run in parallel unless constrained by `needs:`.
///
/// Ref: <https://docs.gitlab.com/ci/yaml/#stages>
/// Ref: <https://docs.gitlab.com/ci/yaml/needs/>
pub fn build_plan(
    stages: &[String],
    jobs: &IndexMap<String, Job>,
    variables: &Variables,
    job_filter: Option<&str>,
    stage_filter: Option<&str>,
) -> Result<Plan> {
    // Step 1: Evaluate rules and determine which jobs will run
    let active_jobs = filter_active_jobs(jobs, variables, job_filter, stage_filter)?;

    // Step 2: Expand parallel:matrix into multiple jobs
    let active_jobs = expand_matrix(active_jobs);

    if active_jobs.is_empty() {
        return Ok(Plan { stages: Vec::new() });
    }

    // Step 3: Validate dependencies
    validate_dependencies(&active_jobs, jobs)?;

    // Step 4: Group jobs by stage, respecting needs: ordering
    let plan_stages = build_stages(stages, &active_jobs, jobs)?;

    Ok(Plan {
        stages: plan_stages,
    })
}

/// Filter jobs based on rules evaluation and user filters.
fn filter_active_jobs(
    jobs: &IndexMap<String, Job>,
    variables: &Variables,
    job_filter: Option<&str>,
    stage_filter: Option<&str>,
) -> Result<IndexMap<String, Job>> {
    let mut active = IndexMap::new();

    for (name, job) in jobs {
        // Apply user filters
        if let Some(filter) = job_filter {
            if name != filter {
                continue;
            }
        }
        if let Some(filter) = stage_filter {
            if job.stage != filter {
                continue;
            }
        }

        // Evaluate rules
        if let Some(rules) = &job.rules {
            match evaluate_rules(rules, variables, job.when) {
                RuleResult::Matched {
                    when,
                    allow_failure,
                    variables: rule_vars,
                } => {
                    if when == When::Never {
                        continue;
                    }
                    let mut job = job.clone();
                    job.when = when;
                    job.allow_failure = allow_failure;
                    // Apply rules:variables — merge into job variables
                    // Ref: <https://docs.gitlab.com/ci/yaml/#rulesvariables>
                    if let Some(rv) = rule_vars {
                        for (k, v) in rv {
                            job.variables.insert(k, v);
                        }
                    }
                    active.insert(name.clone(), job);
                }
                RuleResult::NotMatched => continue,
            }
        } else {
            // No rules — job runs based on its `when:` setting
            if job.when == When::Never {
                continue;
            }
            active.insert(name.clone(), job.clone());
        }
    }

    Ok(active)
}

/// Expand `parallel:matrix:` into multiple jobs.
/// Ref: <https://docs.gitlab.com/ci/yaml/#parallelmatrix>
///
/// Each matrix combination produces a new job with the matrix variables
/// added to the job's variables.
fn expand_matrix(jobs: IndexMap<String, Job>) -> IndexMap<String, Job> {
    use crate::model::job::ParallelConfig;
    use crate::model::variables::VariableValue;

    let mut expanded = IndexMap::new();

    for (name, job) in jobs {
        match &job.parallel {
            Some(ParallelConfig::Matrix { matrix }) => {
                // Generate all combinations
                let combinations = generate_matrix_combinations(matrix);
                for (i, combo) in combinations.iter().enumerate() {
                    let suffix: String = combo
                        .values()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let _expanded_name = format!("{name} [{suffix}]");

                    let mut expanded_job = job.clone();
                    expanded_job.parallel = None; // Don't re-expand
                    // Add matrix variables to job variables
                    for (k, v) in combo {
                        expanded_job
                            .variables
                            .insert(k.clone(), VariableValue::Simple(v.clone()));
                    }

                    expanded.insert(
                        if combinations.len() == 1 {
                            name.clone()
                        } else {
                            format!("{name} {}", i + 1)
                        },
                        expanded_job,
                    );
                }
            }
            Some(ParallelConfig::Count(n)) => {
                // Simple parallel: N copies
                for i in 1..=*n {
                    let mut expanded_job = job.clone();
                    expanded_job.parallel = None;
                    expanded_job.variables.insert(
                        "CI_NODE_INDEX".to_string(),
                        VariableValue::Simple(i.to_string()),
                    );
                    expanded_job.variables.insert(
                        "CI_NODE_TOTAL".to_string(),
                        VariableValue::Simple(n.to_string()),
                    );
                    expanded.insert(format!("{name} {i}/{n}"), expanded_job);
                }
            }
            None => {
                expanded.insert(name, job);
            }
        }
    }

    expanded
}

/// Generate all combinations from a matrix definition.
fn generate_matrix_combinations(
    matrix: &[IndexMap<String, crate::model::job::StringOrVec>],
) -> Vec<IndexMap<String, String>> {
    let mut all_combos = Vec::new();

    for entry in matrix {
        // Each entry produces a cross-product of its values
        let mut entry_combos: Vec<IndexMap<String, String>> = vec![IndexMap::new()];

        for (key, values) in entry {
            let value_list = values.as_slice();
            let mut new_combos = Vec::new();
            for combo in &entry_combos {
                for val in &value_list {
                    let mut new_combo = combo.clone();
                    new_combo.insert(key.clone(), val.to_string());
                    new_combos.push(new_combo);
                }
            }
            entry_combos = new_combos;
        }

        all_combos.extend(entry_combos);
    }

    all_combos
}

/// Validate that all `needs:` references point to known jobs.
/// Validate that all `needs:` references point to known jobs.
/// Optional dependencies (`needs:optional: true`) are skipped if not found.
/// Ref: <https://docs.gitlab.com/ci/yaml/#needsoptional>
fn validate_dependencies(
    active_jobs: &IndexMap<String, Job>,
    all_jobs: &IndexMap<String, Job>,
) -> Result<()> {
    for (name, job) in active_jobs {
        if let Some(needs) = &job.needs {
            for need in needs {
                let dep_name = need.job_name();
                if !all_jobs.contains_key(dep_name) && !need.is_optional() {
                    return Err(LabError::UnknownDependency {
                        job: name.clone(),
                        dependency: dep_name.to_string(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Build ordered stages from active jobs using topological sort.
///
/// Uses Kahn's algorithm: jobs with no unmet dependencies are placed
/// in the current stage. Each stage's jobs can run in parallel.
/// Build ordered stages from active jobs.
///
/// GitLab CI has two dependency modes:
/// - **Stage mode** (no `needs:`): job depends on ALL jobs in previous stages
/// - **DAG mode** (`needs:` present): job depends only on listed jobs
///
/// Ref: <https://docs.gitlab.com/ci/yaml/#stages>
/// Ref: <https://docs.gitlab.com/ci/yaml/needs/>
fn build_stages(
    stage_order: &[String],
    active_jobs: &IndexMap<String, Job>,
    _all_jobs: &IndexMap<String, Job>,
) -> Result<Vec<Stage>> {
    let stage_index: HashMap<&str, usize> = stage_order
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), i))
        .collect();

    // Build dependency graph considering both explicit needs and implicit stage deps
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();

    for (name, job) in active_jobs {
        in_degree.entry(name.clone()).or_insert(0);

        if let Some(needs) = &job.needs {
            // DAG mode: only depend on explicitly listed jobs
            for need in needs {
                let dep = need.job_name().to_string();
                if active_jobs.contains_key(&dep) {
                    *in_degree.entry(name.clone()).or_insert(0) += 1;
                    dependents.entry(dep).or_default().push(name.clone());
                }
            }
        } else {
            // Stage mode: depend on ALL active jobs in previous stages
            let my_stage_idx = stage_index.get(job.stage.as_str()).copied().unwrap_or(0);

            for (dep_name, dep_job) in active_jobs {
                if dep_name == name {
                    continue;
                }
                let dep_stage_idx = stage_index
                    .get(dep_job.stage.as_str())
                    .copied()
                    .unwrap_or(0);
                if dep_stage_idx < my_stage_idx {
                    *in_degree.entry(name.clone()).or_insert(0) += 1;
                    dependents
                        .entry(dep_name.clone())
                        .or_default()
                        .push(name.clone());
                }
            }
        }
    }

    // Topological sort using Kahn's algorithm
    let mut queue: VecDeque<String> = in_degree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(name, _)| name.clone())
        .collect();

    // Sort initial queue by stage order for deterministic output
    let mut queue_vec: Vec<String> = queue.drain(..).collect();
    queue_vec.sort_by_key(|name| {
        active_jobs
            .get(name)
            .map(|j| {
                stage_index
                    .get(j.stage.as_str())
                    .copied()
                    .unwrap_or(usize::MAX)
            })
            .unwrap_or(usize::MAX)
    });
    queue = queue_vec.into();

    let mut resolved: HashSet<String> = HashSet::new();
    let mut plan_stages: Vec<Stage> = Vec::new();

    while !queue.is_empty() {
        let mut stage_groups: IndexMap<String, Vec<PlannedJob>> = IndexMap::new();

        let batch: Vec<String> = queue.drain(..).collect();
        for name in &batch {
            resolved.insert(name.clone());
            let job = &active_jobs[name];
            stage_groups
                .entry(job.stage.clone())
                .or_default()
                .push(PlannedJob {
                    name: name.clone(),
                    job: job.clone(),
                    matrix_entry: None,
                });
        }

        // Add stages in the defined stage order
        for stage_name in stage_order {
            if let Some(jobs) = stage_groups.shift_remove(stage_name) {
                if let Some(existing) = plan_stages.iter_mut().find(|s| s.name == *stage_name) {
                    existing.jobs.extend(jobs);
                } else {
                    plan_stages.push(Stage {
                        name: stage_name.clone(),
                        jobs,
                    });
                }
            }
        }

        // Release dependents
        for name in &batch {
            if let Some(deps) = dependents.get(name) {
                for dep in deps {
                    if let Some(deg) = in_degree.get_mut(dep) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(dep.clone());
                        }
                    }
                }
            }
        }
    }

    // Check for cycles
    if resolved.len() != active_jobs.len() {
        let unresolved: Vec<&str> = active_jobs
            .keys()
            .filter(|k| !resolved.contains(k.as_str()))
            .map(String::as_str)
            .collect();
        return Err(LabError::CircularDependency(unresolved.join(", ")));
    }

    plan_stages.retain(|s| !s.jobs.is_empty());
    Ok(plan_stages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::job::{Job, Need};

    fn job(stage: &str) -> Job {
        Job {
            stage: stage.to_string(),
            script: vec!["echo test".to_string()],
            ..default_job()
        }
    }

    fn job_with_needs(stage: &str, needs: Vec<&str>) -> Job {
        Job {
            stage: stage.to_string(),
            script: vec!["echo test".to_string()],
            needs: Some(
                needs
                    .into_iter()
                    .map(|n| Need::Simple(n.to_string()))
                    .collect(),
            ),
            ..default_job()
        }
    }

    fn default_job() -> Job {
        Job {
            image: None,
            stage: "test".to_string(),
            script: vec![],
            before_script: None,
            after_script: None,
            variables: Variables::new(),
            rules: None,
            needs: None,
            dependencies: None,
            artifacts: None,
            cache: None,
            services: None,
            when: When::OnSuccess,
            allow_failure: Default::default(),
            timeout: None,
            retry: None,
            parallel: None,
            extends: None,
            tags: None,
            resource_group: None,
            interruptible: None,
            inherit: None,
            coverage: None,
            start_in: None,
            trigger: None,
            manual_confirmation: None,
        }
    }

    #[test]
    fn test_simple_stages() {
        let stages = vec!["build".into(), "test".into(), "deploy".into()];
        let mut jobs = IndexMap::new();
        jobs.insert("compile".to_string(), job("build"));
        jobs.insert("unit_test".to_string(), job("test"));
        jobs.insert("release".to_string(), job("deploy"));

        let plan = build_plan(&stages, &jobs, &Variables::new(), None, None).unwrap();
        assert_eq!(plan.stages.len(), 3);
        assert_eq!(plan.stages[0].name, "build");
        assert_eq!(plan.stages[1].name, "test");
        assert_eq!(plan.stages[2].name, "deploy");
    }

    #[test]
    fn test_needs_dag() {
        let stages = vec!["build".into(), "test".into()];
        let mut jobs = IndexMap::new();
        jobs.insert("build_a".to_string(), job("build"));
        jobs.insert("build_b".to_string(), job("build"));
        jobs.insert(
            "test_a".to_string(),
            job_with_needs("test", vec!["build_a"]),
        );

        let plan = build_plan(&stages, &jobs, &Variables::new(), None, None).unwrap();
        assert_eq!(plan.stages.len(), 2);
        assert_eq!(plan.stages[0].jobs.len(), 2); // build_a and build_b parallel
        assert_eq!(plan.stages[1].jobs.len(), 1); // test_a
    }

    #[test]
    fn test_circular_dependency() {
        let stages = vec!["test".into()];
        let mut jobs = IndexMap::new();
        jobs.insert("a".to_string(), job_with_needs("test", vec!["b"]));
        jobs.insert("b".to_string(), job_with_needs("test", vec!["a"]));

        let result = build_plan(&stages, &jobs, &Variables::new(), None, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_job_filter() {
        let stages = vec!["test".into()];
        let mut jobs = IndexMap::new();
        jobs.insert("test_a".to_string(), job("test"));
        jobs.insert("test_b".to_string(), job("test"));

        let plan = build_plan(&stages, &jobs, &Variables::new(), Some("test_a"), None).unwrap();
        assert_eq!(plan.stages[0].jobs.len(), 1);
        assert_eq!(plan.stages[0].jobs[0].name, "test_a");
    }
}
