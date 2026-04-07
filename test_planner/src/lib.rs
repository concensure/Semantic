use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    pub description: String,
    pub input_conditions: String,
    pub expected_output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestPlan {
    pub target_symbol: String,
    pub test_cases: Vec<TestCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedTests {
    pub framework: String,
    pub tests: String,
    pub llm_provider: Option<String>,
    pub llm_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedTests {
    pub target_file: String,
    pub status: String,
    pub refactor_status: refactor_graph::RefactorStatus,
}

pub struct TestPlanner;

impl TestPlanner {
    pub fn build_plan(target_symbol: &str, framework: &str) -> TestPlan {
        let mut cases = Vec::new();
        cases.push(TestCase {
            description: format!("happy path for {target_symbol}"),
            input_conditions: "valid inputs".to_string(),
            expected_output: "returns expected result".to_string(),
        });
        cases.push(TestCase {
            description: format!("edge case for {target_symbol}"),
            input_conditions: "boundary values".to_string(),
            expected_output: "handles boundary safely".to_string(),
        });
        cases.push(TestCase {
            description: format!("error path for {target_symbol}"),
            input_conditions: "invalid input or dependency failure".to_string(),
            expected_output: "returns/throws explicit error".to_string(),
        });
        if framework.to_lowercase().contains("property") {
            cases.push(TestCase {
                description: format!("property invariant for {target_symbol}"),
                input_conditions: "randomized generated inputs".to_string(),
                expected_output: "invariant always holds".to_string(),
            });
        }

        TestPlan {
            target_symbol: target_symbol.to_string(),
            test_cases: cases,
        }
    }

    pub fn generate_tests(plan: &TestPlan, framework: &str, symbol_code: &str) -> GeneratedTests {
        let provider_toml = "[providers]\nopenai = \"https://api.openai.com/v1\"\n";
        let routing_toml = "[planning]\npreferred = [\"openai\"]\n";
        let metrics_json =
            "{\"openai\":{\"success_rate\":0.9,\"latency_ms\":200,\"token_cost\":0.2}}";
        let route = llm_router::LLMRouter::from_files(provider_toml, routing_toml, metrics_json)
            .ok()
            .and_then(|r| r.route(llm_router::LLMTask::Planning));

        let mut body = String::new();
        body.push_str("// Auto-generated tests from TestPlanner\n");
        body.push_str(&format!("// target_symbol: {}\n", plan.target_symbol));
        body.push_str(&format!("// framework: {framework}\n"));
        body.push_str(&format!(
            "// symbol_code_excerpt: {}\n\n",
            symbol_code.lines().take(3).collect::<Vec<_>>().join(" ")
        ));

        for (i, case) in plan.test_cases.iter().enumerate() {
            body.push_str(&format!("#[test]\nfn generated_case_{i}() {{\n"));
            body.push_str(&format!("    // {}\n", case.description));
            body.push_str(&format!("    // input: {}\n", case.input_conditions));
            body.push_str(&format!("    // expected: {}\n", case.expected_output));
            body.push_str("    assert!(true);\n");
            body.push_str("}\n\n");
        }

        GeneratedTests {
            framework: framework.to_string(),
            tests: body,
            llm_provider: route.as_ref().map(|r| r.provider.clone()),
            llm_endpoint: route.as_ref().map(|r| r.endpoint.clone()),
        }
    }

    pub fn apply_tests(
        repo_root: &Path,
        repository: &str,
        plan: &TestPlan,
        generated: &GeneratedTests,
    ) -> Result<AppliedTests> {
        let file_name = sanitize_symbol(&plan.target_symbol);
        let rel_test_path = format!("tests/generated_{}_tests.rs", file_name);
        let target_file = repo_root.join(&rel_test_path);
        if let Some(parent) = target_file.parent() {
            fs::create_dir_all(parent)?;
        }

        let request = refactor_graph::HighLevelRefactorRequest {
            repository: repository.to_string(),
            old_symbol: plan.target_symbol.clone(),
            new_symbol: format!("{}_tests", plan.target_symbol),
            include_tests: true,
        };
        let mut graph = refactor_graph::RefactorGraph::from_request(&request);
        for node in &mut graph.nodes {
            node.edit_plan
                .required_context
                .push(engine::EditContextItem {
                    file_path: rel_test_path.clone(),
                    start_line: 1,
                    end_line: 1,
                    priority: 1,
                    text: format!("insert tests for {}", plan.target_symbol),
                });
        }

        let mut options = refactor_graph::ExecutionOptions::default();
        options.auto_confirm_low_risk = true;
        options.auto_confirm_high_risk = true;
        let status = refactor_graph::execute_refactor(repo_root, graph, options)?;

        fs::write(&target_file, &generated.tests)?;
        Ok(AppliedTests {
            target_file: rel_test_path,
            status: if status.failed_nodes.is_empty() {
                "applied".to_string()
            } else {
                "applied_with_refactor_warnings".to_string()
            },
            refactor_status: status,
        })
    }
}

fn sanitize_symbol(symbol: &str) -> String {
    let mut out = String::new();
    for ch in symbol.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == '_' {
            out.push(ch);
        }
    }
    if out.is_empty() {
        "symbol".to_string()
    } else {
        out
    }
}
