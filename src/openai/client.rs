use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

use crate::config::Config;

#[derive(Debug, Clone, Serialize)]
pub struct ResponseInputMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseRequest {
    pub model: String,
    pub input: Vec<ResponseInputMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "max_output_tokens")]
    pub max_output_tokens: Option<u32>,
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<ResponseTextConfig>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseTextConfig {
    pub format: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsePayload {
    pub id: String,
    pub text: String,
    pub usage: Option<TokenUsage>,
}

#[derive(Clone)]
pub struct OpenAiClient {
    pub config: Arc<Config>,
    pub http: reqwest::Client,
}

impl OpenAiClient {
    pub fn new(config: Arc<Config>) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", config.api_key)).unwrap(),
        );
        if let Some(org) = &config.organization {
            headers.insert(
                "OpenAI-Organization",
                reqwest::header::HeaderValue::from_str(org).unwrap(),
            );
        }
        if let Some(proj) = &config.project {
            headers.insert(
                "OpenAI-Project",
                reqwest::header::HeaderValue::from_str(proj).unwrap(),
            );
        }
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .expect("Failed to build HTTP client");
        OpenAiClient { config, http }
    }

    fn base_url(&self) -> String {
        self.config
            .base_url
            .clone()
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string())
    }

    pub async fn respond(&self, req: ResponseRequest) -> Result<ResponsePayload> {
        let url = format!("{}/responses", self.base_url());
        let resp = self
            .http
            .post(&url)
            .json(&req)
            .send()
            .await
            .context("Failed to send request to OpenAI")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenAI API error {}: {}", status, body);
        }

        let json: Value = resp
            .json()
            .await
            .context("Failed to parse OpenAI response")?;
        parse_response_payload(&json)
    }
}

fn parse_response_payload(json: &Value) -> Result<ResponsePayload> {
    let id = json["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No response id in payload: {}", json))?
        .to_string();

    let mut text = String::new();
    let mut refusal = None::<String>;

    if let Some(output) = json["output"].as_array() {
        for item in output {
            if item["type"].as_str() != Some("message") {
                continue;
            }
            if let Some(parts) = item["content"].as_array() {
                for part in parts {
                    match part["type"].as_str() {
                        Some("output_text") | Some("text") => {
                            if let Some(chunk) = part["text"].as_str() {
                                text.push_str(chunk);
                            }
                        }
                        Some("refusal") => {
                            if let Some(message) = part["refusal"].as_str() {
                                refusal = Some(message.to_string());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    if text.trim().is_empty() {
        if let Some(message) = refusal {
            anyhow::bail!("Model refused request: {}", message);
        }
        anyhow::bail!("No text content in response: {}", json);
    }

    let usage = json.get("usage").and_then(|u| {
        Some(TokenUsage {
            input_tokens: u["input_tokens"].as_u64()?,
            output_tokens: u["output_tokens"].as_u64()?,
        })
    });

    Ok(ResponsePayload { id, text, usage })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_output_text_from_response_payload() {
        let payload = json!({
            "id": "resp_123",
            "output": [
                {
                    "type": "message",
                    "content": [
                        {"type": "output_text", "text": "{\"patches\": []}"}
                    ]
                }
            ]
        });

        let parsed = parse_response_payload(&payload).unwrap();
        assert_eq!(parsed.id, "resp_123");
        assert_eq!(parsed.text, "{\"patches\": []}");
    }

    #[test]
    fn surfaces_refusals_when_no_text_is_returned() {
        let payload = json!({
            "id": "resp_123",
            "output": [
                {
                    "type": "message",
                    "content": [
                        {"type": "refusal", "refusal": "Nope"}
                    ]
                }
            ]
        });

        let err = parse_response_payload(&payload).unwrap_err();
        assert!(err.to_string().contains("Nope"));
    }
}
