use crate::document::Document;
use crate::remarks::Remark;
use crate::openai::client::{ChatMessage, ChatRequest, ResponseFormat};
use crate::config::Config;
use crate::revision_context::{build_targeted_revision_plan, RevisionRequestMode, TargetedRevisionPlan};

const REVISION_SYSTEM_PROMPT: &str = r#"You are Aichitect, an expert technical writer and document architect. Your job is to revise specific sections of a technical document based on user remarks.

You will receive:
1. The full document with anchor IDs for each node
2. A list of user remarks targeting specific anchors
3. The current state of those nodes

You MUST respond with a JSON object containing a "patches" array. Each patch must have:
- "op": one of "replace_section", "replace_text_span", "replace_code_block", "insert_after", "insert_before", "delete_block", "update_heading_text", "update_list_item"
- "anchor": the anchor ID of the target node (must exist in the document)
- "rationale": brief explanation of the change

For replace/insert ops, include "content" with the new markdown text.
For replace_code_block, include "content" (code only, no fences) and optionally "lang".
For update_heading_text / update_list_item, include "new_text".

IMPORTANT:
- Only modify the nodes referenced in the remarks
- Preserve the document's overall structure and voice
- Use proper markdown formatting
- For code blocks, provide only the code content without fences
- Anchors are case-sensitive

Response format:
{
  "patches": [
    {"op": "replace_section", "anchor": "p-0", "content": "...", "rationale": "..."},
    ...
  ]
}
"#;

const REVIEW_SYSTEM_PROMPT: &str = r#"You are Aichitect, an expert requirements analyst and technical reviewer. Your job is to analyze a technical document for issues that would hinder implementation, cause ambiguity, or introduce risk.

For each issue you find, output a JSON object in the "issues" array with:
- "category": one of: ambiguity, contradiction, missing_acceptance_criteria, undefined_term, hidden_assumption, missing_edge_case, missing_operational_constraint, unclear_ownership, vague_success_metric, missing_failure_behavior, misleading_wording, incomplete_code_example, unspecified_input_output
- "anchor": the anchor ID of the most relevant node (must exist in the document)
- "evidence": a direct quote or reference from the document
- "why_it_matters": 1-2 sentences explaining the impact
- "suggested_resolution": a concrete, actionable suggestion for how to fix or clarify the issue

Be thorough but precise. Focus on substantive issues, not style preferences.

Response format:
{
  "issues": [
    {
      "category": "ambiguity",
      "anchor": "p-0",
      "evidence": "...",
      "why_it_matters": "...",
      "suggested_resolution": "..."
    },
    ...
  ]
}
"#;

const TARGETED_REVISION_SYSTEM_PROMPT: &str = r#"You are Aichitect, an expert technical writer and document architect. Your job is to revise a document using only the targeted context packs provided.

You are NOT receiving the full document. You will receive:
1. A limited anchor map for the relevant anchors only
2. One or more targeted revision context packs
3. Explicit related occurrence targets when the same change should be applied consistently

You MUST:
- Only produce patches for anchors explicitly included in the request
- Use the local context packs to preserve terminology and structure
- Apply the same change consistently to explicit occurrence targets when the instruction implies global replacement/removal
- Avoid inventing edits to anchors that were not provided

You MUST respond with a JSON object containing a "patches" array. Each patch must have:
- "op": one of "replace_section", "replace_text_span", "replace_code_block", "insert_after", "insert_before", "delete_block", "update_heading_text", "update_list_item"
- "anchor": one of the provided target anchors
- "rationale": brief explanation of the change

For replace/insert ops, include "content" with the new markdown text.
For replace_code_block, include "content" (code only, no fences) and optionally "lang".
For update_heading_text / update_list_item, include "new_text".

Respond with valid JSON only."#;

pub struct PreparedRevisionRequest {
    pub request: ChatRequest,
    pub mode: RevisionRequestMode,
}

pub fn build_revision_request(
    config: &Config,
    doc: &Document,
    remarks: &[&Remark],
) -> PreparedRevisionRequest {
    if let Some(plan) = build_targeted_revision_plan(doc, remarks) {
        return PreparedRevisionRequest {
            request: build_targeted_revision_request(config, &plan),
            mode: RevisionRequestMode::Targeted,
        };
    }

    PreparedRevisionRequest {
        request: build_full_revision_request(config, doc, remarks),
        mode: RevisionRequestMode::FullDocument,
    }
}

fn build_full_revision_request(
    config: &Config,
    doc: &Document,
    remarks: &[&Remark],
) -> ChatRequest {
    let system = config.system_prompt_override.as_deref()
        .unwrap_or(REVISION_SYSTEM_PROMPT)
        .to_string();

    let anchor_map = doc.anchor_map_display();
    let mut user_msg = format!(
        "## Document Anchor Map\n\n{}\n\n## Full Document\n\n```markdown\n{}\n```\n\n## User Remarks\n\n",
        anchor_map,
        doc.raw
    );

    for remark in remarks {
        if let Some(list_ctx) = &remark.list_context {
            user_msg.push_str(&format!(
                "### Remark on list item `{}`\n\
                 **Selected item:** {}\n\
                 **Full list (with anchors):**\n```\n{}\n```\n\
                 **Instruction:** {}\n\
                 > You may update any items in this list — reorder, merge duplicates, add or remove items as needed. \
                 Return one `UpdateListItem` patch per changed item, and `DeleteBlock` for removed items.\n\n",
                remark.anchor,
                remark.selected_text,
                list_ctx,
                remark.text
            ));
        } else {
            user_msg.push_str(&format!(
                "### Remark on `{}`\n**Target text:** {}\n**Instruction:** {}\n\n",
                remark.anchor,
                remark.selected_text,
                remark.text
            ));
        }

        if !remark.occurrence_anchors.is_empty() {
            user_msg.push_str(
                "**Also apply the same change consistently to all of these occurrences \
                 of the same pattern — produce one patch per location:**\n"
            );
            for (anchor, snippet) in &remark.occurrence_anchors {
                user_msg.push_str(&format!("- `{}`: {}\n", anchor, snippet));
            }
            user_msg.push('\n');
        }
    }

    user_msg.push_str("\nPlease provide patches to address all remarks. Respond with valid JSON only.");

    ChatRequest {
        model: config.model_fix.clone(),
        messages: vec![
            ChatMessage { role: "system".to_string(), content: system },
            ChatMessage { role: "user".to_string(), content: user_msg },
        ],
        temperature: config.temperature,
        max_tokens: config.max_tokens,
        stream: false,
        response_format: Some(ResponseFormat { r#type: "json_object".to_string() }),
    }
}

fn build_targeted_revision_request(
    config: &Config,
    plan: &TargetedRevisionPlan,
) -> ChatRequest {
    let mut relevant_anchors = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for pack in &plan.packs {
        if seen.insert(pack.primary_target.anchor.clone()) {
            relevant_anchors.push(format!(
                "{}: primary target",
                pack.primary_target.anchor
            ));
        }
        for target in &pack.occurrence_targets {
            if seen.insert(target.anchor.clone()) {
                relevant_anchors.push(format!("{}: related occurrence", target.anchor));
            }
        }
        for node in &pack.local_context_nodes {
            if seen.insert(node.anchor.clone()) {
                relevant_anchors.push(format!("{}: {}", node.anchor, node.summary));
            }
        }
    }

    let mut user_msg = format!(
        "## Relevant Anchors\n\n{}\n\n## Targeted Revision Context Packs\n\n",
        relevant_anchors.join("\n")
    );

    for (idx, pack) in plan.packs.iter().enumerate() {
        user_msg.push_str(&format!(
            "### Context Pack {}\n\
             Generation: {}\n\
             Primary anchor: `{}`\n\
             Instruction: {}\n\
             Selected text: {}\n\
             Primary target markdown:\n```markdown\n{}\n```\n\n",
            idx + 1,
            pack.generation,
            pack.primary_target.anchor,
            pack.instruction,
            pack.primary_target.selected_text,
            pack.primary_target.raw_markdown
        ));

        if let Some(list_context) = &pack.list_context {
            user_msg.push_str(&format!(
                "List context:\n```markdown\n{}\n```\n\n",
                list_context
            ));
        }

        if !pack.local_context_nodes.is_empty() {
            user_msg.push_str("Local context nodes:\n");
            for node in &pack.local_context_nodes {
                user_msg.push_str(&format!(
                    "- `{}` ({})\n```markdown\n{}\n```\n",
                    node.anchor,
                    node.summary,
                    node.raw_markdown
                ));
            }
            user_msg.push('\n');
        }

        if !pack.occurrence_targets.is_empty() {
            user_msg.push_str("Explicit related occurrence targets:\n");
            for target in &pack.occurrence_targets {
                user_msg.push_str(&format!(
                    "- `{}` ({})\n  Selected text: {}\n```markdown\n{}\n```\n",
                    target.anchor,
                    target.role,
                    target.selected_text,
                    target.raw_markdown
                ));
            }
            user_msg.push('\n');
        }
    }

    user_msg.push_str(
        "Please provide patches to address all instructions using only the provided anchors and context. Respond with valid JSON only.",
    );

    ChatRequest {
        model: config.model_fix.clone(),
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: TARGETED_REVISION_SYSTEM_PROMPT.to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: user_msg,
            },
        ],
        temperature: config.temperature,
        max_tokens: config.max_tokens,
        stream: false,
        response_format: Some(ResponseFormat { r#type: "json_object".to_string() }),
    }
}

pub fn build_ambiguity_request(config: &Config, doc: &Document) -> ChatRequest {
    let anchor_map = doc.anchor_map_display();
    let user_msg = format!(
        "## Document Anchor Map\n\n{}\n\n## Full Document\n\n```markdown\n{}\n```\n\nPlease review this document and identify all issues. Respond with valid JSON only.",
        anchor_map,
        doc.raw
    );

    ChatRequest {
        model: config.model.clone(),
        messages: vec![
            ChatMessage { role: "system".to_string(), content: REVIEW_SYSTEM_PROMPT.to_string() },
            ChatMessage { role: "user".to_string(), content: user_msg },
        ],
        temperature: Some(0.2),
        max_tokens: config.max_tokens,
        stream: false,
        response_format: Some(ResponseFormat { r#type: "json_object".to_string() }),
    }
}

const CREATION_SYSTEM_PROMPT: &str = r#"You are a technical document creator. Create a well-structured, comprehensive markdown document based on the user's description.

Guidelines:
- Use proper heading hierarchy (# / ## / ###)
- Include concrete sections, acceptance criteria, and examples where relevant
- Use fenced code blocks with language tags for any code
- Be precise and specific — avoid vague language
- Structure the document so each section can be individually reviewed and iterated

Return ONLY the raw markdown content. No preamble, no explanation, no JSON wrapping — just the document."#;

pub fn build_creation_request(config: &Config, prompt: &str) -> ChatRequest {
    ChatRequest {
        model: config.model_fix.clone(),
        messages: vec![
            ChatMessage { role: "system".to_string(), content: CREATION_SYSTEM_PROMPT.to_string() },
            ChatMessage { role: "user".to_string(), content: prompt.to_string() },
        ],
        temperature: config.temperature,
        max_tokens: config.max_tokens,
        stream: false,
        response_format: None, // plain markdown, not JSON
    }
}
