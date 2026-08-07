//! SSE reassembler — Anthropic / Responses / Chat Completions. Author: kejiqing

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct SseEvent {
    pub event: String,
    pub data: Value,
}

#[derive(Debug, Default)]
pub struct SseReassembler {
    pub events: Vec<SseEvent>,
    buf: Vec<u8>,
    current_event: Option<String>,
    current_data_lines: Vec<String>,
    snapshot: Option<Value>,
}

impl SseReassembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed_bytes(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len().saturating_sub(1)]);
            self.feed_line(line.trim_end_matches('\r'));
        }
    }

    fn feed_line(&mut self, line: &str) {
        if let Some(rest) = line.strip_prefix("event:") {
            self.current_event = Some(rest.trim().to_string());
            self.current_data_lines.clear();
        } else if let Some(rest) = line.strip_prefix("data:") {
            self.current_data_lines.push(rest.trim().to_string());
        } else if line.is_empty() {
            if self.current_event.is_some() || !self.current_data_lines.is_empty() {
                let raw_data = self.current_data_lines.join("\n");
                if raw_data == "[DONE]" && self.current_event.is_none() {
                    self.current_event = None;
                    self.current_data_lines.clear();
                    return;
                }
                let data = serde_json::from_str::<Value>(&raw_data).unwrap_or(Value::String(raw_data));
                let event_type = self
                    .current_event
                    .clone()
                    .unwrap_or_else(|| "message".to_string());
                self.add_event(&event_type, data);
                self.current_event = None;
                self.current_data_lines.clear();
            }
        }
    }

    pub fn add_event(&mut self, event_type: &str, data: Value) {
        self.events.push(SseEvent {
            event: event_type.to_string(),
            data: data.clone(),
        });
        self.accumulate(event_type, &data);
    }

    fn accumulate(&mut self, event_type: &str, data: &Value) {
        let Value::Object(map) = data else {
            return;
        };
        let result = (|| -> Option<()> {
            if event_type == "message_start" {
                self.snapshot = map.get("message").cloned();
            } else if matches!(
                event_type,
                "response.created" | "response.completed" | "response.done"
            ) {
                if let Some(response) = map.get("response").filter(|v| v.is_object()) {
                    self.snapshot = Some(response.clone());
                } else if matches!(event_type, "response.completed" | "response.done") {
                    self.snapshot = Some(data.clone());
                }
            } else if event_type == "message" && map.contains_key("choices") {
                self.accumulate_chat_completion_chunk(data);
            } else if self.snapshot.is_none() {
                return None;
            } else if event_type == "content_block_start" {
                let block = map.get("content_block").cloned().unwrap_or(json!({}));
                let snap = self.snapshot.as_mut()?;
                let content = snap
                    .as_object_mut()?
                    .entry("content")
                    .or_insert_with(|| json!([]));
                let arr = content.as_array_mut()?;
                let idx = map
                    .get("index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(arr.len() as u64) as usize;
                while arr.len() <= idx {
                    arr.push(json!({}));
                }
                arr[idx] = block;
            } else if event_type == "content_block_delta" {
                let idx = map.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let delta = map.get("delta").cloned().unwrap_or(json!({}));
                let snap = self.snapshot.as_mut()?;
                let arr = snap.get_mut("content")?.as_array_mut()?;
                if idx < arr.len() {
                    let block = arr[idx].as_object_mut()?;
                    let dtype = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if dtype == "text_delta" {
                        let t = delta.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        let cur = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        block.insert("text".into(), json!(format!("{cur}{t}")));
                    } else if dtype == "thinking_delta" {
                        let t = delta.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                        let cur = block.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                        block.insert("thinking".into(), json!(format!("{cur}{t}")));
                    } else if dtype == "input_json_delta" {
                        let t = delta
                            .get("partial_json")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let cur = block
                            .get("_partial_json")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        block.insert("_partial_json".into(), json!(format!("{cur}{t}")));
                    }
                }
            } else if event_type == "content_block_stop" {
                let idx = map.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let snap = self.snapshot.as_mut()?;
                let arr = snap.get_mut("content")?.as_array_mut()?;
                if idx < arr.len() {
                    let block = arr[idx].as_object_mut()?;
                    if let Some(partial) = block.remove("_partial_json") {
                        if let Some(s) = partial.as_str() {
                            if let Ok(v) = serde_json::from_str::<Value>(s) {
                                block.insert("input".into(), v);
                            }
                        }
                    }
                }
            } else if event_type == "message_delta" {
                let snap = self.snapshot.as_mut()?.as_object_mut()?;
                if let Some(Value::Object(delta)) = map.get("delta") {
                    for (k, v) in delta {
                        snap.insert(k.clone(), v.clone());
                    }
                }
                if let Some(Value::Object(usage)) = map.get("usage") {
                    let u = snap.entry("usage").or_insert_with(|| json!({}));
                    if let Some(uo) = u.as_object_mut() {
                        for (k, v) in usage {
                            uo.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            Some(())
        })();
        let _ = result;
    }

    fn accumulate_chat_completion_chunk(&mut self, data: &Value) {
        let map = match data.as_object() {
            Some(m) => m,
            None => return,
        };
        let choices = map.get("choices").and_then(|v| v.as_array());
        let usage = map.get("usage").cloned();

        let empty_choices = choices.map(|c| c.is_empty()).unwrap_or(true);
        if empty_choices {
            if let Some(Value::Object(u)) = usage {
                if self.snapshot.is_some() {
                    self.merge_chat_completion_usage(&Value::Object(u));
                }
            }
            return;
        }
        let choice = choices
            .and_then(|c| c.first())
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let delta = choice
            .get("delta")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let finish_reason = choice.get("finish_reason").cloned();

        if self.snapshot.is_none() {
            let role = delta
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("assistant");
            self.snapshot = Some(json!({
                "id": map.get("id").cloned().unwrap_or(json!("")),
                "object": "chat.completion",
                "model": map.get("model").cloned().unwrap_or(json!("")),
                "choices": [{
                    "index": 0,
                    "message": {"role": role, "content": ""},
                    "finish_reason": null
                }],
                "content": [{"type": "text", "text": ""}]
            }));
        }

        // Apply message/content updates without overlapping borrows.
        if let Some(role) = delta.get("role").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            if let Some(msg) = self
                .snapshot
                .as_mut()
                .and_then(|s| s.pointer_mut("/choices/0/message"))
                .and_then(|v| v.as_object_mut())
            {
                msg.insert("role".into(), json!(role));
            }
        }
        if let Some(content) = delta
            .get("content")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            if let Some(msg) = self
                .snapshot
                .as_mut()
                .and_then(|s| s.pointer_mut("/choices/0/message"))
                .and_then(|v| v.as_object_mut())
            {
                let cur = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
                msg.insert("content".into(), json!(format!("{cur}{content}")));
            }
            if let Some(text_block) = self
                .snapshot
                .as_mut()
                .and_then(|s| s.pointer_mut("/content/0"))
                .and_then(|v| v.as_object_mut())
            {
                let cur = text_block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                text_block.insert("text".into(), json!(format!("{cur}{content}")));
            }
        }

        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc_delta in tool_calls {
                let Some(tc_obj) = tc_delta.as_object() else {
                    continue;
                };
                let idx = tc_obj.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let existing_clone = {
                    let snap = match self.snapshot.as_mut() {
                        Some(s) => s,
                        None => continue,
                    };
                    let msg = match snap
                        .pointer_mut("/choices/0/message")
                        .and_then(|v| v.as_object_mut())
                    {
                        Some(m) => m,
                        None => continue,
                    };
                    let tool_calls_arr = msg.entry("tool_calls").or_insert_with(|| json!([]));
                    let arr = match tool_calls_arr.as_array_mut() {
                        Some(a) => a,
                        None => continue,
                    };
                    while arr.len() <= idx {
                        arr.push(json!({
                            "id": "",
                            "type": "function",
                            "function": {"name": "", "arguments": ""}
                        }));
                    }
                    let existing = arr[idx].as_object_mut().unwrap();
                    if let Some(id) = tc_obj.get("id").and_then(|v| v.as_str()) {
                        existing.insert("id".into(), json!(id));
                    }
                    if let Some(ty) = tc_obj.get("type").and_then(|v| v.as_str()) {
                        existing.insert("type".into(), json!(ty));
                    }
                    if let Some(fn_delta) = tc_obj.get("function").and_then(|v| v.as_object()) {
                        let fn_obj = existing
                            .entry("function")
                            .or_insert_with(|| json!({"name":"", "arguments":""}));
                        if let Some(fo) = fn_obj.as_object_mut() {
                            if let Some(name) = fn_delta.get("name").and_then(|v| v.as_str()) {
                                let cur = fo.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                fo.insert("name".into(), json!(format!("{cur}{name}")));
                            }
                            if let Some(args) = fn_delta.get("arguments").and_then(|v| v.as_str()) {
                                let cur = fo.get("arguments").and_then(|v| v.as_str()).unwrap_or("");
                                fo.insert("arguments".into(), json!(format!("{cur}{args}")));
                            }
                        }
                    }
                    arr[idx].clone()
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    Self::mirror_tool_call_to_content(snap, idx, &existing_clone);
                }
            }
        }

        if let Some(fr) = finish_reason {
            if !fr.is_null() {
                if let Some(choice0) = self
                    .snapshot
                    .as_mut()
                    .and_then(|s| s.pointer_mut("/choices/0"))
                    .and_then(|v| v.as_object_mut())
                {
                    choice0.insert("finish_reason".into(), fr);
                }
            }
        }
        if let Some(Value::Object(u)) = usage {
            self.merge_chat_completion_usage(&Value::Object(u));
        }
    }

    fn mirror_tool_call_to_content(snap: &mut Value, idx: usize, tc: &Value) {
        let content = match snap.get_mut("content").and_then(|v| v.as_array_mut()) {
            Some(c) => c,
            None => return,
        };
        let target = idx + 1;
        while content.len() <= target {
            content.push(json!({"type":"tool_use","id":"","name":"","input":{}}));
        }
        let block = content[target].as_object_mut().unwrap();
        if let Some(id) = tc.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            block.insert("id".into(), json!(id));
        }
        let fn_obj = tc.get("function").cloned().unwrap_or(json!({}));
        if let Some(name) = fn_obj.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            block.insert("name".into(), json!(name));
        }
        if let Some(args_str) = fn_obj.get("arguments").and_then(|v| v.as_str()) {
            if !args_str.is_empty() {
                if let Ok(v) = serde_json::from_str::<Value>(args_str) {
                    block.insert("input".into(), v);
                }
            }
        }
    }

    fn merge_chat_completion_usage(&mut self, usage: &Value) {
        let Some(snap) = self.snapshot.as_mut() else {
            return;
        };
        let Some(u) = usage.as_object() else {
            return;
        };
        let mut merged = u.clone();
        if u.contains_key("prompt_tokens") && !u.contains_key("input_tokens") {
            merged.insert("input_tokens".into(), u["prompt_tokens"].clone());
        }
        if u.contains_key("completion_tokens") && !u.contains_key("output_tokens") {
            merged.insert("output_tokens".into(), u["completion_tokens"].clone());
        }
        if let Some(obj) = snap.as_object_mut() {
            obj.insert("usage".into(), Value::Object(merged));
        }
    }

    pub fn reconstruct(&self) -> Option<Value> {
        self.snapshot.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_completions_stream_events_are_captured() {
        let mut r = SseReassembler::new();
        r.feed_bytes(
            b"data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
        );
        r.feed_bytes(b"data: [DONE]\n\n");
        assert_eq!(r.events.len(), 1);
        let snap = r.reconstruct().unwrap();
        assert_eq!(snap["choices"][0]["message"]["content"], "hi");
        assert_eq!(snap["content"][0]["text"], "hi");
    }

    #[test]
    fn chat_completions_usage_dual_naming() {
        let mut r = SseReassembler::new();
        r.feed_bytes(
            b"data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
        );
        r.feed_bytes(
            b"data: {\"id\":\"1\",\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\n",
        );
        let snap = r.reconstruct().unwrap();
        assert_eq!(snap["usage"]["input_tokens"], 3);
        assert_eq!(snap["usage"]["output_tokens"], 1);
        assert_eq!(snap["usage"]["prompt_tokens"], 3);
    }

    #[test]
    fn chat_completions_done_sentinel_filtered() {
        let mut r = SseReassembler::new();
        r.feed_bytes(b"data: [DONE]\n\n");
        assert!(r.events.is_empty());
    }

    #[test]
    fn anthropic_stream_message_start() {
        let mut r = SseReassembler::new();
        r.feed_bytes(
            b"event: message_start\ndata: {\"message\":{\"id\":\"m1\",\"content\":[],\"role\":\"assistant\"}}\n\n",
        );
        r.feed_bytes(
            b"event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        );
        r.feed_bytes(
            b"event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
        );
        let snap = r.reconstruct().unwrap();
        assert_eq!(snap["content"][0]["text"], "Hello");
    }

    #[test]
    fn tool_call_accumulation_and_mirror() {
        let mut r = SseReassembler::new();
        r.feed_bytes(
            b"data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"type\":\"function\",\"function\":{\"name\":\"fn\",\"arguments\":\"{\\\"a\\\":\"}}]}}]}\n\n",
        );
        r.feed_bytes(
            b"data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"1}\"}}]}}]}\n\n",
        );
        let snap = r.reconstruct().unwrap();
        assert_eq!(snap["choices"][0]["message"]["tool_calls"][0]["function"]["name"], "fn");
        assert_eq!(snap["content"][1]["type"], "tool_use");
        assert_eq!(snap["content"][1]["input"]["a"], 1);
    }

    #[test]
    fn chunked_across_feeds() {
        let mut r = SseReassembler::new();
        r.feed_bytes(b"data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"hel");
        r.feed_bytes(b"lo\"}}]}\n\n");
        let snap = r.reconstruct().unwrap();
        assert_eq!(snap["choices"][0]["message"]["content"], "hello");
    }

    #[test]
    fn responses_created() {
        let mut r = SseReassembler::new();
        r.feed_bytes(
            b"event: response.created\ndata: {\"response\":{\"id\":\"r1\",\"output\":[]}}\n\n",
        );
        let snap = r.reconstruct().unwrap();
        assert_eq!(snap["id"], "r1");
    }
}
