use flpdf::job::RemoveUnreferencedResources;

#[test]
fn resource_pruning_policy_is_owned_by_the_job_module() {
    let resources = include_str!("../src/resources.rs");
    let policy = include_str!("../src/job/resource_pruning.rs");
    let job_module = include_str!("../src/job/mod.rs");
    let crate_root = include_str!("../src/lib.rs");

    assert!(!resources.contains("pub enum RemoveUnreferencedResources"));
    assert!(!resources.contains("pub fn should_remove_unreferenced_resources"));
    assert!(policy.contains("pub enum RemoveUnreferencedResources"));
    assert!(policy.contains("pub(crate) fn should_remove_unreferenced_resources"));
    assert!(!job_module.contains("pub use resource_pruning::{should_remove_unreferenced_resources"));
    assert!(!crate_root.contains("should_remove_unreferenced_resources"));
}

#[test]
fn public_job_policy_route_keeps_the_qpdf_default_mode() {
    assert_eq!(
        RemoveUnreferencedResources::default(),
        RemoveUnreferencedResources::Auto
    );
}
