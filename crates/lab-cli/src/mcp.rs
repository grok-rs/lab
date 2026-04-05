#![allow(clippy::ptr_arg)]
//! MCP (Model Context Protocol) stdio server for lab.
//!
//! Exposes lab's analysis, validation, and inspection tools as MCP tools
//! that AI agents (Claude Code, Cursor, etc.) can call.
//!
//! Protocol: JSON-RPC 2.0 over stdin/stdout (newline-delimited).

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

use lab_core::analyze;
use lab_core::model::variables::merge_variables;
use lab_core::parser::parse_pipeline;
use lab_core::planner::build_plan;
use lab_core::secrets;

/// Run the MCP stdio server loop.
pub fn run_server() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                write_error(
                    &mut stdout,
                    Value::Null,
                    -32700,
                    &format!("Parse error: {e}"),
                );
                continue;
            }
        };

        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");

        let response = match method {
            "initialize" => handle_initialize(&id),
            "initialized" => continue, // notification, no response
            "tools/list" => handle_tools_list(&id),
            "tools/call" => handle_tools_call(&id, &request),
            "notifications/cancelled" => continue,
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("Unknown method: {method}")}
            }),
        };

        let _ = writeln!(stdout, "{}", response);
        let _ = stdout.flush();
    }
}

fn write_error(stdout: &mut std::io::Stdout, id: Value, code: i32, message: &str) {
    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    });
    let _ = writeln!(stdout, "{}", response);
    let _ = stdout.flush();
}

fn handle_initialize(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "lab",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    })
}

fn handle_tools_list(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "tools": [
                {
                    "name": "lab_analyze",
                    "description": "Analyze a GitLab CI/CD pipeline for security, performance, and best practice issues. Returns structured findings with severity, category, and actionable suggestions.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "file": {
                                "type": "string",
                                "description": "Path to .gitlab-ci.yml (default: .gitlab-ci.yml)",
                                "default": ".gitlab-ci.yml"
                            }
                        }
                    }
                },
                {
                    "name": "lab_validate",
                    "description": "Parse and validate a .gitlab-ci.yml file. Returns the number of stages and jobs, or an error message if invalid.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "file": {
                                "type": "string",
                                "description": "Path to .gitlab-ci.yml",
                                "default": ".gitlab-ci.yml"
                            }
                        }
                    }
                },
                {
                    "name": "lab_list",
                    "description": "List all jobs and stages in a GitLab CI/CD pipeline with their images, dependencies, and configuration.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "file": {
                                "type": "string",
                                "description": "Path to .gitlab-ci.yml",
                                "default": ".gitlab-ci.yml"
                            }
                        }
                    }
                },
                {
                    "name": "lab_dry_run",
                    "description": "Show the execution plan for a pipeline without actually running containers. Shows stages, jobs, images, dependencies, and secret availability.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "file": {
                                "type": "string",
                                "description": "Path to .gitlab-ci.yml",
                                "default": ".gitlab-ci.yml"
                            },
                            "job": {
                                "type": "string",
                                "description": "Specific job to plan (optional, plans all if omitted)"
                            }
                        }
                    }
                },
                {
                    "name": "lab_secrets_check",
                    "description": "Check which CI/CD secrets are available vs missing for the pipeline. Shows which jobs need which secrets.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "file": {
                                "type": "string",
                                "description": "Path to .gitlab-ci.yml",
                                "default": ".gitlab-ci.yml"
                            }
                        }
                    }
                },
                {
                    "name": "lab_graph",
                    "description": "Show the job dependency graph of a pipeline. Lists which jobs depend on which others.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "file": {
                                "type": "string",
                                "description": "Path to .gitlab-ci.yml",
                                "default": ".gitlab-ci.yml"
                            }
                        }
                    }
                },
                {
                    "name": "lab_secrets_pull",
                    "description": "Pull CI/CD secrets from GitLab project and group variables via glab. Saves to .lab/secrets.env. Reports protected/hidden/masked variable status.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {}
                    }
                },
                {
                    "name": "lab_secrets_init",
                    "description": "Generate .lab/secrets.env.example template from pipeline variable references. Creates the secrets directory structure.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "file": {
                                "type": "string",
                                "description": "Path to .gitlab-ci.yml",
                                "default": ".gitlab-ci.yml"
                            }
                        }
                    }
                },
                {
                    "name": "lab_explain_job",
                    "description": "Explain what a specific job does — its image, scripts, dependencies, services, artifacts, rules, and variables. Returns structured job configuration.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "file": {
                                "type": "string",
                                "description": "Path to .gitlab-ci.yml",
                                "default": ".gitlab-ci.yml"
                            },
                            "job": {
                                "type": "string",
                                "description": "Job name to explain"
                            }
                        },
                        "required": ["job"]
                    }
                },
                {
                    "name": "lab_suggest_fix",
                    "description": "Given an analyze finding rule name, return the specific YAML fix to apply. Use after lab_analyze to get actionable code changes.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "file": {
                                "type": "string",
                                "description": "Path to .gitlab-ci.yml",
                                "default": ".gitlab-ci.yml"
                            },
                            "rule": {
                                "type": "string",
                                "description": "Rule name from lab_analyze findings (e.g., 'missing-cache', 'unpinned-image-tag')"
                            },
                            "job": {
                                "type": "string",
                                "description": "Job name the finding applies to (optional)"
                            }
                        },
                        "required": ["rule"]
                    }
                },
                {
                    "name": "lab_run_job",
                    "description": "Run a specific job from the pipeline locally in Docker. Returns the job output and exit status. Use with caution — this actually executes commands in containers.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "file": {
                                "type": "string",
                                "description": "Path to .gitlab-ci.yml",
                                "default": ".gitlab-ci.yml"
                            },
                            "job": {
                                "type": "string",
                                "description": "Job name to run"
                            }
                        },
                        "required": ["job"]
                    }
                },
                {
                    "name": "lab_variable_expand",
                    "description": "Expand a string containing $VAR or ${VAR} references using the pipeline's variable context. Shows what a value resolves to.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "file": {
                                "type": "string",
                                "description": "Path to .gitlab-ci.yml",
                                "default": ".gitlab-ci.yml"
                            },
                            "expression": {
                                "type": "string",
                                "description": "String to expand (e.g., 'node:${NODE_VERSION}')"
                            }
                        },
                        "required": ["expression"]
                    }
                }
            ]
        }
    })
}

fn handle_tools_call(id: &Value, request: &Value) -> Value {
    let params = request.get("params").cloned().unwrap_or(json!({}));
    let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let file = args
        .get("file")
        .and_then(|f| f.as_str())
        .unwrap_or(".gitlab-ci.yml");
    let file_path = PathBuf::from(file);

    let result = match tool_name {
        "lab_analyze" => tool_analyze(&file_path),
        "lab_validate" => tool_validate(&file_path),
        "lab_list" => tool_list(&file_path),
        "lab_dry_run" => tool_dry_run(&file_path, &args),
        "lab_secrets_check" => tool_secrets_check(&file_path),
        "lab_graph" => tool_graph(&file_path),
        "lab_secrets_pull" => tool_secrets_pull(),
        "lab_secrets_init" => tool_secrets_init(&file_path),
        "lab_explain_job" => tool_explain_job(&file_path, &args),
        "lab_suggest_fix" => tool_suggest_fix(&file_path, &args),
        "lab_run_job" => tool_run_job(&file_path, &args),
        "lab_variable_expand" => tool_variable_expand(&file_path, &args),
        _ => Err(format!("Unknown tool: {tool_name}")),
    };

    match result {
        Ok(content) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{
                    "type": "text",
                    "text": content
                }]
            }
        }),
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{
                    "type": "text",
                    "text": format!("Error: {e}")
                }],
                "isError": true
            }
        }),
    }
}

// ============================================================
// Tool implementations
// ============================================================

fn tool_analyze(file: &PathBuf) -> Result<String, String> {
    let pipeline = parse_pipeline(file).map_err(|e| e.to_string())?;
    let findings = analyze::analyze(&pipeline);
    serde_json::to_string_pretty(&findings).map_err(|e| e.to_string())
}

fn tool_validate(file: &PathBuf) -> Result<String, String> {
    match parse_pipeline(file) {
        Ok(pipeline) => Ok(json!({
            "valid": true,
            "stages": pipeline.stages.len(),
            "jobs": pipeline.jobs.len(),
            "stage_names": pipeline.stages,
            "job_names": pipeline.jobs.keys().collect::<Vec<_>>()
        })
        .to_string()),
        Err(e) => Ok(json!({
            "valid": false,
            "error": e.to_string()
        })
        .to_string()),
    }
}

fn tool_list(file: &PathBuf) -> Result<String, String> {
    let pipeline = parse_pipeline(file).map_err(|e| e.to_string())?;

    let mut stages = Vec::new();
    for stage_name in &pipeline.stages {
        let jobs: Vec<Value> = pipeline
            .jobs
            .iter()
            .filter(|(_, job)| job.stage == *stage_name)
            .map(|(name, job)| {
                json!({
                    "name": name,
                    "image": job.image.as_ref().map(|i| i.name()),
                    "needs": job.needs.as_ref().map(|n|
                        n.iter().map(|nd| nd.job_name()).collect::<Vec<_>>()
                    ),
                    "when": format!("{:?}", job.when),
                    "allow_failure": job.allow_failure.is_allowed(1),
                    "services": job.services.as_ref().map(|s|
                        s.iter().map(|svc| svc.image_name()).collect::<Vec<_>>()
                    ),
                })
            })
            .collect();

        if !jobs.is_empty() {
            stages.push(json!({
                "stage": stage_name,
                "jobs": jobs
            }));
        }
    }

    serde_json::to_string_pretty(&json!({"stages": stages})).map_err(|e| e.to_string())
}

fn tool_dry_run(file: &PathBuf, args: &Value) -> Result<String, String> {
    let pipeline = parse_pipeline(file).map_err(|e| e.to_string())?;

    let workdir = std::env::current_dir().map_err(|e| e.to_string())?;
    let config = lab_core::config::Config {
        workdir: workdir.clone(),
        ..Default::default()
    };
    let predefined =
        lab_core::model::variables::predefined_variables(&config, "", "").unwrap_or_default();
    let secret_vars = secrets::load_secrets_file(&workdir).unwrap_or_default();
    let global_vars = merge_variables(&[&predefined, &pipeline.variables, &secret_vars]);

    let job_filter = args.get("job").and_then(|j| j.as_str());

    let plan = build_plan(
        &pipeline.stages,
        &pipeline.jobs,
        &global_vars,
        job_filter,
        None,
    )
    .map_err(|e| e.to_string())?;

    let mut stages_output = Vec::new();
    for stage in &plan.stages {
        let jobs: Vec<Value> = stage
            .jobs
            .iter()
            .map(|pj| {
                let image = pj
                    .job
                    .image
                    .as_ref()
                    .map(|i| i.name())
                    .unwrap_or("(default)");
                json!({
                    "name": pj.name,
                    "image": image,
                    "script_commands": pj.job.script.len(),
                    "timeout": pj.job.timeout.map(|d| d.as_secs()),
                    "needs": pj.job.needs.as_ref().map(|n|
                        n.iter().map(|nd| nd.job_name()).collect::<Vec<_>>()
                    ),
                })
            })
            .collect();

        stages_output.push(json!({
            "stage": stage.name,
            "jobs": jobs
        }));
    }

    let total_jobs: usize = plan.stages.iter().map(|s| s.jobs.len()).sum();
    serde_json::to_string_pretty(&json!({
        "total_jobs": total_jobs,
        "total_stages": plan.stages.len(),
        "secrets_loaded": secret_vars.len(),
        "stages": stages_output
    }))
    .map_err(|e| e.to_string())
}

fn tool_secrets_check(file: &PathBuf) -> Result<String, String> {
    let pipeline = parse_pipeline(file).map_err(|e| e.to_string())?;
    let workdir = std::env::current_dir().map_err(|e| e.to_string())?;
    let available = secrets::load_secrets_file(&workdir).unwrap_or_default();
    let all_vars = merge_variables(&[&pipeline.variables, &available]);

    let missing = secrets::check_secrets(&pipeline, &all_vars);

    let available_list: Vec<Value> = available
        .keys()
        .map(|k| json!({"name": k, "status": "available"}))
        .collect();

    let missing_list: Vec<Value> = missing
        .iter()
        .map(|m| {
            json!({
                "name": m.name,
                "status": "missing",
                "used_in_jobs": m.used_in_jobs
            })
        })
        .collect();

    serde_json::to_string_pretty(&json!({
        "available": available_list,
        "missing": missing_list,
        "total_available": available.len(),
        "total_missing": missing.len()
    }))
    .map_err(|e| e.to_string())
}

fn tool_graph(file: &PathBuf) -> Result<String, String> {
    let pipeline = parse_pipeline(file).map_err(|e| e.to_string())?;

    let mut edges = Vec::new();
    let mut nodes = Vec::new();

    for (name, job) in &pipeline.jobs {
        nodes.push(json!({
            "name": name,
            "stage": job.stage
        }));

        if let Some(needs) = &job.needs {
            for need in needs {
                edges.push(json!({
                    "from": need.job_name(),
                    "to": name
                }));
            }
        }
    }

    serde_json::to_string_pretty(&json!({
        "nodes": nodes,
        "edges": edges
    }))
    .map_err(|e| e.to_string())
}

fn tool_secrets_pull() -> Result<String, String> {
    let workdir = std::env::current_dir().map_err(|e| e.to_string())?;
    let result = secrets::pull_secrets_full(&workdir).map_err(|e| e.to_string())?;

    secrets::save_secrets_file(&workdir, &result.included).map_err(|e| e.to_string())?;

    serde_json::to_string_pretty(&json!({
        "saved": result.included.len(),
        "masked_count": result.masked_keys.len(),
        "skipped_protected": result.skipped_protected,
        "skipped_hidden": result.skipped_hidden,
        "scoped_variables": result.skipped_scope.iter()
            .map(|(k, s)| json!({"key": k, "scope": s}))
            .collect::<Vec<_>>()
    }))
    .map_err(|e| e.to_string())
}

fn tool_secrets_init(file: &PathBuf) -> Result<String, String> {
    let pipeline = parse_pipeline(file).map_err(|e| e.to_string())?;
    let workdir = std::env::current_dir().map_err(|e| e.to_string())?;
    secrets::generate_secrets_example(&pipeline, &workdir).map_err(|e| e.to_string())?;
    Ok(json!({
        "created": [".lab/secrets.env.example", ".lab/secrets.env"],
        "message": "Secrets template generated. Run lab_secrets_pull to fetch from GitLab, or fill in values manually."
    })
    .to_string())
}

fn tool_explain_job(file: &PathBuf, args: &Value) -> Result<String, String> {
    let pipeline = parse_pipeline(file).map_err(|e| e.to_string())?;
    let job_name = args
        .get("job")
        .and_then(|j| j.as_str())
        .ok_or("missing 'job' parameter")?;

    let job = pipeline
        .jobs
        .get(job_name)
        .ok_or_else(|| format!("job '{job_name}' not found"))?;

    let explanation = json!({
        "name": job_name,
        "stage": job.stage,
        "image": job.image.as_ref().map(|i| i.name()),
        "when": format!("{:?}", job.when),
        "allow_failure": job.allow_failure.is_allowed(1),
        "interruptible": job.interruptible,
        "timeout_seconds": job.timeout.map(|d| d.as_secs()),
        "retry_max": job.retry.as_ref().map(|r| r.max_retries()),
        "script": job.script,
        "before_script": job.before_script,
        "after_script": job.after_script,
        "variables": job.variables.iter()
            .map(|(k, v)| json!({"name": k, "value": v.value()}))
            .collect::<Vec<_>>(),
        "needs": job.needs.as_ref().map(|n|
            n.iter().map(|nd| json!({
                "job": nd.job_name(),
                "artifacts": nd.wants_artifacts(),
                "optional": nd.is_optional()
            })).collect::<Vec<_>>()
        ),
        "services": job.services.as_ref().map(|s|
            s.iter().map(|svc| json!({
                "image": svc.image_name(),
                "hostname": svc.hostname()
            })).collect::<Vec<_>>()
        ),
        "artifacts": job.artifacts.as_ref().map(|a| json!({
            "paths": a.paths,
            "exclude": a.exclude,
            "expire_in": a.expire_in,
            "when": format!("{:?}", a.when_upload),
        })),
        "cache": job.cache.as_ref().map(|c|
            c.iter().map(|cache| json!({
                "paths": cache.paths,
                "policy": format!("{:?}", cache.policy),
            })).collect::<Vec<_>>()
        ),
        "rules": job.rules.as_ref().map(|r|
            r.iter().map(|rule| json!({
                "if": rule.if_expr,
                "when": rule.when.map(|w| format!("{w:?}")),
                "has_changes": rule.changes.is_some(),
                "has_exists": rule.exists.is_some(),
            })).collect::<Vec<_>>()
        ),
        "coverage_regex": job.coverage,
        "resource_group": job.resource_group,
        "tags": job.tags,
        "trigger": job.trigger.is_some(),
    });

    serde_json::to_string_pretty(&explanation).map_err(|e| e.to_string())
}

fn tool_suggest_fix(file: &PathBuf, args: &Value) -> Result<String, String> {
    let pipeline = parse_pipeline(file).map_err(|e| e.to_string())?;
    let rule = args
        .get("rule")
        .and_then(|r| r.as_str())
        .ok_or("missing 'rule' parameter")?;
    let job_name = args.get("job").and_then(|j| j.as_str());

    let fix = match rule {
        "missing-workflow-rules" => json!({
            "rule": rule,
            "yaml_fix": "workflow:\n  rules:\n    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH\n    - if: $CI_PIPELINE_SOURCE == \"merge_request_event\"\n    - if: $CI_COMMIT_TAG",
            "explanation": "Add workflow:rules to prevent duplicate pipelines. This runs pipelines for default branch, MRs, and tags."
        }),
        "unpinned-image-tag" => {
            let image = job_name
                .and_then(|n| pipeline.jobs.get(n))
                .and_then(|j| j.image.as_ref())
                .map(|i| i.name().to_string())
                .unwrap_or_default();
            let base = image.split(':').next().unwrap_or(&image);
            json!({
                "rule": rule,
                "job": job_name,
                "yaml_fix": format!("image: {base}:20-alpine  # Pin to specific version"),
                "explanation": "Pin Docker image to a specific tag. Use alpine variants for smaller images. Consider using SHA256 digest for maximum reproducibility."
            })
        }
        "missing-cache" => {
            let job = job_name.and_then(|n| pipeline.jobs.get(n));
            let script_text = job
                .map(|j| j.script.join(" "))
                .unwrap_or_default()
                .to_lowercase();
            let (key_file, paths) = if script_text.contains("npm") || script_text.contains("pnpm") {
                ("package-lock.json", "node_modules/\n      - .pnpm-store")
            } else if script_text.contains("pip") {
                ("requirements.txt", ".venv/")
            } else if script_text.contains("bundle") {
                ("Gemfile.lock", "vendor/bundle/")
            } else if script_text.contains("cargo") {
                ("Cargo.lock", "target/")
            } else {
                ("lockfile", "deps/")
            };
            json!({
                "rule": rule,
                "job": job_name,
                "yaml_fix": format!("cache:\n  key:\n    files:\n      - {key_file}\n  paths:\n    - {paths}\n  policy: pull-push"),
                "explanation": format!("Cache dependencies using {key_file} as key. This avoids re-downloading on every run.")
            })
        }
        "missing-timeout" => json!({
            "rule": rule,
            "job": job_name,
            "yaml_fix": "timeout: 30m",
            "explanation": "Set a timeout to prevent stuck jobs from wasting resources."
        }),
        "missing-retry" => json!({
            "rule": rule,
            "job": job_name,
            "yaml_fix": "retry:\n  max: 2\n  when:\n    - runner_system_failure\n    - stuck_or_timeout_failure",
            "explanation": "Retry on infrastructure failures. Don't retry on script_failure to catch real bugs."
        }),
        "missing-interruptible" => json!({
            "rule": rule,
            "job": job_name,
            "yaml_fix": "interruptible: true",
            "explanation": "Mark test/lint jobs as interruptible so they're canceled when a new push supersedes them."
        }),
        "deploy-without-rules" => json!({
            "rule": rule,
            "job": job_name,
            "yaml_fix": "rules:\n  - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH\n    when: manual\n    allow_failure: false",
            "explanation": "CRITICAL: Restrict deploys to the default branch with manual trigger."
        }),
        "deploy-allow-failure" => json!({
            "rule": rule,
            "job": job_name,
            "yaml_fix": "allow_failure: false",
            "explanation": "Deploy failures should block the pipeline to prevent silent broken deployments."
        }),
        "artifact-no-expiry" => json!({
            "rule": rule,
            "job": job_name,
            "yaml_fix": "artifacts:\n  expire_in: 1 week",
            "explanation": "Set an expiry to automatically clean up old artifacts and save storage."
        }),
        "missing-coverage" => json!({
            "rule": rule,
            "job": job_name,
            "yaml_fix": "coverage: '/Coverage:\\s*\\d+\\.?\\d*%/'",
            "explanation": "Extract coverage percentage from test output. Adjust regex to match your test framework's format."
        }),
        "dind-without-tls" => json!({
            "rule": rule,
            "job": job_name,
            "yaml_fix": "variables:\n  DOCKER_TLS_VERIFY: '1'\n  DOCKER_CERT_PATH: /certs/client\n  DOCKER_HOST: tcp://docker:2376",
            "explanation": "Enable TLS for Docker-in-Docker to prevent man-in-the-middle attacks."
        }),
        "large-base-image" => json!({
            "rule": rule,
            "job": job_name,
            "yaml_fix": "image: node:20-alpine  # or python:3.12-slim",
            "explanation": "Alpine/slim images are 5-10x smaller, have fewer vulnerabilities, and pull faster."
        }),
        "hardcoded-secret" => json!({
            "rule": rule,
            "job": job_name,
            "yaml_fix": "# Move to GitLab CI/CD Variables:\n# Settings > CI/CD > Variables\n# Mark as: masked=true, protected=true",
            "explanation": "CRITICAL: Never hardcode secrets in .gitlab-ci.yml. Use CI/CD variables with masked and protected flags."
        }),
        _ => json!({
            "rule": rule,
            "error": format!("No fix suggestion available for rule '{rule}'")
        }),
    };

    serde_json::to_string_pretty(&fix).map_err(|e| e.to_string())
}

fn tool_run_job(file: &PathBuf, args: &Value) -> Result<String, String> {
    let job_name = args
        .get("job")
        .and_then(|j| j.as_str())
        .ok_or("missing 'job' parameter")?;

    // Run via subprocess to capture output
    let output = std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
        .args([
            "run",
            job_name,
            "-f",
            file.to_str().unwrap_or(".gitlab-ci.yml"),
            "--no-preflight",
        ])
        .output()
        .map_err(|e| format!("failed to run job: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    Ok(json!({
        "job": job_name,
        "exit_code": output.status.code(),
        "success": output.status.success(),
        "stdout": stdout.chars().take(10000).collect::<String>(),
        "stderr": stderr.chars().take(5000).collect::<String>(),
    })
    .to_string())
}

fn tool_variable_expand(file: &PathBuf, args: &Value) -> Result<String, String> {
    let expression = args
        .get("expression")
        .and_then(|e| e.as_str())
        .ok_or("missing 'expression' parameter")?;

    let pipeline = parse_pipeline(file).map_err(|e| e.to_string())?;
    let workdir = std::env::current_dir().map_err(|e| e.to_string())?;
    let config = lab_core::config::Config {
        workdir: workdir.clone(),
        ..Default::default()
    };
    let predefined =
        lab_core::model::variables::predefined_variables(&config, "", "").unwrap_or_default();
    let secret_vars = secrets::load_secrets_file(&workdir).unwrap_or_default();
    let all_vars = merge_variables(&[&predefined, &pipeline.variables, &secret_vars]);

    let expanded = lab_core::model::variables::expand_variables(expression, &all_vars);

    Ok(json!({
        "input": expression,
        "expanded": expanded,
        "variables_used": all_vars.len()
    })
    .to_string())
}
