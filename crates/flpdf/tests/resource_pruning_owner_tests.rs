use flpdf::job::{should_remove_unreferenced_resources, RemoveUnreferencedResources};
use flpdf::Pdf;

#[test]
fn resource_pruning_policy_is_owned_by_the_job_module() {
    let resources = include_str!("../src/resources.rs");
    let policy = include_str!("../src/job/resource_pruning.rs");

    assert!(!resources.contains("pub enum RemoveUnreferencedResources"));
    assert!(!resources.contains("pub fn should_remove_unreferenced_resources"));
    assert!(policy.contains("pub enum RemoveUnreferencedResources"));
    assert!(policy.contains("pub fn should_remove_unreferenced_resources"));
}

#[test]
fn public_job_policy_route_keeps_the_qpdf_default_and_empty_document_behavior() {
    assert_eq!(
        RemoveUnreferencedResources::default(),
        RemoveUnreferencedResources::Auto
    );

    let mut pdf = Pdf::empty().expect("empty PDF");
    assert!(!should_remove_unreferenced_resources(&mut pdf).expect("heuristic result"));
}
