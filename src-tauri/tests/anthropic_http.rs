//! Wire-level HTTP tests for `LiveAnthropicClient` using wiremock.
//! Validates the request shape (headers, body) and response parsing
//! (text + token usage + cache attribution) against a recorded API
//! fixture. These tests do NOT call the live Anthropic API.

use agentscout::anthropic::{
    AnthropicClient, CompletionRequest, LiveAnthropicClient, Message, Role,
};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn req<'a>(messages: &'a [Message], cache_breakpoint: Option<usize>) -> CompletionRequest<'a> {
    CompletionRequest {
        messages,
        system: Some("You are a test."),
        model: "claude-sonnet-4-6",
        max_tokens: 256,
        cache_breakpoint,
    }
}

#[tokio::test]
async fn happy_path_parses_text_and_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(header("anthropic-beta", "prompt-caching-2024-07-31"))
        .and(header("x-api-key", "test-key"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_abc",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-6",
            "content": [{ "type": "text", "text": "hello back" }],
            "usage": {
                "input_tokens": 42,
                "output_tokens": 7,
                "cache_read_input_tokens": 31,
                "cache_creation_input_tokens": 0
            }
        })))
        .mount(&server)
        .await;

    let client = LiveAnthropicClient::with_base_url("test-key".into(), server.uri());
    let messages = vec![Message {
        role: Role::User,
        content: "hi".into(),
    }];
    let resp = client.complete(req(&messages, None)).await.unwrap();
    assert_eq!(resp.text, "hello back");
    assert_eq!(resp.usage.input_tokens, 42);
    assert_eq!(resp.usage.output_tokens, 7);
    assert_eq!(resp.usage.cache_read_tokens, 31);
    assert_eq!(resp.usage.cache_creation_tokens, 0);
    assert_eq!(resp.model, "claude-sonnet-4-6");
}

#[tokio::test]
async fn cache_breakpoint_attaches_cache_control_to_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "m",
            "content": [{ "type": "text", "text": "ok" }],
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        })))
        .mount(&server)
        .await;

    let client = LiveAnthropicClient::with_base_url("test-key".into(), server.uri());
    let messages = vec![
        Message {
            role: Role::User,
            content: "static prefix".into(),
        },
        Message {
            role: Role::Assistant,
            content: "ok".into(),
        },
        Message {
            role: Role::User,
            content: "dynamic".into(),
        },
    ];
    client.complete(req(&messages, Some(0))).await.unwrap();

    // Verify the request body shape included cache_control on message 0
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).expect("body parses");
    let first_msg_content = &body["messages"][0]["content"];
    assert!(
        first_msg_content.is_array(),
        "cached message uses content blocks"
    );
    assert_eq!(first_msg_content[0]["type"], "text");
    assert_eq!(first_msg_content[0]["text"], "static prefix");
    assert_eq!(first_msg_content[0]["cache_control"]["type"], "ephemeral");
    // Non-cached messages stay as plain strings
    assert_eq!(body["messages"][2]["content"], "dynamic");
}

#[tokio::test]
async fn system_prompt_included_when_provided() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{ "type": "text", "text": "" }],
            "usage": { "input_tokens": 0, "output_tokens": 0 }
        })))
        .mount(&server)
        .await;
    let client = LiveAnthropicClient::with_base_url("k".into(), server.uri());
    let messages = vec![Message {
        role: Role::User,
        content: "hi".into(),
    }];
    client.complete(req(&messages, None)).await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(body["system"], "You are a test.");
}

#[tokio::test]
async fn http_4xx_surfaces_status_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": { "type": "invalid_request_error", "message": "bad model" }
        })))
        .mount(&server)
        .await;
    let client = LiveAnthropicClient::with_base_url("k".into(), server.uri());
    let messages = vec![Message {
        role: Role::User,
        content: "hi".into(),
    }];
    let err = client.complete(req(&messages, None)).await.unwrap_err();
    let s = format!("{:#}", err);
    assert!(s.contains("400"));
    assert!(s.contains("bad model"));
}

#[tokio::test]
async fn http_5xx_surfaces_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream lit on fire"))
        .mount(&server)
        .await;
    let client = LiveAnthropicClient::with_base_url("k".into(), server.uri());
    let messages = vec![Message {
        role: Role::User,
        content: "hi".into(),
    }];
    let err = client.complete(req(&messages, None)).await.unwrap_err();
    let s = format!("{:#}", err);
    assert!(s.contains("500"));
}

#[tokio::test]
async fn missing_usage_block_defaults_to_zeros() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{ "type": "text", "text": "x" }]
            // no `usage` field
        })))
        .mount(&server)
        .await;
    let client = LiveAnthropicClient::with_base_url("k".into(), server.uri());
    let messages = vec![Message {
        role: Role::User,
        content: "hi".into(),
    }];
    let resp = client.complete(req(&messages, None)).await.unwrap();
    assert_eq!(resp.usage.input_tokens, 0);
    assert_eq!(resp.usage.output_tokens, 0);
}

#[tokio::test]
async fn multiple_text_blocks_are_joined() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [
                { "type": "text", "text": "first" },
                { "type": "text", "text": "second" }
            ],
            "usage": { "input_tokens": 1, "output_tokens": 2 }
        })))
        .mount(&server)
        .await;
    let client = LiveAnthropicClient::with_base_url("k".into(), server.uri());
    let messages = vec![Message {
        role: Role::User,
        content: "hi".into(),
    }];
    let resp = client.complete(req(&messages, None)).await.unwrap();
    assert_eq!(resp.text, "first\nsecond");
}
