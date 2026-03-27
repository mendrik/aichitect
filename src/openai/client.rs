use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use crate::config::Config;

#[allow(dead_code)]
pub enum StreamEvent {
    Token(String),
    Done,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub r#type: String,
}

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
        self.config.base_url.clone().unwrap_or_else(|| "https://api.openai.com/v1".to_string())
    }

    pub async fn chat(&self, req: ChatRequest) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url());
        let resp = self.http.post(&url)
            .json(&req)
            .send()
            .await
            .context("Failed to send request to OpenAI")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenAI API error {}: {}", status, body);
        }

        let json: serde_json::Value = resp.json().await.context("Failed to parse OpenAI response")?;
        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("No content in response: {}", json))?
            .to_string();
        Ok(content)
    }

    #[allow(dead_code)]
    pub async fn chat_stream(&self, req: ChatRequest, tx: mpsc::Sender<StreamEvent>) -> Result<()> {
        use futures::StreamExt;

        let url = format!("{}/chat/completions", self.base_url());
        let resp = self.http.post(&url)
            .json(&req)
            .send()
            .await
            .context("Failed to send streaming request to OpenAI")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let _ = tx.send(StreamEvent::Error(format!("OpenAI API error {}: {}", status, body))).await;
            return Ok(());
        }

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(newline_pos) = buffer.find('\n') {
                        let line: String = buffer.drain(..=newline_pos).collect();
                        let line = line.trim();

                        if line.starts_with("data: ") {
                            let data = &line["data: ".len()..];
                            if data == "[DONE]" {
                                let _ = tx.send(StreamEvent::Done).await;
                                return Ok(());
                            }
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                                if let Some(content) = json["choices"][0]["delta"]["content"].as_str() {
                                    if !content.is_empty() {
                                        let _ = tx.send(StreamEvent::Token(content.to_string())).await;
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(StreamEvent::Error(e.to_string())).await;
                    return Ok(());
                }
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }
}
