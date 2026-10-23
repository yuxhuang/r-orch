//! T1 / F1 — D3-2 and D3-3: role summaries. Explicit summaries win and are
//! rejected (never silently shortened) when empty or over 192 UTF-8 bytes;
//! derived summaries skip only the exact known preamble; clipping is exact,
//! marker included, and UTF-8 safe.

use orch_domain::{
    Agent, AgentConfig, AgentStats, AgentStatus, ROLE_SUMMARY_MAX_BYTES, RoleSummaryError,
    clip_with_marker, derive_role_summary, normalize_role_summary, role_summary_for,
    truncation_marker,
};

const LIMIT: usize = 192;

/// The shipped preamble paragraph, verbatim from the frozen fixture prefix.
const PREAMBLE: &str = "Embedded shared-rule sections are authoritative; no rule-file lookup is required. <workspace root> means the owning orch project root (the directory containing .orchestry), not the per-task checkout. Establish it from task/context before using destination placeholders. If unavailable, report it to the lead instead of guessing from cwd. Sibling ../pensieve destinations and named skills are deployment prerequisites; edit the template for other layouts.";

fn agent(name: &str, role: Option<&str>, summary: Option<&str>) -> Agent {
    Agent {
        id: "agt_x".into(),
        name: name.into(),
        adapter: "claude".into(),
        role: role.map(str::to_owned),
        config: AgentConfig {
            role_summary: summary.map(str::to_owned),
            ..AgentConfig::default()
        },
        status: AgentStatus::Idle,
        current_task: None,
        autonomous: None,
        stats: AgentStats::default(),
        last_error: None,
        capabilities: Vec::new(),
        remote: None,
    }
}

/// Split a clipped summary into (retained prefix, omitted count).
fn parse_clip(clipped: &str) -> (&str, usize) {
    let at = clipped.rfind(" [truncated, ").expect("has a marker");
    let (prefix, marker) = clipped.split_at(at);
    let digits = marker
        .strip_prefix(" [truncated, ")
        .and_then(|m| m.strip_suffix(" more bytes]"))
        .expect("marker shape is exact");
    (prefix, digits.parse().expect("count is a number"))
}

// ── Explicit summaries (authoring) ──────────────────────────────────────────

#[test]
fn an_explicit_summary_of_191_bytes_is_accepted_unchanged() {
    let s = "a".repeat(191);
    assert_eq!(normalize_role_summary(&s), Ok(s));
}

#[test]
fn an_explicit_summary_of_exactly_192_bytes_is_accepted_unchanged() {
    let s = "a".repeat(192);
    assert_eq!(normalize_role_summary(&s), Ok(s));
}

#[test]
fn an_explicit_summary_of_193_bytes_is_rejected_with_its_size_not_shortened() {
    assert_eq!(
        normalize_role_summary(&"a".repeat(193)),
        Err(RoleSummaryError::TooLong { bytes: 193 })
    );
}

#[test]
fn the_limit_is_counted_in_utf8_bytes_not_characters() {
    // 64 three-byte characters = 192 bytes (accepted); 65 = 195 (rejected).
    assert!(normalize_role_summary(&"€".repeat(64)).is_ok());
    assert_eq!(
        normalize_role_summary(&"€".repeat(65)),
        Err(RoleSummaryError::TooLong { bytes: 195 })
    );
}

#[test]
fn a_multibyte_char_straddling_byte_192_makes_the_summary_too_long() {
    // 190 ASCII + one 4-byte emoji = 194 bytes although only 191 characters.
    let s = format!("{}😀", "a".repeat(190));
    assert_eq!(
        normalize_role_summary(&s),
        Err(RoleSummaryError::TooLong { bytes: 194 })
    );
}

#[test]
fn the_limit_applies_after_whitespace_normalization() {
    // 192 letters plus a lot of collapsible whitespace around and inside.
    let s = format!("  \t{}\r\n  ", "a".repeat(100)) + &"a".repeat(91);
    let ok = normalize_role_summary(&s).expect("192 bytes once normalized");
    assert_eq!(ok.len(), LIMIT);
}

#[test]
fn an_empty_explicit_summary_is_rejected() {
    assert_eq!(normalize_role_summary(""), Err(RoleSummaryError::Empty));
}

#[test]
fn a_whitespace_only_explicit_summary_is_rejected_as_empty() {
    assert_eq!(
        normalize_role_summary(" \t\r\n\u{a0}\u{2003} "),
        Err(RoleSummaryError::Empty)
    );
}

#[test]
fn cr_lf_and_tab_in_an_explicit_summary_become_single_spaces_on_one_line() {
    assert_eq!(
        normalize_role_summary("Reviews\r\nRust\tcode\n\nand   more.\r"),
        Ok("Reviews Rust code and more.".to_owned())
    );
}

#[test]
fn control_and_template_text_in_an_explicit_summary_is_kept_as_data() {
    let s = "Use {{ agent.name }} and {% raw %} \u{7}bell";
    assert_eq!(normalize_role_summary(s), Ok(s.to_owned()));
}

// ── Display of explicit summaries ───────────────────────────────────────────

#[test]
fn an_explicit_summary_wins_over_the_role_text() {
    let a = agent(
        "Reviewer",
        Some("Writes docs. More."),
        Some("Reviews Rust."),
    );
    assert_eq!(role_summary_for(&a), "Reviews Rust.");
}

#[test]
fn an_explicit_summary_never_replaces_the_full_role() {
    let a = agent(
        "Reviewer",
        Some("Writes docs. More."),
        Some("Reviews Rust."),
    );
    assert_eq!(a.role.as_deref(), Some("Writes docs. More."));
}

#[test]
fn a_blank_hand_edited_explicit_summary_falls_back_to_the_derived_one() {
    let a = agent("Reviewer", Some("Writes docs. More."), Some(" \t "));
    assert_eq!(role_summary_for(&a), "Writes docs.");
}

#[test]
fn a_hand_edited_oversized_explicit_summary_is_clipped_for_display_with_a_marker() {
    let long = "w".repeat(500);
    let shown = role_summary_for(&agent("A", None, Some(&long)));
    assert_eq!(shown.len(), LIMIT);
    let (prefix, omitted) = parse_clip(&shown);
    assert_eq!(prefix.len() + omitted, 500);
}

// ── Derivation ──────────────────────────────────────────────────────────────

#[test]
fn the_exact_known_preamble_paragraph_is_skipped_and_the_next_sentence_is_used() {
    let role = format!("{PREAMBLE}\n\nSenior test engineer. You write tests.\n\nMore.");
    assert_eq!(
        derive_role_summary("test-engineer", Some(&role)),
        "Senior test engineer."
    );
}

#[test]
fn a_leading_heading_then_the_exact_preamble_are_both_skipped() {
    let role = format!("# Role\n\n{PREAMBLE}\n\nCode reviewer! Reads diffs.");
    assert_eq!(derive_role_summary("n", Some(&role)), "Code reviewer!");
}

#[test]
fn a_near_match_preamble_is_not_stripped() {
    // One word differs from the known opening sentence.
    let near = PREAMBLE.replacen("authoritative", "authoritative-ish", 1);
    let role = format!("{near}\n\nSenior test engineer.");
    let got = derive_role_summary("n", Some(&role));
    assert!(
        got.starts_with("Embedded shared-rule sections are authoritative-ish;"),
        "{got}"
    );
}

#[test]
fn a_preamble_with_a_different_ending_is_not_stripped() {
    let near = PREAMBLE.replace("other layouts.", "other layouts!!");
    let role = format!("{near}\n\nSenior test engineer.");
    assert!(derive_role_summary("n", Some(&role)).starts_with("Embedded shared-rule"));
}

#[test]
fn the_preamble_only_counts_in_leading_position() {
    let role = format!("Real first paragraph here.\n\n{PREAMBLE}");
    assert_eq!(
        derive_role_summary("n", Some(&role)),
        "Real first paragraph here."
    );
}

#[test]
fn a_keyword_looking_paragraph_is_not_dropped_by_vague_matching() {
    let role = "Rules: shared rule sections matter. Second.";
    assert_eq!(
        derive_role_summary("n", Some(role)),
        "Rules: shared rule sections matter."
    );
}

#[test]
fn an_empty_role_falls_back_to_the_normalized_agent_name() {
    assert_eq!(derive_role_summary("  Test \t Eng ", Some("")), "Test Eng");
}

#[test]
fn an_absent_role_falls_back_to_the_normalized_agent_name() {
    assert_eq!(derive_role_summary("Test Eng", None), "Test Eng");
}

#[test]
fn a_whitespace_only_role_falls_back_to_the_agent_name() {
    assert_eq!(derive_role_summary("Name", Some(" \n\t\n  ")), "Name");
}

#[test]
fn a_headings_only_role_falls_back_to_the_agent_name() {
    assert_eq!(
        derive_role_summary("Name", Some("# Title\n\n## Sub\n\n###### Deep")),
        "Name"
    );
}

#[test]
fn a_role_that_is_only_the_preamble_falls_back_to_the_agent_name() {
    assert_eq!(derive_role_summary("Name", Some(PREAMBLE)), "Name");
}

#[test]
fn a_hash_without_a_space_is_not_a_heading_and_is_kept() {
    assert_eq!(
        derive_role_summary("Name", Some("#hashtag is a thing. Next.")),
        "#hashtag is a thing."
    );
}

#[test]
fn the_first_sentence_ends_only_at_punctuation_followed_by_space_or_end() {
    assert_eq!(
        derive_role_summary("n", Some("Uses v1.2.3 daily. Second one.")),
        "Uses v1.2.3 daily."
    );
}

#[test]
fn a_paragraph_without_sentence_punctuation_is_used_whole_with_lines_joined() {
    assert_eq!(
        derive_role_summary("n", Some("line one\nline   two")),
        "line one line two"
    );
}

#[test]
fn cr_lf_and_tab_in_a_role_produce_a_one_line_summary() {
    let got = derive_role_summary("n", Some("Does\r\nthings\twell.\r\n\r\nNext."));
    assert_eq!(got, "Does things well.");
    assert!(!got.contains(['\r', '\n', '\t']));
}

// ── Exact clipping and the 191/192/193 boundary ─────────────────────────────

#[test]
fn a_derived_sentence_of_191_and_192_bytes_is_not_clipped() {
    for n in [191usize, 192] {
        let s = format!("{}.", "a".repeat(n - 1));
        assert_eq!(s.len(), n);
        assert_eq!(derive_role_summary("n", Some(&s)), s, "{n}");
    }
}

#[test]
fn a_derived_sentence_of_193_bytes_is_clipped_to_exactly_192_with_the_exact_count() {
    let s = format!("{}.", "a".repeat(192));
    assert_eq!(s.len(), 193);
    let got = derive_role_summary("n", Some(&s));
    assert_eq!(got.len(), 192);
    let (prefix, omitted) = parse_clip(&got);
    assert_eq!(prefix.len(), 165);
    assert_eq!(omitted, 28);
    assert!(s.starts_with(prefix));
}

#[test]
fn the_marker_text_is_exactly_the_documented_ascii_form() {
    assert_eq!(truncation_marker(28), "[truncated, 28 more bytes]");
}

#[test]
fn clipping_is_exact_for_every_length_across_digit_width_changes() {
    // ASCII, no spaces: the longest prefix always fills the budget exactly.
    let mut lengths: Vec<usize> = (193..=1300).collect();
    lengths.extend([9_999, 10_000, 99_999, 100_000, 100_001]);
    for len in lengths {
        let text = "x".repeat(len);
        let clipped = clip_with_marker(&text, ROLE_SUMMARY_MAX_BYTES);
        assert_eq!(clipped.len(), LIMIT, "len {len}");
        let (prefix, omitted) = parse_clip(&clipped);
        assert_eq!(
            prefix.len() + omitted,
            len,
            "count is original minus kept, len {len}"
        );
        assert!(text.starts_with(prefix), "len {len}");
    }
}

#[test]
fn a_four_byte_emoji_at_the_boundary_is_never_split_and_the_count_is_in_bytes() {
    for pad in 150..=175 {
        let text = format!("{}😀😀😀 {}", "a".repeat(pad), "tail ".repeat(20));
        let clipped = clip_with_marker(&text, ROLE_SUMMARY_MAX_BYTES);
        // `String` is valid UTF-8 by construction; the retained part must also
        // be an exact prefix and the reported count its complement.
        assert!(clipped.len() <= LIMIT, "pad {pad}");
        let (prefix, omitted) = parse_clip(&clipped);
        assert!(text.starts_with(prefix), "pad {pad}");
        assert_eq!(prefix.len() + omitted, text.len(), "pad {pad}");
        // Maximal: the next whole character would not have fit.
        let next = text[prefix.len()..].chars().next().expect("more text");
        let wider = prefix.len() + next.len_utf8();
        let marker_len = 1 + truncation_marker(text.len() - wider).len();
        assert!(wider + marker_len > LIMIT, "pad {pad}");
    }
}

#[test]
fn a_derived_summary_with_an_emoji_across_the_limit_stays_within_192_bytes() {
    let sentence = format!("{}😀 and more words follow here.", "é".repeat(88));
    let got = derive_role_summary("n", Some(&sentence));
    assert!(got.len() <= LIMIT, "{} bytes", got.len());
    let (prefix, omitted) = parse_clip(&got);
    assert_eq!(prefix.len() + omitted, sentence.len());
}

#[test]
fn text_that_fits_gets_no_marker() {
    assert_eq!(clip_with_marker("short", ROLE_SUMMARY_MAX_BYTES), "short");
    assert_eq!(clip_with_marker("", ROLE_SUMMARY_MAX_BYTES), "");
}

#[test]
fn trailing_whitespace_at_the_cut_is_dropped_not_kept_before_the_marker() {
    let text = format!("{} {}", "a".repeat(164), "b".repeat(60));
    let clipped = clip_with_marker(&text, ROLE_SUMMARY_MAX_BYTES);
    assert!(!clipped.contains("  "), "{clipped:?}");
    let (prefix, omitted) = parse_clip(&clipped);
    assert!(!prefix.ends_with(' '));
    assert_eq!(prefix.len(), 164, "the space at the cut is dropped");
    assert_eq!(prefix.len() + omitted, text.len());
}

#[test]
fn a_budget_too_small_for_any_prefix_yields_only_the_marker() {
    let clipped = clip_with_marker(&"x".repeat(500), 20);
    assert_eq!(clipped, "[truncated, 500 more bytes]");
}
