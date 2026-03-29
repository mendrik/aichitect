use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::config::Config;
use crate::document::{Document, PatchOp};
use crate::openai::client::{ResponseInputMessage, ResponseRequest, ResponseTextConfig};
use crate::remarks::Remark;
use crate::review::{ReviewItem, ReviewStatus};
use crate::revision_context::{build_targeted_revision_plan, TargetedRevisionPlan};

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
- Treat the provided document generation and local context as the current source of truth even if earlier turns used older text."#;

const REVIEW_SYSTEM_PROMPT: &str = r#"You are Aichitect, an expert requirements analyst and technical reviewer. Your job is to analyze a technical document for issues that would hinder implementation, cause ambiguity, or introduce risk.

For each issue you find, output a JSON object in the "issues" array with:
- "category": one of: ambiguity, contradiction, missing_acceptance_criteria, undefined_term, hidden_assumption, missing_edge_case, missing_operational_constraint, unclear_ownership, vague_success_metric, missing_failure_behavior, misleading_wording, incomplete_code_example, unspecified_input_output
- "anchor": the anchor ID of the most relevant node (must exist in the document)
- "evidence": a direct quote or reference from the document
- "why_it_matters": 1-2 sentences explaining the impact
- "suggested_resolution": a concrete, actionable suggestion for how to fix or clarify the issue

Be thorough but precise. Focus on substantive issues, not style preferences.
Treat the supplied document text as the current source of truth for this turn even if earlier turns referenced older versions."#;

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
- Treat the provided packs as the current document state for this turn even if earlier turns referenced older text

You MUST respond with a JSON object containing a "patches" array. Each patch must have:
- "op": one of "replace_section", "replace_text_span", "replace_code_block", "insert_after", "insert_before", "delete_block", "update_heading_text", "update_list_item"
- "anchor": one of the provided target anchors
- "rationale": brief explanation of the change

For replace/insert ops, include "content" with the new markdown text.
For replace_code_block, include "content" (code only, no fences) and optionally "lang".
For update_heading_text / update_list_item, include "new_text"."#;

const CREATION_SYSTEM_PROMPT: &str = r#"You are a technical document creator. Create a well-structured, comprehensive markdown document based on the user's description.

Guidelines:
- Use proper heading hierarchy (# / ## / ###)
- Include concrete sections, acceptance criteria, and examples where relevant
- Use fenced code blocks with language tags for any code
- Be precise and specific — avoid vague language
- Structure the document so each section can be individually reviewed and iterated

Return ONLY the raw markdown content. No preamble, no explanation, no JSON wrapping — just the document."#;

pub fn build_revision_request(
    config: &Config,
    doc: &Document,
    remarks: &[&Remark],
    previous_response_id: Option<String>,
) -> ResponseRequest {
    if let Some(plan) = build_targeted_revision_plan(doc, remarks) {
        return build_targeted_revision_request(config, &plan, previous_response_id);
    }

    build_full_revision_request(config, doc, remarks, previous_response_id)
}

fn build_full_revision_request(
    config: &Config,
    doc: &Document,
    remarks: &[&Remark],
    previous_response_id: Option<String>,
) -> ResponseRequest {
    let system = config
        .system_prompt_override
        .as_deref()
        .unwrap_or(REVISION_SYSTEM_PROMPT)
        .to_string();

    let anchor_map = doc.anchor_map_display();
    let mut user_msg = format!(
        "## Current Document Generation\n\n{}\n\n## Document Anchor Map\n\n{}\n\n## Full Document\n\n```markdown\n{}\n```\n\n## User Remarks\n\n",
        doc.generation,
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
                 Return one `update_list_item` patch per changed item, and `delete_block` for removed items.\n\n",
                remark.anchor,
                remark.selected_text,
                list_ctx,
                remark.text
            ));
        } else {
            user_msg.push_str(&format!(
                "### Remark on `{}`\n**Target text:** {}\n**Instruction:** {}\n\n",
                remark.anchor, remark.selected_text, remark.text
            ));
        }

        if !remark.occurrence_anchors.is_empty() {
            user_msg.push_str(
                "**Also apply the same change consistently to all of these occurrences \
                 of the same pattern — produce one patch per location:**\n",
            );
            for (anchor, snippet) in &remark.occurrence_anchors {
                user_msg.push_str(&format!("- `{}`: {}\n", anchor, snippet));
            }
            user_msg.push('\n');
        }
    }

    user_msg.push_str(
        "\nPlease provide patches to address all remarks. Only emit the structured JSON response.",
    );

    ResponseRequest {
        model: config.model_fix.clone(),
        input: vec![message("system", system), message("user", user_msg)],
        previous_response_id,
        temperature: config.temperature,
        max_output_tokens: config.max_tokens,
        store: true,
        text: Some(json_schema_format(
            "document_patch_response",
            patch_schema(),
        )),
    }
}

fn build_targeted_revision_request(
    config: &Config,
    plan: &TargetedRevisionPlan,
    previous_response_id: Option<String>,
) -> ResponseRequest {
    let system = config
        .system_prompt_override
        .as_deref()
        .unwrap_or(TARGETED_REVISION_SYSTEM_PROMPT)
        .to_string();

    let mut relevant_anchors = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for pack in &plan.packs {
        if seen.insert(pack.primary_target.anchor.clone()) {
            relevant_anchors.push(format!("{}: primary target", pack.primary_target.anchor));
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
                    node.anchor, node.summary, node.raw_markdown
                ));
            }
            user_msg.push('\n');
        }

        if !pack.occurrence_targets.is_empty() {
            user_msg.push_str("Explicit related occurrence targets:\n");
            for target in &pack.occurrence_targets {
                user_msg.push_str(&format!(
                    "- `{}` ({})\n  Selected text: {}\n```markdown\n{}\n```\n",
                    target.anchor, target.role, target.selected_text, target.raw_markdown
                ));
            }
            user_msg.push('\n');
        }
    }

    user_msg.push_str(
        "Please provide patches to address all instructions using only the provided anchors and context. Only emit the structured JSON response.",
    );

    ResponseRequest {
        model: config.model_fix.clone(),
        input: vec![message("system", system), message("user", user_msg)],
        previous_response_id,
        temperature: config.temperature,
        max_output_tokens: config.max_tokens,
        store: true,
        text: Some(json_schema_format(
            "document_patch_response",
            patch_schema(),
        )),
    }
}

pub fn build_ambiguity_request(
    config: &Config,
    doc: &Document,
    previous_response_id: Option<String>,
) -> ResponseRequest {
    let anchor_map = doc.anchor_map_display();
    let user_msg = format!(
        "## Current Document Generation\n\n{}\n\n## Document Anchor Map\n\n{}\n\n## Full Document\n\n```markdown\n{}\n```\n\nPlease review this document and identify all substantive implementation issues. Only emit the structured JSON response.",
        doc.generation,
        anchor_map,
        doc.raw
    );

    ResponseRequest {
        model: config.model.clone(),
        input: vec![
            message("system", REVIEW_SYSTEM_PROMPT.to_string()),
            message("user", user_msg),
        ],
        previous_response_id,
        temperature: Some(0.2),
        max_output_tokens: config.max_tokens,
        store: true,
        text: Some(json_schema_format(
            "document_issue_response",
            review_schema(),
        )),
    }
}

pub fn build_creation_request(
    config: &Config,
    prompt: &str,
    previous_response_id: Option<String>,
) -> ResponseRequest {
    ResponseRequest {
        model: config.model_fix.clone(),
        input: vec![
            message("system", CREATION_SYSTEM_PROMPT.to_string()),
            message("user", prompt.to_string()),
        ],
        previous_response_id,
        temperature: config.temperature,
        max_output_tokens: config.max_tokens,
        store: true,
        text: None,
    }
}

pub fn parse_revision_response(raw: &str) -> Result<Vec<PatchOp>> {
    let envelope: PatchEnvelope =
        serde_json::from_str(raw).context("Failed to parse patch response JSON")?;
    envelope
        .patches
        .into_iter()
        .map(TryInto::try_into)
        .collect()
}

pub fn parse_review_response(raw: &str) -> Result<Vec<ReviewItem>> {
    let envelope: ReviewEnvelope =
        serde_json::from_str(raw).context("Failed to parse review response JSON")?;
    Ok(envelope
        .issues
        .into_iter()
        .map(|issue| ReviewItem {
            id: Uuid::new_v4(),
            category: issue.category,
            anchor: issue.anchor,
            evidence: issue.evidence,
            why_it_matters: issue.why_it_matters,
            suggested_resolution: issue.suggested_resolution,
            status: ReviewStatus::New,
            user_answer: None,
        })
        .collect())
}

fn message(role: impl Into<String>, content: impl Into<String>) -> ResponseInputMessage {
    ResponseInputMessage {
        role: role.into(),
        content: content.into(),
    }
}

fn json_schema_format(name: &str, schema: serde_json::Value) -> ResponseTextConfig {
    ResponseTextConfig {
        format: json!({
            "type": "json_schema",
            "name": name,
            "schema": schema,
            "strict": true
        }),
    }
}

fn patch_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "patches": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "op": {
                            "type": "string",
                            "enum": [
                                "replace_section",
                                "replace_text_span",
                                "replace_code_block",
                                "insert_after",
                                "insert_before",
                                "delete_block",
                                "update_heading_text",
                                "update_list_item"
                            ]
                        },
                        "anchor": { "type": "string" },
                        "rationale": { "type": "string" },
                        "content": { "type": "string" },
                        "lang": { "type": "string" },
                        "new_text": { "type": "string" }
                    },
                    "required": ["op", "anchor", "rationale"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["patches"],
        "additionalProperties": false
    })
}

fn review_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "issues": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "category": {
                            "type": "string",
                            "enum": [
                                "ambiguity",
                                "contradiction",
                                "missing_acceptance_criteria",
                                "undefined_term",
                                "hidden_assumption",
                                "missing_edge_case",
                                "missing_operational_constraint",
                                "unclear_ownership",
                                "vague_success_metric",
                                "missing_failure_behavior",
                                "misleading_wording",
                                "incomplete_code_example",
                                "unspecified_input_output"
                            ]
                        },
                        "anchor": { "type": "string" },
                        "evidence": { "type": "string" },
                        "why_it_matters": { "type": "string" },
                        "suggested_resolution": { "type": "string" }
                    },
                    "required": [
                        "category",
                        "anchor",
                        "evidence",
                        "why_it_matters",
                        "suggested_resolution"
                    ],
                    "additionalProperties": false
                }
            }
        },
        "required": ["issues"],
        "additionalProperties": false
    })
}

#[derive(Debug, Deserialize)]
struct PatchEnvelope {
    patches: Vec<PatchDraft>,
}

#[derive(Debug, Deserialize)]
struct PatchDraft {
    op: String,
    anchor: String,
    rationale: String,
    content: Option<String>,
    lang: Option<String>,
    new_text: Option<String>,
}

impl TryFrom<PatchDraft> for PatchOp {
    type Error = anyhow::Error;

    fn try_from(value: PatchDraft) -> Result<Self, Self::Error> {
        let PatchDraft {
            op,
            anchor,
            rationale,
            content,
            lang,
            new_text,
        } = value;

        match op.as_str() {
            "replace_section" => Ok(PatchOp::ReplaceSection {
                anchor,
                content: required_field("content", content)?,
                rationale,
            }),
            "replace_text_span" => Ok(PatchOp::ReplaceTextSpan {
                anchor,
                content: required_field("content", content)?,
                rationale,
            }),
            "replace_code_block" => Ok(PatchOp::ReplaceCodeBlock {
                anchor,
                content: required_field("content", content)?,
                lang,
                rationale,
            }),
            "insert_after" => Ok(PatchOp::InsertAfter {
                anchor,
                content: required_field("content", content)?,
                rationale,
            }),
            "insert_before" => Ok(PatchOp::InsertBefore {
                anchor,
                content: required_field("content", content)?,
                rationale,
            }),
            "delete_block" => Ok(PatchOp::DeleteBlock { anchor, rationale }),
            "update_heading_text" => Ok(PatchOp::UpdateHeadingText {
                anchor,
                new_text: required_field("new_text", new_text)?,
                rationale,
            }),
            "update_list_item" => Ok(PatchOp::UpdateListItem {
                anchor,
                new_text: required_field("new_text", new_text)?,
                rationale,
            }),
            other => anyhow::bail!("Unsupported patch op: {}", other),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReviewEnvelope {
    issues: Vec<ReviewDraft>,
}

#[derive(Debug, Deserialize)]
struct ReviewDraft {
    category: crate::review::ReviewCategory,
    anchor: String,
    evidence: String,
    why_it_matters: String,
    suggested_resolution: String,
}

fn required_field(name: &str, value: Option<String>) -> Result<String> {
    value.ok_or_else(|| anyhow::anyhow!("Patch field `{}` is required for this op", name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_patch_response_into_patch_ops() {
        let raw = r#"{
          "patches": [
            {
              "op": "replace_section",
              "anchor": "p-0",
              "content": "Updated.\n",
              "rationale": "clarify"
            }
          ]
        }"#;

        let patches = parse_revision_response(raw).unwrap();
        assert_eq!(patches.len(), 1);
        match &patches[0] {
            PatchOp::ReplaceSection {
                anchor,
                content,
                rationale,
            } => {
                assert_eq!(anchor, "p-0");
                assert_eq!(content, "Updated.\n");
                assert_eq!(rationale, "clarify");
            }
            other => panic!("unexpected patch variant: {:?}", other),
        }
    }

    #[test]
    fn parses_review_response_into_review_items() {
        let raw = r#"{
          "issues": [
            {
              "category": "ambiguity",
              "anchor": "p-1",
              "evidence": "The system should be fast.",
              "why_it_matters": "Implementers do not know what fast means.",
              "suggested_resolution": "Add measurable latency targets."
            }
          ]
        }"#;

        let issues = parse_review_response(raw).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].anchor, "p-1");
        assert_eq!(
            issues[0].suggested_resolution,
            "Add measurable latency targets."
        );
        assert_eq!(issues[0].status, ReviewStatus::New);
    }
}
