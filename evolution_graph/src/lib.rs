use anyhow::Result;
use engine::{EditContextItem, EditPlan, EditType};
use improvement_planner::{ImprovementPlan, ImprovementType};
use refactor_graph::{RefactorGraph, RefactorNode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionNode {
    pub id: String,
    pub improvement: ImprovementPlan,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EvolutionGraph {
    pub nodes: Vec<EvolutionNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionSimulation {
    pub estimated_patch_count: usize,
    pub estimated_node_count: usize,
    pub preview_nodes: Vec<String>,
}

pub struct EvolutionGraphBuilder;

impl EvolutionGraphBuilder {
    pub fn from_plans(plans: &[ImprovementPlan]) -> EvolutionGraph {
        let mut nodes = Vec::new();
        for (idx, plan) in plans.iter().enumerate() {
            let dependencies = if idx == 0 {
                vec![]
            } else {
                vec![format!("ev_{}", idx - 1)]
            };
            nodes.push(EvolutionNode {
                id: format!("ev_{idx}"),
                improvement: plan.clone(),
                dependencies,
            });
        }
        EvolutionGraph { nodes }
    }

    pub fn to_refactor_graph(graph: &EvolutionGraph, repository: String) -> RefactorGraph {
        let nodes = graph
            .nodes
            .iter()
            .map(|n| RefactorNode {
                id: n.id.clone(),
                target_symbol: n
                    .improvement
                    .target_symbols
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string()),
                edit_plan: improvement_to_edit_plan(&n.improvement),
                dependencies: n.dependencies.clone(),
                repository: Some(repository.clone()),
            })
            .collect();
        RefactorGraph { nodes }
    }

    pub fn simulate(graph: &EvolutionGraph) -> Result<EvolutionSimulation> {
        let preview_nodes: Vec<String> = graph
            .nodes
            .iter()
            .take(20)
            .map(|n| format!("{}: {}", n.id, n.improvement.description))
            .collect();
        Ok(EvolutionSimulation {
            estimated_patch_count: graph.nodes.len(),
            estimated_node_count: graph.nodes.len(),
            preview_nodes,
        })
    }
}

fn improvement_to_edit_plan(improvement: &ImprovementPlan) -> EditPlan {
    let edit_type = match improvement.improvement_type {
        ImprovementType::RemoveDeadCode => EditType::RefactorFunction,
        ImprovementType::ExtractInterface => EditType::ChangeSignature,
        ImprovementType::SplitModule => EditType::RefactorFunction,
        ImprovementType::SimplifyLogic => EditType::ModifyLogic,
        ImprovementType::DeduplicateCode => EditType::RefactorFunction,
    };

    EditPlan {
        target_symbol: improvement
            .target_symbols
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".to_string()),
        edit_type,
        impacted_symbols: improvement.target_symbols.clone(),
        required_context: vec![EditContextItem {
            file_path: "src/placeholder.ts".to_string(),
            start_line: 1,
            end_line: 1,
            priority: 0,
            text: improvement.description.clone(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::EvolutionGraphBuilder;
    use improvement_planner::{ImprovementPlan, ImprovementType};

    #[test]
    fn builds_and_simulates_graph() {
        let plans = vec![
            ImprovementPlan {
                plan_id: "p1".to_string(),
                description: "remove dead code".to_string(),
                improvement_type: ImprovementType::RemoveDeadCode,
                target_symbols: vec!["a".to_string()],
                expected_benefit: "x".to_string(),
            },
            ImprovementPlan {
                plan_id: "p2".to_string(),
                description: "split module".to_string(),
                improvement_type: ImprovementType::SplitModule,
                target_symbols: vec!["m".to_string()],
                expected_benefit: "y".to_string(),
            },
        ];
        let graph = EvolutionGraphBuilder::from_plans(&plans);
        let sim = EvolutionGraphBuilder::simulate(&graph).expect("sim");
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(sim.estimated_patch_count, 2);
    }
}
