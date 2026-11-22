
#[test]
fn overflow_is_a_refusal_even_at_the_largest_ceiling() {
    let error = super::add("expanded_nodes", u64::MAX, 1, u64::MAX).unwrap_err();
    assert_eq!(error.rule, "expanded_nodes");
    assert_eq!(error.observed, u64::MAX);
}
