# GitLab CI/CD YAML Keyword Reference

> Based on the official GitLab CI/CD YAML specification:
> **Source**: [`gitlab.com/gitlab-org/gitlab/-/blob/master/doc/ci/yaml/_index.md`](https://gitlab.com/gitlab-org/gitlab/-/blob/master/doc/ci/yaml/_index.md)
> **Online docs**: [`docs.gitlab.com/ci/yaml`](https://docs.gitlab.com/ci/yaml/)

This document maps every GitLab CI/CD YAML keyword to its implementation status in `lab`.

## Status Legend

| Icon | Meaning |
|------|---------|
| **Implemented** | Fully working in local execution |
| **Parsed** | Accepted by YAML parser (no parse errors) but not actively used in execution |
| **N/A** | Not applicable for local execution (requires GitLab server, API, or registry) |

---

## Global Keywords

| Keyword | Description | Status | Source |
|---------|-------------|--------|--------|
| `stages` | Names and order of pipeline stages | **Implemented** | `parser/schema.rs` |
| `variables` | Default CI/CD variables for all jobs | **Implemented** | `model/variables.rs` |
| `default` | Custom default values for job keywords | **Implemented** | `parser/schema.rs` → `apply_defaults()` |
| `default:image` | Default Docker image | **Implemented** | |
| `default:before_script` | Default before_script | **Implemented** | |
| `default:after_script` | Default after_script | **Implemented** | |
| `default:services` | Default services | **Implemented** | |
| `default:cache` | Default cache config | **Implemented** | |
| `default:artifacts` | Default artifacts config | **Implemented** | |
| `default:retry` | Default retry config | **Implemented** | |
| `default:timeout` | Default timeout | **Implemented** | |
| `default:interruptible` | Default interruptible flag | **Implemented** | |
| `default:tags` | Default runner tags | **Implemented** | |
| `default:hooks` | Default hooks | **Parsed** | Accepted by parser, not executed |
| `default:id_tokens` | Default ID tokens | N/A | |
| `include` | Import config from other YAML files | **Implemented** (partial) | `parser/resolver.rs` |
| `include:local` | Include local file | **Implemented** | `parser/resolver.rs` |
| `include:remote` | Include file from URL | **Implemented** | `parser/resolver.rs` (via curl) |
| `include:project` | Include file from another project | **Implemented** | `parser/resolver.rs` → `glab api` fetch |
| `include:template` | Include GitLab-provided template | **Implemented** | `parser/resolver.rs` (fetches from GitLab repo) | |
| `include:component` | Include CI/CD component | N/A | Requires component registry |
| `include:rules` | Conditional include | **Implemented** | `parser/resolver.rs` → `filter_includes_by_rules()` | |
| `include:inputs` | Pass inputs to included config | N/A | Requires `spec:inputs` interpolation |
| `workflow` | Control pipeline types | **Implemented** (partial) | `main.rs` |
| `workflow:rules` | Pipeline-level rules | **Implemented** | `main.rs` → workflow gate |
| `workflow:name` | Pipeline name | **Implemented** | `main.rs` → displayed with variable expansion |
| `workflow:auto_cancel` | Auto-cancel settings | **Parsed** | `parser/schema.rs` → `AutoCancelConfig` |
| `workflow:auto_cancel:on_new_commit` | Cancel on new commit | **Parsed** | Not enforced locally |
| `workflow:auto_cancel:on_job_failure` | Cancel on job failure | **Parsed** | Not enforced locally |
| `spec` | Specification for external configs | N/A | Server-side interpolation (`$[[ inputs.x ]]`) |
| `spec:inputs` | Input parameters | N/A | Server-side interpolation |
| `spec:description` | Config description | N/A | Metadata only |

---

## Job Keywords — Core

| Keyword | Description | Status | Source |
|---------|-------------|--------|--------|
| `script` | Shell commands to execute (required) | **Implemented** | `runner/script.rs` |
| `before_script` | Commands before main script | **Implemented** | `runner/script.rs` |
| `after_script` | Commands after main script (always runs) | **Implemented** | `runner/script.rs` |
| `image` | Docker image for the job | **Implemented** | `model/job.rs` → `ImageConfig` |
| `image:name` | Image name | **Implemented** | |
| `image:entrypoint` | Override image entrypoint | **Parsed** | Not wired to container creation |
| `stage` | Pipeline stage assignment | **Implemented** | `planner/dag.rs` |
| `extends` | Inherit from other jobs | **Implemented** | `parser/yaml.rs` → `resolve_extends()` |
| `variables` | Job-specific CI/CD variables | **Implemented** | `model/variables.rs` |
| `when` | When to run the job | **Implemented** | `on_success`, `on_failure`, `always`, `manual`, `delayed`, `never` |
| `tags` | Runner selection tags | **Parsed** | Ignored locally |
| `interruptible` | Job can be cancelled when redundant | **Parsed** | Not enforced |
| `inherit` | Control global defaults inheritance | **Parsed** | `model/job.rs` → `InheritConfig` |
| `inherit:default` | Select which defaults to inherit | **Parsed** | Not enforced in `apply_defaults` |
| `inherit:variables` | Select which global vars to inherit | **Parsed** | Not enforced |

---

## Job Keywords — Execution

| Keyword | Description | Status | Source |
|---------|-------------|--------|--------|
| `services` | Docker service containers (sidecars) | **Implemented** | `docker/service.rs` |
| `services:name` | Service image name | **Implemented** | |
| `services:alias` | Service hostname alias | **Implemented** | |
| `services:entrypoint` | Override service entrypoint | **Implemented** | |
| `services:command` | Override service command | **Implemented** | |
| `services:variables` | Service-specific variables | **Implemented** | |
| `needs` | DAG dependencies (run before stage) | **Implemented** | `planner/dag.rs` |
| `needs:job` | Depend on specific job | **Implemented** | |
| `needs:artifacts` | Download artifacts from dep | **Implemented** | `runner/script.rs` |
| `needs:optional` | Optional dependency | **Implemented** | `planner/dag.rs` → skips missing optional deps |
| `needs:pipeline` | Cross-pipeline dependency | N/A | Requires GitLab API |
| `dependencies` | Restrict artifact sources | **Implemented** | `runner/script.rs` |
| `parallel` | Parallel job instances | **Implemented** | `planner/dag.rs` → `expand_matrix()` |
| `parallel:matrix` | Matrix of variable combinations | **Implemented** | |
| `timeout` | Custom job timeout | **Implemented** | `runner/script.rs` → `tokio::timeout` |
| `retry` | Auto-retry on failure | **Implemented** | `runner/script.rs` |
| `retry:max` | Maximum retry count | **Implemented** | |
| `retry:when` | Retry conditions | **Implemented** | `runner/script.rs` → filters by failure type |
| `allow_failure` | Allow job to fail | **Implemented** | `runner/runner.rs` |
| `allow_failure:exit_codes` | Specific exit codes allowed | **Implemented** | `model/job.rs` → `AllowFailure` |
| `resource_group` | Limit job concurrency | **Implemented** | `runner/runner.rs` → lock file mutex |
| `start_in` | Delay job execution | **Implemented** | `runner/runner.rs` → `parse_delay()` |
| `run` | Runner execution config | N/A | GitLab Runner 17+ internal |

---

## Job Keywords — Rules & Conditions

| Keyword | Description | Status | Source |
|---------|-------------|--------|--------|
| `rules` | Conditional job execution | **Implemented** | `model/rules.rs` |
| `rules:if` | Variable expression condition | **Implemented** | Full parser: `==`, `!=`, `=~`, `!~`, `&&`, `\|\|`, parens |
| `rules:changes` | File change condition | **Implemented** | `model/rules.rs` → `check_git_changes()` |
| `rules:changes:paths` | Glob patterns for changed files | **Implemented** | |
| `rules:changes:compare_to` | Comparison ref | **Implemented** | `model/rules.rs` → used as git diff ref |
| `rules:exists` | File existence condition | **Implemented** | `model/rules.rs` → `check_files_exist()` |
| `rules:when` | Override `when` on match | **Implemented** | |
| `rules:allow_failure` | Override `allow_failure` on match | **Implemented** | |
| `rules:variables` | Override variables on match | **Implemented** | `planner/dag.rs` → merged into job vars |

---

## Job Keywords — Artifacts & Cache

| Keyword | Description | Status | Source |
|---------|-------------|--------|--------|
| `artifacts` | Files to attach to job | **Implemented** | `artifacts.rs` |
| `artifacts:paths` | File/dir paths to collect | **Implemented** | |
| `artifacts:exclude` | Paths to exclude | **Implemented** | `artifacts.rs` → `remove_excluded_artifacts()` |
| `artifacts:expire_in` | Expiration duration | **Parsed** | Not enforced (local storage) |
| `artifacts:name` | Archive name | **Parsed** | Not used |
| `artifacts:when` | When to upload (success/failure/always) | **Implemented** | `runner/script.rs` → respects on_success/on_failure/always |
| `artifacts:untracked` | Include untracked files | **Implemented** | `artifacts.rs` → `git ls-files --others` |
| `artifacts:expose_as` | Expose in MR UI | N/A | |
| `artifacts:public` | Public access | N/A | |
| `artifacts:access` | Access control | N/A | |
| `artifacts:reports` | Test/coverage/security reports | **Parsed** | `model/job.rs` → `ArtifactReports` (junit, sast, codequality, etc.) | |
| `cache` | Files cached between runs | **Implemented** | `cache.rs` |
| `cache:paths` | Paths to cache | **Implemented** | |
| `cache:key` | Cache key (string or files) | **Implemented** | |
| `cache:key:files` | Hash file contents for key | **Implemented** | |
| `cache:key:prefix` | Key prefix | **Implemented** | |
| `cache:key:files_commits` | Include commit SHAs | **Implemented** | `cache.rs` → `git log -1 --format=%H -- <file>` |
| `cache:untracked` | Cache untracked files | **Parsed** | Not used |
| `cache:unprotect` | Allow unprotected branches | N/A | |
| `cache:when` | When to upload cache | **Implemented** | `cache.rs` → `CacheWhen` (on_success/on_failure/always) |
| `cache:policy` | pull / push / pull-push | **Implemented** | |
| `cache:fallback_keys` | Fallback cache keys | **Implemented** | |

---

## Job Keywords — Advanced

| Keyword | Description | Status |
|---------|-------------|--------|
| `environment` | Deployment environment | N/A (local-only) |
| `environment:name` | Environment name | N/A |
| `environment:url` | Environment URL | N/A |
| `trigger` | Downstream pipeline trigger | **Implemented** (partial) | `model/job.rs` → `TriggerConfig` |
| `trigger:include` | Child pipeline config | **Implemented** | `runner/runner.rs` → `run_child_pipeline()` |
| `trigger:project` | Multi-project pipeline | N/A | Requires GitLab API |
| `trigger:strategy` | Trigger strategy | **Parsed** | `model/job.rs` |
| `release` | Create a release | N/A (local-only) |
| `coverage` | Code coverage regex | **Implemented** | `runner/output.rs` → `extract_coverage()` |
| `secrets` | CI/CD secrets from provider | N/A (local-only) |
| `id_tokens` | OIDC tokens | N/A (local-only) |
| `identity` | Third-party identity federation | N/A (local-only) |
| `pages` | GitLab Pages deployment | N/A (local-only) |
| `dast_configuration` | DAST profiles | N/A (local-only) |
| `manual_confirmation` | Manual job confirmation message | **Implemented** | `runner/runner.rs` (stdin prompt) |
| `hooks` | Job lifecycle hooks | **Parsed** | Accepted by parser, not executed |

---

## YAML Optimization Features

| Feature | Description | Status | Source |
|---------|-------------|--------|--------|
| YAML anchors (`&` / `*`) | Reuse YAML nodes | **Implemented** | serde_yaml native |
| Merge keys (`<<:`) | Merge mapping nodes | **Implemented** | `parser/yaml.rs` → `resolve_merge_keys()` |
| `extends` deep merge | Template inheritance | **Implemented** | `parser/yaml.rs` → `resolve_extends()` |
| `!reference` tags | Reference job sections | **Implemented** | `parser/yaml.rs` → `resolve_reference_tags()` |

---

## Predefined CI/CD Variables

Simulated by `lab` for local execution. See [`model/variables.rs` → `predefined_variables()`](../crates/lab-core/src/model/variables.rs).

| Variable | Simulated |
|----------|-----------|
| `CI` | `true` |
| `GITLAB_CI` | `true` |
| `CI_LOCAL` | `true` (lab-specific) |
| `CI_SERVER` | `yes` |
| `CI_PROJECT_NAME` | From working directory |
| `CI_PROJECT_DIR` | Canonical working directory |
| `CI_PROJECT_PATH` | `local/<project_name>` |
| `CI_PROJECT_NAMESPACE` | `local` |
| `CI_COMMIT_SHA` | From `git rev-parse HEAD` |
| `CI_COMMIT_SHORT_SHA` | First 8 chars of SHA |
| `CI_COMMIT_BRANCH` | From `git rev-parse --abbrev-ref HEAD` |
| `CI_COMMIT_REF_NAME` | Same as branch |
| `CI_COMMIT_MESSAGE` | From `git log -1 --format=%B` |
| `CI_PIPELINE_ID` | `0` |
| `CI_PIPELINE_SOURCE` | `local` |
| `CI_JOB_NAME` | Current job name |
| `CI_JOB_STAGE` | Current stage name |
| `CI_JOB_ID` | `0` |
| `CI_RUNNER_ID` | `0` |
| `CI_RUNNER_DESCRIPTION` | `lab-local` |
| `CI_DEFAULT_BRANCH` | `main` |

> Full list of GitLab predefined variables: [docs.gitlab.com/ci/variables/predefined_variables](https://docs.gitlab.com/ci/variables/predefined_variables/)
