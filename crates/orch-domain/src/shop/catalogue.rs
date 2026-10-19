// Catalogue data, pinned by the golden fixtures in `fixtures/shop/`: when this
// changes intentionally, update them too (tests/shop_catalogue.rs).

use crate::agent::ApprovalPolicy;
use crate::model_tiers::ModelTier;

use super::{AgentShopTemplate, OrgAgentEntry, OrgTemplate};

/// The 15 agent-shop templates, in catalogue order.
pub static AGENT_SHOP_TEMPLATES: [AgentShopTemplate; 15] = [
    AgentShopTemplate {
        key: "backend-dev",
        name: "Backend Developer",
        description: "APIs, databases, backend services",
        tier: ModelTier::Balanced,
        approval_policy: ApprovalPolicy::Auto,
        skills: &[
            "review",
            "careful",
            "feature-dev:feature-dev",
            "feature-dev:code-explorer",
        ],
        role: include_str!("roles/backend-dev.md"),
    },
    AgentShopTemplate {
        key: "frontend-dev",
        name: "Frontend Developer",
        description: "React, UI components, CSS, responsive design",
        tier: ModelTier::Balanced,
        approval_policy: ApprovalPolicy::Auto,
        skills: &[
            "design-review",
            "review",
            "feature-dev:feature-dev",
            "feature-dev:code-explorer",
        ],
        role: include_str!("roles/frontend-dev.md"),
    },
    AgentShopTemplate {
        key: "qa-engineer",
        name: "QA Engineer",
        description: "Test writing, coverage analysis, quality assurance, browser testing",
        tier: ModelTier::Balanced,
        approval_policy: ApprovalPolicy::Auto,
        skills: &[
            "qa",
            "testing-suite:generate-tests",
            "testing-suite:test-coverage",
        ],
        role: include_str!("roles/qa-engineer.md"),
    },
    AgentShopTemplate {
        key: "code-reviewer",
        name: "Code Reviewer",
        description: "PR review with auto-fix, adversarial review, security checks",
        tier: ModelTier::Capable,
        approval_policy: ApprovalPolicy::Suggest,
        skills: &[
            "review",
            "careful",
            "feature-dev:code-reviewer",
            "feature-dev:code-explorer",
        ],
        role: include_str!("roles/code-reviewer.md"),
    },
    AgentShopTemplate {
        key: "architect",
        name: "Architect",
        description: "System design, architecture decisions, tech leadership",
        tier: ModelTier::Capable,
        approval_policy: ApprovalPolicy::Suggest,
        skills: &[
            "plan-eng-review",
            "office-hours",
            "feature-dev:code-architect",
            "feature-dev:code-explorer",
        ],
        role: include_str!("roles/architect.md"),
    },
    AgentShopTemplate {
        key: "devops-engineer",
        name: "DevOps Engineer",
        description: "CI/CD, infrastructure, deployment, monitoring",
        tier: ModelTier::Balanced,
        approval_policy: ApprovalPolicy::Auto,
        skills: &["ship", "canary", "devops-automation:cloud-architect"],
        role: include_str!("roles/devops-engineer.md"),
    },
    AgentShopTemplate {
        key: "bug-hunter",
        name: "Bug Hunter",
        description: "Systematic debugging, root cause analysis, minimal fixes",
        tier: ModelTier::Balanced,
        approval_policy: ApprovalPolicy::Auto,
        skills: &[
            "investigate",
            "careful",
            "feature-dev:feature-dev",
            "feature-dev:code-explorer",
        ],
        role: include_str!("roles/bug-hunter.md"),
    },
    AgentShopTemplate {
        key: "tech-writer",
        name: "Technical Writer",
        description: "Documentation, READMEs, API docs, release notes",
        tier: ModelTier::Balanced,
        approval_policy: ApprovalPolicy::Auto,
        skills: &["document-release", "review", "feature-dev:code-explorer"],
        role: include_str!("roles/tech-writer.md"),
    },
    AgentShopTemplate {
        key: "marketer",
        name: "Marketer",
        description: "Marketing strategy, positioning, copy, campaigns",
        tier: ModelTier::Balanced,
        approval_policy: ApprovalPolicy::Auto,
        skills: &["office-hours"],
        role: include_str!("roles/marketer.md"),
    },
    AgentShopTemplate {
        key: "content-creator",
        name: "Content Creator",
        description: "Blog posts, articles, social media content",
        tier: ModelTier::Balanced,
        approval_policy: ApprovalPolicy::Auto,
        skills: &["office-hours"],
        role: include_str!("roles/content-creator.md"),
    },
    AgentShopTemplate {
        key: "growth-hacker",
        name: "Growth Hacker",
        description: "Growth experiments, analytics, user acquisition",
        tier: ModelTier::Balanced,
        approval_policy: ApprovalPolicy::Auto,
        skills: &["office-hours", "feature-dev:feature-dev"],
        role: include_str!("roles/growth-hacker.md"),
    },
    AgentShopTemplate {
        key: "security-auditor",
        name: "Security Auditor",
        description: "Security scanning, vulnerability analysis, OWASP, guardrails",
        tier: ModelTier::Capable,
        approval_policy: ApprovalPolicy::Suggest,
        skills: &["review", "careful", "guard", "feature-dev:code-reviewer"],
        role: include_str!("roles/security-auditor.md"),
    },
    AgentShopTemplate {
        key: "performance-engineer",
        name: "Performance Engineer",
        description: "Optimization, profiling, benchmarks, load testing",
        tier: ModelTier::Balanced,
        approval_policy: ApprovalPolicy::Auto,
        skills: &[
            "benchmark",
            "investigate",
            "feature-dev:feature-dev",
            "feature-dev:code-explorer",
        ],
        role: include_str!("roles/performance-engineer.md"),
    },
    AgentShopTemplate {
        key: "data-engineer",
        name: "Data Engineer",
        description: "Data pipelines, ETL, analytics, SQL",
        tier: ModelTier::Balanced,
        approval_policy: ApprovalPolicy::Auto,
        skills: &[
            "careful",
            "feature-dev:feature-dev",
            "feature-dev:code-explorer",
        ],
        role: include_str!("roles/data-engineer.md"),
    },
    AgentShopTemplate {
        key: "fullstack-dev",
        name: "Full-Stack Developer",
        description: "End-to-end development, frontend and backend",
        tier: ModelTier::Balanced,
        approval_policy: ApprovalPolicy::Auto,
        skills: &[
            "review",
            "design-review",
            "feature-dev:feature-dev",
            "feature-dev:code-explorer",
        ],
        role: include_str!("roles/fullstack-dev.md"),
    },
];

/// The 10 org-shop templates, in catalogue order.
pub static ORG_TEMPLATES: [OrgTemplate; 10] = [
    OrgTemplate {
        key: "startup-mvp",
        name: "Startup MVP",
        description: "Ship an MVP in 48 hours",
        agents: &[
            OrgAgentEntry {
                shop_key: "architect",
                name: "CTO",
            },
            OrgAgentEntry {
                shop_key: "backend-dev",
                name: "Backend",
            },
            OrgAgentEntry {
                shop_key: "backend-dev",
                name: "Backend 2",
            },
            OrgAgentEntry {
                shop_key: "frontend-dev",
                name: "Frontend",
            },
            OrgAgentEntry {
                shop_key: "qa-engineer",
                name: "QA",
            },
            OrgAgentEntry {
                shop_key: "code-reviewer",
                name: "Reviewer",
            },
        ],
        lead_index: 0,
    },
    OrgTemplate {
        key: "pr-review-corp",
        name: "PR Review Corp",
        description: "Automated review for every PR",
        agents: &[
            OrgAgentEntry {
                shop_key: "architect",
                name: "CTO",
            },
            OrgAgentEntry {
                shop_key: "security-auditor",
                name: "Security",
            },
            OrgAgentEntry {
                shop_key: "performance-engineer",
                name: "Performance",
            },
            OrgAgentEntry {
                shop_key: "code-reviewer",
                name: "Style",
            },
            OrgAgentEntry {
                shop_key: "qa-engineer",
                name: "QA",
            },
        ],
        lead_index: 0,
    },
    OrgTemplate {
        key: "migration-squad",
        name: "Migration Squad",
        description: "JS-to-TS migration over a weekend",
        agents: &[
            OrgAgentEntry {
                shop_key: "architect",
                name: "CTO",
            },
            OrgAgentEntry {
                shop_key: "fullstack-dev",
                name: "Migrator",
            },
            OrgAgentEntry {
                shop_key: "fullstack-dev",
                name: "Migrator 2",
            },
            OrgAgentEntry {
                shop_key: "fullstack-dev",
                name: "Migrator 3",
            },
            OrgAgentEntry {
                shop_key: "qa-engineer",
                name: "QA",
            },
            OrgAgentEntry {
                shop_key: "code-reviewer",
                name: "Reviewer",
            },
        ],
        lead_index: 0,
    },
    OrgTemplate {
        key: "security-dept",
        name: "Security Department",
        description: "Multi-layer security audit",
        agents: &[
            OrgAgentEntry {
                shop_key: "security-auditor",
                name: "Lead Auditor",
            },
            OrgAgentEntry {
                shop_key: "security-auditor",
                name: "Scanner",
            },
            OrgAgentEntry {
                shop_key: "security-auditor",
                name: "Secrets Auditor",
            },
            OrgAgentEntry {
                shop_key: "bug-hunter",
                name: "Hunter",
            },
            OrgAgentEntry {
                shop_key: "code-reviewer",
                name: "Reviewer",
            },
        ],
        lead_index: 0,
    },
    OrgTemplate {
        key: "test-factory",
        name: "Test Factory",
        description: "Coverage from 40% to 80% overnight",
        agents: &[
            OrgAgentEntry {
                shop_key: "qa-engineer",
                name: "Coverage Lead",
            },
            OrgAgentEntry {
                shop_key: "backend-dev",
                name: "Backend",
            },
            OrgAgentEntry {
                shop_key: "backend-dev",
                name: "Backend 2",
            },
            OrgAgentEntry {
                shop_key: "qa-engineer",
                name: "QA",
            },
            OrgAgentEntry {
                shop_key: "qa-engineer",
                name: "QA 2",
            },
            OrgAgentEntry {
                shop_key: "code-reviewer",
                name: "Reviewer",
            },
        ],
        lead_index: 0,
    },
    OrgTemplate {
        key: "content-agency",
        name: "Content Agency",
        description: "Content factory: plan, write, edit, optimize",
        agents: &[
            OrgAgentEntry {
                shop_key: "marketer",
                name: "Strategist",
            },
            OrgAgentEntry {
                shop_key: "content-creator",
                name: "Writer",
            },
            OrgAgentEntry {
                shop_key: "content-creator",
                name: "Writer 2",
            },
            OrgAgentEntry {
                shop_key: "tech-writer",
                name: "Editor",
            },
            OrgAgentEntry {
                shop_key: "growth-hacker",
                name: "SEO",
            },
        ],
        lead_index: 0,
    },
    OrgTemplate {
        key: "data-lab",
        name: "Data Lab",
        description: "3 CSVs to executive report by morning",
        agents: &[
            OrgAgentEntry {
                shop_key: "data-engineer",
                name: "Lead Analyst",
            },
            OrgAgentEntry {
                shop_key: "data-engineer",
                name: "Data Engineer",
            },
        ],
        lead_index: 0,
    },
    OrgTemplate {
        key: "sales-machine",
        name: "Sales Machine",
        description: "Outbound pipeline: research, outreach, follow-up, close",
        agents: &[
            OrgAgentEntry {
                shop_key: "marketer",
                name: "Sales Director",
            },
            OrgAgentEntry {
                shop_key: "content-creator",
                name: "SDR",
            },
            OrgAgentEntry {
                shop_key: "content-creator",
                name: "SDR 2",
            },
            OrgAgentEntry {
                shop_key: "content-creator",
                name: "Copywriter",
            },
            OrgAgentEntry {
                shop_key: "growth-hacker",
                name: "Growth Analyst",
            },
        ],
        lead_index: 0,
    },
    OrgTemplate {
        key: "bugfix-dept",
        name: "Bugfix Department",
        description: "100 issues to 0 in a week",
        agents: &[
            OrgAgentEntry {
                shop_key: "architect",
                name: "Triager",
            },
            OrgAgentEntry {
                shop_key: "bug-hunter",
                name: "Fixer",
            },
            OrgAgentEntry {
                shop_key: "bug-hunter",
                name: "Fixer 2",
            },
            OrgAgentEntry {
                shop_key: "bug-hunter",
                name: "Fixer 3",
            },
            OrgAgentEntry {
                shop_key: "qa-engineer",
                name: "QA",
            },
            OrgAgentEntry {
                shop_key: "code-reviewer",
                name: "Reviewer",
            },
        ],
        lead_index: 0,
    },
    OrgTemplate {
        key: "docs-team",
        name: "Docs Team",
        description: "Technical docs from codebase analysis",
        agents: &[
            OrgAgentEntry {
                shop_key: "architect",
                name: "Docs Lead",
            },
            OrgAgentEntry {
                shop_key: "tech-writer",
                name: "Writer",
            },
            OrgAgentEntry {
                shop_key: "tech-writer",
                name: "Writer 2",
            },
            OrgAgentEntry {
                shop_key: "tech-writer",
                name: "Editor",
            },
            OrgAgentEntry {
                shop_key: "code-reviewer",
                name: "Reviewer",
            },
        ],
        lead_index: 0,
    },
];
