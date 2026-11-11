//! End-to-end check of the embedded llama.cpp against a real GGUF:
//! a tool call, the tool result, the final answer, and KV-cache reuse.
//!
//!     cargo run --release -p rocinante-llama --example smoke -- /path/to/model.gguf

use std::time::Instant;

use futures::StreamExt;
use rocinante_llama::{LlamaProvider, LlamaSettings};
use rocinante_providers::{ChatDelta, ChatRequest, GenParams, Message, Provider, ToolSchema};
use serde_json::json;

async fn turn(
    p: &LlamaProvider,
    model: &str,
    messages: &[Message],
    tools: &[ToolSchema],
) -> Message {
    let started = Instant::now();
    let req = ChatRequest {
        model: model.to_string(),
        messages: messages.to_vec(),
        tools: tools.to_vec(),
        params: GenParams {
            num_ctx: Some(8192),
            max_tokens: Some(512),
            temperature: Some(0.2),
            ..Default::default()
        },
        format: None,
    };
    let mut stream = p.chat(req).await.expect("chat");
    let mut reply = Message::assistant("");
    while let Some(delta) = stream.next().await {
        match delta.expect("delta") {
            ChatDelta::Text(t) => {
                print!("{t}");
                reply.content.push_str(&t);
            }
            ChatDelta::Thinking(t) => print!("\x1b[90m{t}\x1b[0m"),
            ChatDelta::ToolCall(c) => {
                println!("\n[tool call] {} {}", c.name, c.arguments);
                reply.tool_calls.push(c);
            }
            ChatDelta::Usage(u) => println!(
                "\n[usage] prompt {} completion {} ({:.1}s)",
                u.prompt_tokens,
                u.completion_tokens,
                started.elapsed().as_secs_f32()
            ),
            ChatDelta::Done(r) => println!("[done] {r:?}"),
            ChatDelta::ToolCallPartial { .. } | ChatDelta::Progress { .. } => {}
        }
    }
    if let Some(info) = rocinante_llama::loaded() {
        println!("[loaded] {} · n_ctx {}", info.desc, info.n_ctx);
    }
    if let Some(t) = rocinante_llama::last_turn() {
        println!(
            "[cache] prompt {} · reused {} · processed {}",
            t.prompt,
            t.cached,
            t.prompt - t.cached
        );
    }
    reply
}

#[tokio::main]
async fn main() {
    let model = std::env::args().nth(1).expect("usage: smoke <model.gguf>");
    println!("backend: {}", rocinante_llama::backend_summary());
    let p = LlamaProvider::new("local", LlamaSettings::default());
    let tools = vec![ToolSchema {
        name: "read".into(),
        description: "Read a file from the project and return its contents.".into(),
        parameters: json!({
            "type": "object",
            "properties": {"path": {"type": "string", "description": "file path"}},
            "required": ["path"]
        }),
    }];
    let mut messages = vec![
        Message::system("You are a coding agent. Use the tools to inspect files; never guess."),
        Message::user("What is the package name in Cargo.toml? Read the file first."),
    ];
    println!("=== turn 1: expect a read tool call");
    let reply = turn(&p, &model, &messages, &tools).await;
    let call = reply.tool_calls.first().cloned();
    messages.push(reply);
    let Some(call) = call else {
        println!("!! no tool call");
        std::process::exit(1);
    };
    messages.push(Message::tool_result(
        call.id,
        "[package]\nname = \"rocinante-demo\"\nversion = \"0.3.0\"\n",
    ));
    println!("=== turn 2: expect an answer naming rocinante-demo (prompt prefix reused)");
    let reply = turn(&p, &model, &messages, &tools).await;
    if !reply.content.contains("rocinante-demo") {
        println!("!! answer missing the package name");
        std::process::exit(1);
    }
    messages.push(reply);
    messages.push(Message::user("And its version? One short sentence."));
    println!("=== turn 3: a follow-up reuses the whole conversation");
    let reply = turn(&p, &model, &messages, &tools).await;
    let stats = rocinante_llama::last_turn().unwrap_or_default();
    if !reply.content.contains("0.3.0") {
        println!("!! answer missing the version");
        std::process::exit(1);
    }
    if stats.cached == 0 {
        println!("!! nothing was reused from the cache");
        std::process::exit(1);
    }
    println!("=== ok");
}
