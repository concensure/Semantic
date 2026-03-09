use architecture_analysis::{ArchitectureIssue, ArchitectureIssueType};
use code_health::{CodeHealthIssue, IssueType};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImprovementPlan {
    pub plan_id: String,
    pub description: String,
    pub improvement_type: ImprovementType,
    pub target_symbols: Vec<String>,
    pub expected_benefit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ImprovementType {
    RemoveDeadCode,
    ExtractInterface,
    SplitModule,
    SimplifyLogic,
    DeduplicateCode,
}

pub struct ImprovementPlanner;

impl ImprovementPlanner {
    pub fn from_issues(
        code_issues: &[CodeHealthIssue],
        architecture_issues: &[ArchitectureIssue],
    ) -> Vec<ImprovementPlan> {
        let mut plans = Vec::new();

        for issue in code_issues {
            let (improvement_type, description, expected_benefit) = match issue.issue_type {
                IssueType::DeadCode => (
                    ImprovementType::RemoveDeadCode,
                    format!("Remove unused symbol in {}", issue.file_path),
                    "Reduce maintenance surface and build size".to_string(),
                ),
                IssueType::DuplicateLogic => (
                    ImprovementType::DeduplicateCode,
                    format!("Deduplicate repeated logic in {}", issue.file_path),
                    "Lower bug duplication and simplify updates".to_string(),
                ),
                IssueType::LargeFunction | IssueType::DeepNesting => (
                    ImprovementType::SimplifyLogic,
                    format!("Simplify complex function in {}", issue.file_path),
                    "Improve readability and reduce defect risk".to_string(),
                ),
                IssueType::LargeModule => (
                    ImprovementType::SplitModule,
                    format!("Split large module at {}", issue.file_path),
                    "Improve modularity and testability".to_string(),
                ),
                IssueType::CircularDependency | IssueType::UnusedImport => (
                    ImprovementType::ExtractInterface,
                    format!("Break dependency smell in {}", issue.file_path),
                    "Reduce coupling and improve layering".to_string(),
                ),
            };

            plans.push(ImprovementPlan {
                plan_id: format!("imp_code_{}", sanitize_id(&issue.issue_id)),
                description,
                improvement_type,
                target_symbols: issue.symbol.clone().into_iter().collect(),
                expected_benefit,
            });
        }

        for issue in architecture_issues {
            let (improvement_type, expected_benefit) = match issue.issue_type {
                ArchitectureIssueType::LayerViolation => {
                    (ImprovementType::ExtractInterface, "Restore architectural boundaries")
                }
                ArchitectureIssueType::TightCoupling => {
                    (ImprovementType::ExtractInterface, "Reduce coupling and improve evolution speed")
                }
                ArchitectureIssueType::MissingInterface => {
                    (ImprovementType::ExtractInterface, "Improve abstraction and replaceability")
                }
                ArchitectureIssueType::CyclicModuleDependency => {
                    (ImprovementType::SplitModule, "Break cycles and simplify dependency flow")
                }
                ArchitectureIssueType::GodModule => {
                    (ImprovementType::SplitModule, "Reduce module size and ownership contention")
                }
            };
            plans.push(ImprovementPlan {
                plan_id: format!("imp_arch_{}", sanitize_id(&issue.module)),
                description: issue.description.clone(),
                improvement_type,
                target_symbols: vec![issue.module.clone()],
                expected_benefit: expected_benefit.to_string(),
            });
        }

        plans.sort_by(|a, b| a.plan_id.cmp(&b.plan_id));
        plans
    }
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::ImprovementPlanner;
    use architecture_analysis::{ArchitectureIssue, ArchitectureIssueType};
    use code_health::{CodeHealthIssue, IssueType, Severity};

    #[test]
    fn builds_plans_from_mixed_issues() {
        let code = vec![CodeHealthIssue {
            issue_id: "i1".to_string(),
            repository: "r".to_string(),
            file_path: "src/a.ts".to_string(),
            symbol: Some("deadFn".to_string()),
            issue_type: IssueType::DeadCode,
            severity: Severity::Medium,
            description: "dead".to_string(),
        }];
        let arch = vec![ArchitectureIssue {
            module: "api".to_string(),
            issue_type: ArchitectureIssueType::TightCoupling,
            description: "coupling".to_string(),
        }];
        let plans = ImprovementPlanner::from_issues(&code, &arch);
        assert_eq!(plans.len(), 2);
    }
}
